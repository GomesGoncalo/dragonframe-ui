//! Syncing Google Photos shared albums straight onto the frame.
//!
//! Google's API no longer allows reading a user's library, so this uses the
//! same unauthenticated call the public shared-album web page makes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::USER_AGENT;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::blocking;
use crate::index::Index;
use crate::store::{self, Config};
use crate::types::{AlbumLink, SyncedName};

const RPC: &str = "snAcKc"; // lists one page of an album
/// Where album listings come from.
pub const RPC_URL: &str = "https://photos.google.com/_/PhotosUi/data/batchexecute";
const BROWSER_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
const DOWNLOADS_IN_PARALLEL: usize = 6;
/// Size suffixes Google's image server understands: a copy that fits the frame, and a small one.
const FRAME_SIZE: &str = "=w1920-h1920";
const THUMBNAIL_SIZE: &str = "=w480-h480";
const MAX_PHOTO_BYTES: u64 = 40 * 1024 * 1024;

/// Why a sync could not be carried out.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("could not open the album link ({0})")]
    OpenLink(#[source] reqwest::Error),
    #[error(
        "not a Google Photos shared-album link (expected photos.app.goo.gl/... or photos.google.com/share/...?key=...)"
    )]
    NotAnAlbum,
    #[error("could not read the album ({0})")]
    ReadAlbum(#[source] reqwest::Error),
    #[error("Google Photos returned an unexpected response; the album may be private or Google changed its format")]
    UnexpectedResponse,
    #[error("could not use the photo folder ({0})")]
    Folder(#[from] std::io::Error),
}

/// Why one photo could not be downloaded.
#[derive(Debug, thiserror::Error)]
enum DownloadError {
    #[error("{0}")]
    Http(#[from] reqwest::Error),
    #[error("the file is unexpectedly large")]
    TooLarge,
    #[error("Google did not return a JPEG")]
    NotJpeg,
    #[error("could not save it ({0})")]
    Io(#[from] std::io::Error),
}

/// Where the sync job is in its life. A job cannot be both running and finished.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    /// No sync has run since the server started.
    #[default]
    Idle,
    Running,
    /// How the last run ended; a failure carries its message.
    Finished(Result<(), String>),
}

/// What a running sync is doing right now.
struct Progress {
    done: usize,
    total: usize,
    detail: String,
    /// When this step began, for working out a rate.
    since: Instant,
}

#[derive(Default)]
pub struct Job {
    pub phase: Phase,
    progress: Option<Progress>,
    pub log: Vec<String>,
}

impl Job {
    pub fn is_running(&self) -> bool {
        self.phase == Phase::Running
    }

    /// The job as the page wants it.
    pub fn view(&self) -> Value {
        let progress = self.progress.as_ref().map(|p| {
            let downloading = p.done > 0 && p.detail.starts_with("downloading");
            let per_second =
                f64::from(u32::try_from(p.done).unwrap_or(u32::MAX)) / p.since.elapsed().as_secs_f64().max(1.0);
            let rate = if downloading { (per_second * 10.0).round() / 10.0 } else { 0.0 };
            json!({ "done": p.done, "total": p.total, "detail": p.detail, "rate": rate })
        });
        let result = match &self.phase {
            Phase::Finished(outcome) => json!({ "ok": outcome.is_ok(), "error": outcome.as_ref().err() }),
            Phase::Idle | Phase::Running => Value::Null,
        };
        json!({ "running": self.is_running(), "progress": progress, "log": self.log, "result": result })
    }

    /// One line describing how the last run ended, for the status page.
    pub fn summary(&self) -> Option<String> {
        match &self.phase {
            Phase::Finished(Err(error)) => Some(error.clone()),
            Phase::Finished(Ok(())) => self.log.last().cloned(),
            Phase::Idle | Phase::Running => None,
        }
    }
}

pub struct Options {
    pub albums: Vec<AlbumLink>,
    pub delete: bool,
    pub refresh: bool,
    pub limit: Option<usize>,
}

/// What a sync works with.
#[derive(Clone)]
pub struct Context {
    pub cfg: Arc<Config>,
    pub index: Arc<Index>,
    pub job: Arc<Mutex<Job>>,
    pub http: reqwest::Client,
    /// The album-listing address: Google's, or a stand-in during tests.
    pub rpc_url: String,
}

/// The HTTP client for talking to Google, with name lookups that work on the frame.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(crate::dns::FrameResolver))
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(90))
        .build()
        .expect("the HTTP client configuration is valid")
}

fn say(job: &Mutex<Job>, line: String) {
    info!("{line}");
    job.lock().unwrap().log.push(line);
}

fn progress(job: &Mutex<Job>, done: usize, total: usize, detail: &str) {
    let mut job = job.lock().unwrap();
    match &mut job.progress {
        Some(step) if step.detail == detail => (step.done, step.total) = (done, total),
        step => *step = Some(Progress { done, total, detail: detail.to_string(), since: Instant::now() }),
    }
}

/// The stable file name for an album item; must match `frame_sync.py` so photos it uploaded are recognised.
fn name_for(media_key: &str) -> SyncedName {
    SyncedName::from_digest(&store::sha1(format!("gphotos\0{media_key}").as_bytes()))
}

/// Album id and access key from a resolved `photos.google.com/share/<album>?key=<key>` URL.
fn share_ids(url: &str) -> Option<(&str, &str)> {
    let (album, query) = url.split("/share/").nth(1)?.split_once('?')?;
    let key = query.split('&').find_map(|kv| kv.strip_prefix("key="))?;
    (!album.is_empty() && !key.is_empty()).then_some((album, key))
}

#[derive(Debug, PartialEq)]
struct Entry {
    media_key: String,
    /// Where to download a frame-sized copy; None for videos.
    url: Option<String>,
    /// When it was taken, seconds since 1970.
    taken: Option<u64>,
}

/// One page of an album listing.
#[derive(Debug, PartialEq)]
struct AlbumPage {
    entries: Vec<Entry>,
    next: Option<String>,
    /// How many items the album says it has in total.
    total: Option<usize>,
}

/// Parse the body of the album-listing call: a junk first line, then rows of
/// JSON where our row carries the real payload as a JSON string.
fn parse_page(body: &str) -> Option<AlbumPage> {
    let rows: Value = serde_json::from_str(body.split_once('\n')?.1).ok()?;
    let payload = rows.as_array()?.iter().find(|r| r[0] == "wrb.fr" && r[1] == RPC)?[2].as_str()?;
    let data: Value = serde_json::from_str(payload).ok()?;
    let mut entries = Vec::new();
    for entry in data[1].as_array().map(Vec::as_slice).unwrap_or_default() {
        let (Some(media_key), Some(base_url)) = (entry[0].as_str(), entry[1][0].as_str()) else {
            continue;
        };
        let is_video = entry.as_array().and_then(|e| e.last()).is_some_and(|last| last.get("76647426").is_some());
        entries.push(Entry {
            media_key: media_key.to_string(),
            url: (!is_video).then(|| format!("{base_url}{FRAME_SIZE}")),
            taken: entry[2].as_u64().map(|millis| millis / 1000),
        });
    }
    Some(AlbumPage {
        entries,
        next: data[2].as_str().filter(|t| !t.is_empty()).map(str::to_string),
        total: data[3][21].as_u64().and_then(|n| usize::try_from(n).ok()),
    })
}

/// The photos of one album.
#[derive(Default)]
struct Album {
    /// File name -> download URL.
    photos: HashMap<SyncedName, String>,
    /// File name -> when it was taken.
    dates: HashMap<SyncedName, u64>,
    /// Whether every item the album reports was listed.
    complete: bool,
}

async fn list_album(ctx: &Context, link: &AlbumLink) -> Result<Album, SyncError> {
    let (http, job) = (&ctx.http, &*ctx.job);
    // Short links only redirect for non-browser user agents; browsers get a JS page.
    let page = http.get(link.as_str()).header(USER_AGENT, "curl/8.0").send().await.map_err(SyncError::OpenLink)?;
    let url = page.url().to_string();
    let (album_id, key) = share_ids(&url).ok_or(SyncError::NotAnAlbum)?;

    let mut album = Album::default();
    let mut seen = HashSet::new();
    let mut token: Option<String> = None;
    let mut total = None;
    loop {
        let inner = json!([album_id, token, null, key]).to_string();
        let f_req = json!([[[RPC, inner, null, "generic"]]]).to_string();
        let body = http
            .post(format!("{}?rpcids={RPC}", ctx.rpc_url))
            .header(USER_AGENT, BROWSER_UA)
            .form(&[("f.req", f_req)])
            .send()
            .await
            .map_err(SyncError::ReadAlbum)?
            .text()
            .await
            .map_err(SyncError::ReadAlbum)?;
        let page = parse_page(&body).ok_or_else(|| {
            warn!(bytes = body.len(), start = %body.chars().take(80).collect::<String>(), "unexpected album response");
            SyncError::UnexpectedResponse
        })?;
        debug!(entries = page.entries.len(), total = ?page.total, more = page.next.is_some(), "album page");
        total = page.total.or(total);
        let empty = page.entries.is_empty();
        for entry in page.entries {
            if let Some(url) = entry.url {
                let name = name_for(&entry.media_key);
                if let Some(taken) = entry.taken {
                    album.dates.insert(name.clone(), taken);
                }
                album.photos.insert(name, url);
            }
            seen.insert(entry.media_key);
        }
        progress(job, seen.len(), total.unwrap_or(seen.len()), "reading the album from Google Photos");
        token = page.next;
        if empty || token.is_none() {
            break;
        }
    }
    album.complete = total.is_none_or(|t| seen.len() >= t);
    if !album.complete {
        say(job, format!("warning: album reports {} items but only {} were listed", total.unwrap_or(0), seen.len()));
    }
    if seen.len() > album.photos.len() {
        say(job, format!("skipping {} videos", seen.len() - album.photos.len()));
    }
    Ok(album)
}

async fn download(http: &reqwest::Client, url: &str, dest: &Path) -> Result<(), DownloadError> {
    let response = http.get(url).header(USER_AGENT, BROWSER_UA).send().await?.error_for_status()?;
    if response.content_length().is_some_and(|len| len > MAX_PHOTO_BYTES) {
        return Err(DownloadError::TooLarge);
    }
    let data = response.bytes().await?;
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err(DownloadError::NotJpeg);
    }
    let tmp = dest.with_extension("part");
    tokio::fs::write(&tmp, &data).await?;
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

/// Download photos into `dir`, several at once. Returns how many failed.
async fn download_all(ctx: &Context, dir: &Path, photos: Vec<(SyncedName, String)>) -> usize {
    let job = &*ctx.job;
    let total = photos.len();
    let (mut done, mut failed) = (0usize, 0usize);
    progress(job, 0, total, "downloading to the frame");
    let mut finished = |outcome: Result<(SyncedName, Result<(), DownloadError>), tokio::task::JoinError>| {
        done += 1;
        match outcome {
            Ok((_, Ok(()))) => {}
            Ok((name, Err(e))) => {
                failed += 1;
                warn!(file = %name, error = %e, "download failed");
                job.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log.push(format!("skipped {name}: {e}"));
            }
            Err(e) => {
                failed += 1;
                warn!(error = %e, "download task failed");
            }
        }
        progress(job, done, total, "downloading to the frame");
    };
    // Each task holds a permit while it runs, which caps how many run at once.
    let limit = Arc::new(Semaphore::new(DOWNLOADS_IN_PARALLEL));
    let mut tasks = JoinSet::new();
    for (name, url) in photos {
        let permit = Arc::clone(&limit).acquire_owned().await.expect("semaphore is never closed");
        let (http, dest) = (ctx.http.clone(), dir.join(name.as_str()));
        tasks.spawn(async move {
            let _permit = permit;
            let result = download(&http, &url, &dest).await;
            (name, result)
        });
        while let Some(outcome) = tasks.try_join_next() {
            finished(outcome);
        }
    }
    while let Some(outcome) = tasks.join_next().await {
        finished(outcome);
    }
    failed
}

/// Photos in the bin whose thumbnail is missing (deleted before thumbnails were kept) get a
/// small copy from the album, so the list of deleted photos can show them.
async fn fetch_missing_bin_thumbnails(ctx: &Context, album: &HashMap<SyncedName, String>) {
    let cfg = Arc::clone(&ctx.cfg);
    let wanted: Vec<SyncedName> = blocking(move || {
        store::load_bin(&cfg).into_keys().filter(|name| !store::thumbnail_exists(&cfg, &name.path())).collect()
    })
    .await;
    let missing: Vec<(SyncedName, String)> = wanted
        .into_iter()
        .filter_map(|name| {
            let small = album.get(&name)?.replace(FRAME_SIZE, THUMBNAIL_SIZE);
            Some((name, small))
        })
        .collect();
    if missing.is_empty() {
        return;
    }
    if let Some(folder) = store::thumb_path(&ctx.cfg, &missing[0].0.path()).parent() {
        let _ = tokio::fs::create_dir_all(folder).await;
    }
    let limit = Arc::new(Semaphore::new(DOWNLOADS_IN_PARALLEL));
    let mut tasks = JoinSet::new();
    let total = missing.len();
    for (name, url) in missing {
        let permit = Arc::clone(&limit).acquire_owned().await.expect("semaphore is never closed");
        let (http, dest) = (ctx.http.clone(), store::thumb_path(&ctx.cfg, &name.path()));
        tasks.spawn(async move {
            let _permit = permit;
            download(&http, &url, &dest).await
        });
    }
    let mut fetched = 0usize;
    while let Some(outcome) = tasks.join_next().await {
        fetched += usize::from(matches!(outcome, Ok(Ok(()))));
    }
    say(&ctx.job, format!("fetched thumbnails for {fetched} of {total} deleted photos that had none"));
}

/// What is already on the frame, and where synced photos go.
struct OnFrame {
    excluded: HashSet<SyncedName>,
    synced: HashSet<SyncedName>,
    dir: PathBuf,
}

fn on_frame(cfg: &Config) -> Result<OnFrame, SyncError> {
    let dir = cfg.sync_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(OnFrame {
        excluded: store::load_excluded(cfg).into_iter().collect(),
        synced: store::synced_names(cfg).into_iter().collect(),
        dir,
    })
}

async fn sync(ctx: &Context, options: &Options) -> Result<(), SyncError> {
    let (cfg, job) = (&ctx.cfg, &*ctx.job);
    let mut wanted = HashMap::new();
    let mut complete = true;
    for link in &options.albums {
        let album = list_album(ctx, link).await?;
        say(job, format!("{link}: {} photos", album.photos.len()));
        complete &= album.complete;
        for (name, taken) in &album.dates {
            ctx.index.set_taken(&name.path(), *taken);
        }
        wanted.extend(album.photos);
    }
    let index = Arc::clone(&ctx.index);
    blocking(move || index.save()).await;

    let c = Arc::clone(cfg);
    let OnFrame { excluded, synced, dir } = blocking(move || on_frame(&c)).await?;
    fetch_missing_bin_thumbnails(ctx, &wanted).await;
    let skipped = wanted.keys().filter(|n| excluded.contains(*n)).count();
    if skipped > 0 {
        say(job, format!("leaving out {skipped} photos you deleted earlier"));
        wanted.retain(|name, _| !excluded.contains(name));
    }

    let mut to_add: Vec<(SyncedName, String)> =
        wanted.iter().filter(|(name, _)| !synced.contains(*name)).map(|(n, u)| (n.clone(), u.clone())).collect();
    to_add.sort();
    let mut to_remove: Vec<SyncedName> = Vec::new();
    if options.delete {
        if complete {
            to_remove = synced.iter().filter(|n| !wanted.contains_key(*n)).cloned().collect();
        } else {
            say(job, "warning: not removing anything because an album listing was incomplete".into());
        }
    }
    say(job, format!("on frame: {}   to download: {}   to remove: {}", synced.len(), to_add.len(), to_remove.len()));
    if let Some(limit) = options.limit.filter(|&l| to_add.len() > l) {
        to_add.truncate(limit);
        say(job, format!("limiting this run to {limit} downloads; sync again for the rest"));
    }

    let started = Instant::now();
    let total = to_add.len();
    let failed = download_all(ctx, &dir, to_add).await;
    let (downloaded, removed) = (total - failed, to_remove.len());
    let (c, refresh) = (Arc::clone(cfg), options.refresh);
    let on_frame_now = blocking(move || {
        for name in &to_remove {
            let _ = std::fs::remove_file(dir.join(name.as_str()));
        }
        if downloaded > 0 || removed > 0 {
            store::scan(&dir); // one folder scan indexes the new files and drops the removed ones
            if refresh {
                store::refresh_slideshow();
            }
        }
        store::synced_names(&c).len()
    })
    .await;
    say(
        job,
        format!(
            "done in {}s: downloaded {downloaded}, removed {removed}, failed {failed}. {on_frame_now} synced photos on the frame.",
            started.elapsed().as_secs()
        ),
    );
    Ok(())
}

/// Mark a sync as started. Returns false if one is already running.
pub fn try_start(job: &Mutex<Job>) -> bool {
    let mut job = job.lock().unwrap();
    if job.is_running() {
        return false;
    }
    *job = Job { phase: Phase::Running, ..Job::default() };
    true
}

/// Run a sync that `try_start` accepted, recording progress and the outcome in the job.
pub async fn run(ctx: Context, options: Options) {
    let outcome = sync(&ctx, &options).await;
    if let Err(e) = &outcome {
        error!(error = %e, "sync failed");
    }
    ctx.job.lock().unwrap().phase = Phase::Finished(outcome.map_err(|e| e.to_string()));
}

/// A stand-in for Google Photos, for tests: it serves one shared album over local HTTP.
#[cfg(test)]
pub mod testing {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::extract::{Form, Path, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Redirect, Response};
    use axum::routing::{get, post};
    use axum::Router;
    use serde_json::{json, Value};

    use crate::store::testing::picture;

    /// What the fake album holds for one media key.
    #[derive(Clone, Copy, PartialEq)]
    pub enum Item {
        Photo,
        Video,
        /// Listed, but downloading it fails.
        Broken,
        /// Listed, but what downloads is not an image.
        NotAnImage,
    }

    struct Album {
        base: String,
        items: Vec<(&'static str, Item)>,
        /// How many items the album claims to have beyond those it lists.
        unlisted: usize,
    }

    /// When the fake photos were taken: one day apart, starting here.
    pub const FIRST_TAKEN: u64 = 1_600_000_000;
    const PAGE_SIZE: usize = 2;

    /// Start the fake and return its address, such as `http://127.0.0.1:4821`.
    /// The album link is `<address>/a/album`; the listing address is `<address>/rpc`.
    pub async fn google(items: Vec<(&'static str, Item)>, unlisted: usize) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let album = Arc::new(Album { base: base.clone(), items, unlisted });
        let app = Router::new()
            .route("/a/album", get(|| async { Redirect::temporary("/share/ALBUM?pli=1&key=KEY") }))
            .route("/a/other", get(|| async { Redirect::temporary("/share/SOMEONE_ELSES?key=KEY") }))
            .route("/a/not-an-album", get(|| async { "a web page" }))
            .route("/share/{album}", get(|| async { "the album page" }))
            .route("/rpc", post(list))
            .route("/img/{name}", get(image))
            .with_state(album);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    /// One page of the album, in the shape Google sends it.
    async fn list(State(album): State<Arc<Album>>, Form(form): Form<HashMap<String, String>>) -> String {
        let request: Value = serde_json::from_str(&form["f.req"]).unwrap();
        let asked: Value = serde_json::from_str(request[0][0][1].as_str().unwrap()).unwrap();
        if asked[0] != "ALBUM" || asked[3] != "KEY" {
            return "<html>sign in</html>".into();
        }
        let from: usize = asked[1].as_str().map_or(0, |token| token.parse().unwrap());
        let entries: Vec<Value> = album
            .items
            .iter()
            .enumerate()
            .skip(from)
            .take(PAGE_SIZE)
            .map(|(n, (key, item))| {
                let extra = if *item == Item::Video { json!({ "76647426": [1234] }) } else { json!({ "1": 2 }) };
                let taken_ms = (FIRST_TAKEN + n as u64 * 86_400) * 1000;
                json!([
                    key,
                    [format!("{}/img/{key}", album.base), 4000, 3000],
                    taken_ms,
                    "d",
                    0,
                    [],
                    0,
                    null,
                    null,
                    extra
                ])
            })
            .collect();
        let next = if from + PAGE_SIZE < album.items.len() { (from + PAGE_SIZE).to_string() } else { String::new() };
        let mut info = vec![Value::Null; 43];
        info[21] = json!(album.items.len() + album.unlisted);
        let payload = json!([null, entries, next, info]).to_string();
        format!(")]}}'\n{}", json!([["wrb.fr", "snAcKc", payload, null, null, null, "generic"], ["di", 42]]))
    }

    /// `/img/<key>=w<width>-h<height>`: a JPEG of roughly that size, or whatever the item misbehaves with.
    async fn image(State(album): State<Arc<Album>>, Path(name): Path<String>) -> Response {
        let (key, wanted) = name.split_once('=').unwrap();
        match album.items.iter().find(|(k, _)| *k == key).map(|(_, item)| *item) {
            Some(Item::Photo) => {
                let width = if wanted.starts_with("w480") { 48 } else { 192 };
                picture(width, width * 3 / 4, image::ImageFormat::Jpeg).into_response()
            }
            Some(Item::NotAnImage) => "<html>quota exceeded</html>".into_response(),
            Some(Item::Broken) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            Some(Item::Video) | None => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{google, Item, FIRST_TAKEN};
    use super::*;
    use crate::store::testing::{picture, TempFrame};
    use crate::types::Restore;

    fn context(frame: &TempFrame, google: &str) -> Context {
        let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
        Context {
            index: Arc::new(Index::load(&cfg)),
            cfg: Arc::new(cfg),
            job: Arc::default(),
            http: client(),
            rpc_url: format!("{google}/rpc"),
        }
    }

    fn album(google: &str) -> Vec<AlbumLink> {
        vec![AlbumLink::local(&format!("{google}/a/album"))]
    }

    fn options(google: &str) -> Options {
        Options { albums: album(google), delete: false, refresh: false, limit: None }
    }

    fn on_frame_names(ctx: &Context) -> Vec<String> {
        let mut names: Vec<String> = store::synced_names(&ctx.cfg).iter().map(ToString::to_string).collect();
        names.sort();
        names
    }

    fn log(ctx: &Context) -> String {
        ctx.job.lock().unwrap().log.join("\n")
    }

    #[tokio::test]
    async fn listing_follows_the_link_pages_through_the_album_and_skips_videos() {
        let google = google(
            vec![
                ("K1", Item::Photo),
                ("K2", Item::Photo),
                ("K3", Item::Video),
                ("K4", Item::Photo),
                ("K5", Item::Photo),
            ],
            0,
        )
        .await;
        let frame = TempFrame::new("gphotos-list");
        let ctx = context(&frame, &google);
        let listed = list_album(&ctx, &album(&google)[0]).await.unwrap();
        assert!(listed.complete);
        let mut names: Vec<&SyncedName> = listed.photos.keys().collect();
        names.sort();
        let mut expected = [name_for("K1"), name_for("K2"), name_for("K4"), name_for("K5")];
        expected.sort();
        assert_eq!(names, expected.iter().collect::<Vec<_>>(), "three pages, the video left out");
        assert_eq!(listed.photos[&name_for("K4")], format!("{google}/img/K4=w1920-h1920"));
        assert_eq!(listed.dates[&name_for("K1")], FIRST_TAKEN);
        assert_eq!(listed.dates[&name_for("K5")], FIRST_TAKEN + 4 * 86_400);
        assert!(log(&ctx).contains("skipping 1 videos"));
        let progress = ctx.job.lock().unwrap().view()["progress"].clone();
        assert_eq!((progress["done"].as_u64(), progress["total"].as_u64()), (Some(5), Some(5)));
    }

    #[tokio::test]
    async fn a_listing_shorter_than_the_album_claims_is_marked_incomplete() {
        let google = google(vec![("K1", Item::Photo)], 4).await;
        let frame = TempFrame::new("gphotos-incomplete");
        let ctx = context(&frame, &google);
        let listed = list_album(&ctx, &album(&google)[0]).await.unwrap();
        assert!(!listed.complete);
        assert!(log(&ctx).contains("album reports 5 items but only 1 were listed"));
    }

    #[tokio::test]
    async fn links_that_are_not_readable_albums_fail_with_the_reason() {
        let google = google(vec![("K1", Item::Photo)], 0).await;
        let frame = TempFrame::new("gphotos-bad-links");
        let ctx = context(&frame, &google);
        let try_link = |path: &str| AlbumLink::local(&format!("{google}{path}"));
        assert!(matches!(list_album(&ctx, &try_link("/a/not-an-album")).await, Err(SyncError::NotAnAlbum)));
        assert!(matches!(list_album(&ctx, &try_link("/a/other")).await, Err(SyncError::UnexpectedResponse)));
        let nobody_home = AlbumLink::local("http://127.0.0.1:1/a/album");
        assert!(matches!(list_album(&ctx, &nobody_home).await, Err(SyncError::OpenLink(_))));
        let mut unreachable_listing = ctx.clone();
        unreachable_listing.rpc_url = "http://127.0.0.1:1/rpc".into();
        assert!(matches!(list_album(&unreachable_listing, &try_link("/a/album")).await, Err(SyncError::ReadAlbum(_))));
    }

    #[tokio::test]
    async fn a_sync_downloads_what_is_missing_and_nothing_else() {
        let items = vec![
            ("K1", Item::Photo),
            ("K2", Item::Photo),
            ("K3", Item::Video),
            ("K4", Item::Photo),
            ("K5", Item::Broken),
            ("K6", Item::NotAnImage),
        ];
        let google = google(items, 0).await;
        let frame = TempFrame::new("gphotos-sync");
        let ctx = context(&frame, &google);
        // K1 is already on the frame; K2 was deleted earlier and sits in the bin with no thumbnail.
        frame.put(name_for("K1").path().as_str(), b"already here");
        frame.put(name_for("K2").path().as_str(), b"x");
        store::delete(&ctx.cfg, &[name_for("K2").path()]);
        std::fs::remove_file(store::thumb_path(&ctx.cfg, &name_for("K2").path())).ok();

        assert!(try_start(&ctx.job));
        run(ctx.clone(), options(&google)).await;

        assert_eq!(ctx.job.lock().unwrap().phase, Phase::Finished(Ok(())));
        let mut expected = vec![name_for("K1").to_string(), name_for("K4").to_string()];
        expected.sort();
        assert_eq!(on_frame_names(&ctx), expected, "only K4 was new and downloadable");
        assert_eq!(
            std::fs::read(name_for("K1").path().within(&ctx.cfg.cloud_dir)).unwrap(),
            b"already here",
            "untouched"
        );
        assert!(std::fs::read(name_for("K4").path().within(&ctx.cfg.cloud_dir)).unwrap().starts_with(&[0xff, 0xd8]));
        let leftovers = std::fs::read_dir(ctx.cfg.sync_dir())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "part"))
            .count();
        assert_eq!(leftovers, 0, "failed downloads leave no partial files");

        let log = log(&ctx);
        assert!(log.contains("leaving out 1 photos you deleted earlier"), "{log}");
        assert!(log.contains("on frame: 1   to download: 3   to remove: 0"), "{log}");
        assert!(log.contains("downloaded 1, removed 0, failed 2"), "{log}");
        assert!(log.contains(&format!("skipped {}: Google did not return a JPEG", name_for("K6"))), "{log}");
        assert!(log.contains("fetched thumbnails for 1 of 1 deleted photos that had none"), "{log}");
        assert!(store::thumbnail_exists(&ctx.cfg, &name_for("K2").path()), "the bin can now show the deleted photo");
        assert_eq!(
            ctx.index.get(&name_for("K4").path()).taken,
            Some(FIRST_TAKEN + 3 * 86_400),
            "the album's date is recorded"
        );
        assert_eq!(ctx.job.lock().unwrap().summary().as_deref(), log.lines().last());
    }

    #[tokio::test]
    async fn a_second_sync_has_nothing_to_do_and_restoring_brings_a_photo_back() {
        let google = google(vec![("K1", Item::Photo), ("K2", Item::Photo)], 0).await;
        let frame = TempFrame::new("gphotos-resync");
        let ctx = context(&frame, &google);
        sync(&ctx, &options(&google)).await.unwrap();
        assert_eq!(on_frame_names(&ctx).len(), 2);
        sync(&ctx, &options(&google)).await.unwrap();
        assert!(log(&ctx).contains("on frame: 2   to download: 0   to remove: 0"));

        store::delete(&ctx.cfg, &[name_for("K1").path()]);
        sync(&ctx, &options(&google)).await.unwrap();
        assert_eq!(on_frame_names(&ctx), [name_for("K2").to_string()], "a deleted photo stays away");
        store::restore(&ctx.cfg, &Restore::All);
        sync(&ctx, &options(&google)).await.unwrap();
        assert_eq!(on_frame_names(&ctx).len(), 2, "until it is restored");
    }

    #[tokio::test]
    async fn the_download_limit_is_respected() {
        let google = google(vec![("K1", Item::Photo), ("K2", Item::Photo), ("K3", Item::Photo)], 0).await;
        let frame = TempFrame::new("gphotos-limit");
        let ctx = context(&frame, &google);
        sync(&ctx, &Options { limit: Some(1), ..options(&google) }).await.unwrap();
        assert_eq!(on_frame_names(&ctx).len(), 1);
        assert!(log(&ctx).contains("limiting this run to 1 downloads"));
        sync(&ctx, &Options { limit: Some(5), ..options(&google) }).await.unwrap();
        assert_eq!(on_frame_names(&ctx).len(), 3, "a limit above what is missing changes nothing");
    }

    #[tokio::test]
    async fn mirroring_removes_synced_photos_that_left_the_album_but_never_on_a_partial_listing() {
        let stray = SyncedName::parse("fs_eeeeeeeeeeeeeeee.jpg").unwrap();
        let frame = TempFrame::new("gphotos-mirror");
        frame.put(stray.path().as_str(), &picture(32, 24, image::ImageFormat::Jpeg));
        frame.put("frame-sync/up_0123456789abcdef.jpg", b"added by hand");

        let partial = google(vec![("K1", Item::Photo)], 3).await;
        let ctx = context(&frame, &partial);
        sync(&ctx, &Options { delete: true, ..options(&partial) }).await.unwrap();
        assert!(on_frame_names(&ctx).contains(&stray.to_string()), "kept: the listing was incomplete");
        assert!(log(&ctx).contains("not removing anything because an album listing was incomplete"));

        let whole = google(vec![("K1", Item::Photo)], 0).await;
        let ctx = context(&frame, &whole);
        sync(&ctx, &Options { delete: true, ..options(&whole) }).await.unwrap();
        assert_eq!(on_frame_names(&ctx), [name_for("K1").to_string()], "the stray synced photo is gone");
        assert!(
            frame.0.cloud_dir.join("frame-sync/up_0123456789abcdef.jpg").exists(),
            "photos added by hand are never mirrored away"
        );
        assert!(store::load_excluded(&ctx.cfg).is_empty(), "removed by mirroring, not deleted by the user");
    }

    #[tokio::test]
    async fn a_failed_sync_ends_with_its_error_as_the_summary() {
        let google = google(vec![("K1", Item::Photo)], 0).await;
        let frame = TempFrame::new("gphotos-fail");
        let ctx = context(&frame, &google);
        assert!(try_start(&ctx.job));
        let bad = Options { albums: vec![AlbumLink::local(&format!("{google}/a/not-an-album"))], ..options(&google) };
        run(ctx.clone(), bad).await;
        let job = ctx.job.lock().unwrap();
        assert!(!job.is_running());
        assert!(job.summary().unwrap().starts_with("not a Google Photos shared-album link"));
        assert_eq!(job.view()["result"]["ok"], false);
    }

    /// A listing response shaped like Google's: junk line, then rows whose payload is a JSON string.
    fn response(payload: &Value) -> String {
        let rows = json!([["wrb.fr", RPC, payload.to_string(), null, null, null, "generic"], ["di", 42]]);
        format!(")]}}'\n{rows}")
    }

    fn album_info(total: u64) -> Value {
        let mut info = vec![Value::Null; 43];
        info[21] = json!(total);
        Value::Array(info)
    }

    #[test]
    fn file_names_match_the_python_sync_tool() {
        // Values from frame_sync.py: sha1("gphotos\0" + media key)[:16]
        assert_eq!(name_for("AF1QipTESTKEY").as_str(), "fs_3bfa83970424045a.jpg");
        assert_eq!(name_for("AF1QipN-abc_123").as_str(), "fs_a9f5a3221e70280d.jpg");
    }

    #[test]
    fn share_ids_come_from_the_resolved_url() {
        let url = "https://photos.google.com/share/AF1QipAlbum?pli=1&key=SecretKey";
        assert_eq!(share_ids(url), Some(("AF1QipAlbum", "SecretKey")));
        assert_eq!(share_ids("https://photos.google.com/share/AF1QipAlbum?key=K"), Some(("AF1QipAlbum", "K")));
    }

    #[test]
    fn share_ids_reject_other_links() {
        for url in [
            "https://example.com/",
            "https://photos.google.com/share/AF1QipAlbum",
            "https://photos.google.com/share/AF1QipAlbum?pli=1",
            "https://photos.google.com/share/?key=K",
            "https://photos.app.goo.gl/abc",
        ] {
            assert_eq!(share_ids(url), None, "{url}");
        }
    }

    #[test]
    fn parse_page_reads_photos_videos_dates_token_and_total() {
        let payload = json!([
            null,
            [
                ["AF1QipPhoto", ["https://lh3.googleusercontent.com/pw/a", 4000, 3000], 1_680_420_498_744_u64, "d", 0, [], 0, null, null, {"1": 2}],
                ["AF1QipVideo", ["https://lh3.googleusercontent.com/pw/b", 1920, 1080], 1_564_812_698_000_u64, "d", 0, [], 0, null, null, {"76647426": [1234]}],
                ["AF1QipUndated", ["https://lh3.googleusercontent.com/pw/c", 10, 10]],
                ["AF1QipBroken"],
            ],
            "next-page-token",
            album_info(10928),
        ]);
        let page = parse_page(&response(&payload)).unwrap();
        assert_eq!(
            page.entries,
            vec![
                Entry {
                    media_key: "AF1QipPhoto".into(),
                    url: Some("https://lh3.googleusercontent.com/pw/a=w1920-h1920".into()),
                    taken: Some(1_680_420_498),
                },
                Entry { media_key: "AF1QipVideo".into(), url: None, taken: Some(1_564_812_698) },
                Entry {
                    media_key: "AF1QipUndated".into(),
                    url: Some("https://lh3.googleusercontent.com/pw/c=w1920-h1920".into()),
                    taken: None,
                },
            ]
        );
        assert_eq!(page.next.as_deref(), Some("next-page-token"));
        assert_eq!(page.total, Some(10928));
    }

    #[test]
    fn parse_page_last_page_has_no_token() {
        let page = parse_page(&response(&json!([null, [], "", album_info(3)]))).unwrap();
        assert_eq!(page, AlbumPage { entries: vec![], next: None, total: Some(3) });
        let page = parse_page(&response(&json!([null, null, null, null]))).unwrap();
        assert_eq!(page, AlbumPage { entries: vec![], next: None, total: None });
    }

    #[test]
    fn parse_page_rejects_unexpected_responses() {
        assert_eq!(parse_page(""), None);
        assert_eq!(parse_page("<html>sign in</html>"), None);
        assert_eq!(parse_page(")]}'\n[[\"wrb.fr\",\"other\",\"[]\"]]"), None);
        assert_eq!(parse_page(")]}'\n[[\"wrb.fr\",\"snAcKc\",null]]"), None);
        assert_eq!(parse_page(")]}'\nnot json"), None);
    }

    #[test]
    fn a_second_sync_cannot_start_while_one_runs() {
        let job = Mutex::new(Job::default());
        assert!(try_start(&job));
        assert!(!try_start(&job));
        assert!(job.lock().unwrap().view()["running"].as_bool().unwrap());
    }

    #[test]
    fn a_finished_job_can_be_started_again_and_starts_clean() {
        let job = Mutex::new(Job::default());
        assert!(try_start(&job));
        say(&job, "old line".into());
        progress(&job, 1, 2, "downloading to the frame");
        job.lock().unwrap().phase = Phase::Finished(Err("boom".into()));
        assert!(!job.lock().unwrap().is_running());
        assert!(try_start(&job));
        let view = job.lock().unwrap().view();
        assert_eq!(
            (view["running"].as_bool(), view["log"].clone(), view["progress"].clone(), view["result"].clone()),
            (Some(true), json!([]), Value::Null, Value::Null)
        );
    }

    #[test]
    fn progress_restarts_its_clock_only_when_the_step_changes() {
        let job = Mutex::new(Job::default());
        progress(&job, 1, 10, "reading the album from Google Photos");
        let first = job.lock().unwrap().progress.as_ref().unwrap().since;
        progress(&job, 5, 10, "reading the album from Google Photos");
        assert_eq!(job.lock().unwrap().progress.as_ref().unwrap().since, first);
        assert_eq!(job.lock().unwrap().progress.as_ref().unwrap().done, 5);
        progress(&job, 0, 3, "downloading to the frame");
        let step = job.lock().unwrap();
        let step = step.progress.as_ref().unwrap();
        assert_eq!((step.done, step.total, step.detail.as_str()), (0, 3, "downloading to the frame"));
    }

    #[test]
    fn job_view_reports_progress_and_outcome() {
        let job = Mutex::new(Job::default());
        assert_eq!(job.lock().unwrap().view()["progress"], Value::Null);
        assert_eq!(job.lock().unwrap().summary(), None);
        progress(&job, 3, 10, "downloading to the frame");
        let view = job.lock().unwrap().view();
        assert_eq!((view["progress"]["done"].as_u64(), view["progress"]["total"].as_u64()), (Some(3), Some(10)));
        assert_eq!(view["result"], Value::Null);

        job.lock().unwrap().phase = Phase::Finished(Err("boom".into()));
        assert_eq!(job.lock().unwrap().view()["result"], json!({"ok": false, "error": "boom"}));
        assert_eq!(job.lock().unwrap().summary().as_deref(), Some("boom"));
        say(&job, "done in 3s".into());
        job.lock().unwrap().phase = Phase::Finished(Ok(()));
        assert_eq!(job.lock().unwrap().view()["result"], json!({"ok": true, "error": null}));
        assert_eq!(job.lock().unwrap().summary().as_deref(), Some("done in 3s"));
    }
}
