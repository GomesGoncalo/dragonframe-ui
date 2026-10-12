//! Photo manager web app that runs on the Dragon Touch frame itself.
//!
//! Open `http://<frame address>:8080` from any browser on the same network to
//! see every photo on the frame, delete photos, add more, find duplicates and
//! sync Google Photos shared albums. Nothing goes through adb or another
//! computer.
//!
//! The server is async (tokio + axum). Work that blocks, such as decoding
//! images, walking the photo folder and running Android commands, lives in
//! plain functions in `store` and `index` and is run through [`blocking`].
//!
//! Requests are parsed into types at the door (see `types`): a handler that
//! takes a `DeleteRequest` already holds valid photo paths and has nothing
//! left to check.

mod auth;
mod dns;
mod faces;
mod gphotos;
mod index;
mod store;
mod types;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Query, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn, Level};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::writer::MakeWriterExt;

use gphotos::Phase;
use index::Index;
use store::{Config, LastSync, Settings};
use types::{AlbumLinks, Bounded, Likeness, NonEmpty, PhotoPath, PinChange, Restore, Source, SyncedName};

const PAGE: &str = include_str!("page.html");
const MAX_BODY: usize = 60 * 1024 * 1024;
const DAY: u64 = 24 * 60 * 60;
/// Deleted synced photos can be restored for this long; after that they leave the bin for good.
const BIN_DAYS: u64 = 30;
const MAX_DUPLICATE_GROUPS: usize = 500;
/// Most photos one request may ask to have their faces looked at (each takes seconds on the frame).
const MAX_ANALYSE: usize = 40;
const ANALYSE_IN_PARALLEL: usize = 2;
/// The face models are dropped after this long without use.
const MODELS_IDLE: Duration = Duration::from_mins(5);
/// Most groups the page may report as being on screen at once.
const MAX_WATCHED_GROUPS: usize = 12;
/// Rest between photos nobody is waiting for, so background face checks stay gentle on the frame.
const BACKGROUND_PAUSE: Duration = Duration::from_secs(3);
/// How often the list of background work is rebuilt, to pick up new photos.
const REPLAN_EVERY: Duration = Duration::from_mins(15);
const NO_STORE: HeaderValue = HeaderValue::from_static("no-store");
const KEEP_FOREVER: HeaderValue = HeaderValue::from_static("private, max-age=31536000, immutable");

struct App {
    cfg: Arc<Config>,
    index: Arc<Index>,
    job: Arc<Mutex<gphotos::Job>>,
    guard: auth::Guard,
    http: reqwest::Client,
    started: Instant,
    /// Serialises read-modify-write of the settings file.
    settings_lock: Mutex<()>,
    /// The face models, held only while there are faces to look at.
    models: faces::OnDemand,
    /// Which photos to look at next.
    face_queue: Mutex<faces::Queue>,
    /// How many photos have look-alikes, as of the last time the background work was planned.
    with_look_alikes: AtomicUsize,
    /// Where album listings come from: Google, or a stand-in during tests.
    rpc_url: String,
}

type Shared = Arc<App>;

impl App {
    fn new(cfg: Config) -> Shared {
        let index = Index::load(&cfg);
        Arc::new(App {
            cfg: Arc::new(cfg),
            index: Arc::new(index),
            job: Arc::default(),
            guard: auth::Guard::default(),
            http: gphotos::client(),
            started: Instant::now(),
            settings_lock: Mutex::new(()),
            models: faces::OnDemand::default(),
            face_queue: Mutex::default(),
            with_look_alikes: AtomicUsize::new(0),
            rpc_url: gphotos::RPC_URL.to_string(),
        })
    }

    /// Change the settings file atomically with respect to other requests. Blocking.
    fn update_settings<T>(&self, change: impl FnOnce(&mut Settings) -> T) -> T {
        let _guard = self.settings_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut settings = store::load_settings(&self.cfg);
        let result = change(&mut settings);
        store::save_settings(&self.cfg, &settings);
        result
    }

    fn sync_context(&self) -> gphotos::Context {
        gphotos::Context {
            cfg: Arc::clone(&self.cfg),
            index: Arc::clone(&self.index),
            job: Arc::clone(&self.job),
            http: self.http.clone(),
            rpc_url: self.rpc_url.clone(),
        }
    }
}

/// Run blocking work (file access, image decoding, Android commands) off the async threads.
///
/// # Panics
/// If the work itself panics.
pub async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work).await.expect("a blocking task panicked")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// --- errors and small helpers ---------------------------------------------------

/// An error reply: a status and a message the page can show.
#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError { status, message: message.into() }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not found")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, [(CACHE_CONTROL, NO_STORE)], Json(json!({ "error": self.message }))).into_response()
    }
}

type ApiResult<T = Json<Value>> = Result<T, ApiError>;

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A request body parsed into `T`. A body that does not parse is answered with 400 and the
/// reason, so a handler never sees a request it would have to validate.
struct Parsed<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Parsed<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let body = Bytes::from_request(request, state).await.map_err(|e| ApiError::new(e.status(), e.body_text()))?;
        let json: &[u8] = if body.is_empty() { b"{}" } else { &body };
        serde_json::from_slice(json).map(Parsed).map_err(|e| {
            // serde appends where in the text it stopped; the page only needs the reason.
            let reason = e.to_string();
            ApiError::bad_request(reason.split(" at line ").next().unwrap_or(&reason))
        })
    }
}

#[derive(Deserialize)]
struct DeleteRequest {
    paths: NonEmpty<PhotoPath>,
}

/// The photos of one group of look-alikes.
type Group = Bounded<PhotoPath, 1, MAX_ANALYSE>;

#[derive(Deserialize)]
struct AnalyseRequest {
    paths: Group,
}

#[derive(Deserialize)]
struct WatchingRequest {
    groups: Bounded<Group, 0, MAX_WATCHED_GROUPS>,
}

#[derive(Deserialize)]
struct RestoreRequest {
    names: Restore,
}

#[derive(Deserialize)]
struct AlbumsRequest {
    sources: AlbumLinks,
}

#[derive(Deserialize)]
struct DailySyncRequest {
    #[serde(default)]
    enabled: bool,
}

#[derive(Deserialize)]
struct SyncRequest {
    sources: AlbumLinks,
    #[serde(default)]
    delete: bool,
    #[serde(default)]
    refresh: bool,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct UnlockRequest {
    /// Whatever was typed: a wrong guess need not even look like a PIN.
    #[serde(default)]
    pin: String,
}

#[derive(Deserialize)]
struct PinRequest {
    pin: PinChange,
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex =
            (bytes[i] == b'%').then(|| text.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(byte) = hex {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn content_type(photo: &PhotoPath) -> &'static str {
    match photo.as_str().rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

// --- middleware --------------------------------------------------------------------

/// Pages on other websites must not be able to drive this app through a
/// visitor's browser: changes need a header browsers only send same-origin,
/// and the Origin, when present, has to be us.
fn same_origin(headers: &HeaderMap) -> bool {
    let host = header(headers, HOST.as_str()).unwrap_or("");
    let origin_ok = header(headers, ORIGIN.as_str()).is_none_or(|origin| origin.split("://").nth(1) == Some(host));
    origin_ok && headers.contains_key("x-frame-app")
}

/// Every change (POST) must come from our own page and, when a PIN is set, from an unlocked session.
async fn protect(State(app): State<Shared>, request: Request, next: Next) -> Response {
    if request.method() == Method::POST {
        if !same_origin(request.headers()) {
            return ApiError::new(StatusCode::FORBIDDEN, "forbidden").into_response();
        }
        // Unlocking is how a session starts, and analysing photos changes nothing on the frame.
        if !matches!(request.uri().path(), "/api/unlock" | "/api/analyse" | "/api/watching") {
            let token = header(request.headers(), "x-frame-token").map(str::to_string);
            let cfg = Arc::clone(&app.cfg);
            let allowed = blocking(move || auth::allowed(&store::load_settings(&cfg), token.as_deref())).await;
            if !allowed {
                return ApiError::new(StatusCode::UNAUTHORIZED, "PIN required").into_response();
            }
        }
    }
    next.run(request).await
}

async fn log_requests(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let (method, url) = (request.method().clone(), request.uri().to_string());
    let mut response = next.run(request).await;
    response.headers_mut().insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    let status = response.status().as_u16();
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if matches!(status, 401 | 403 | 429) || status >= 500 {
        warn!(%method, url, status, ms, "request refused or failed");
    } else {
        let bytes = header(response.headers(), CONTENT_LENGTH.as_str()).unwrap_or("?");
        debug!(%method, url, status, bytes, ms, "request");
    }
    response
}

// --- reading ---------------------------------------------------------------------------

async fn page() -> impl IntoResponse {
    ([(CACHE_CONTROL, NO_STORE)], Html(PAGE))
}

async fn logs(State(app): State<Shared>) -> impl IntoResponse {
    let cfg = Arc::clone(&app.cfg);
    let text = blocking(move || store::recent_log(&cfg, 500)).await;
    ([(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8")), (CACHE_CONTROL, NO_STORE)], text)
}

/// What the page polls: the running job, what is on screen, and (with `?full`) every photo.
async fn state(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<Value> {
    let full = query.contains_key("full");
    let token = header(&headers, "x-frame-token").map(str::to_string);
    let job = app.job.lock().unwrap_or_else(std::sync::PoisonError::into_inner).view();
    let shared = Arc::clone(&app);
    let mut out = blocking(move || {
        let cfg = &shared.cfg;
        let settings = store::load_settings(cfg);
        let mut bin: Vec<(SyncedName, u64)> = store::load_bin(cfg).into_iter().collect();
        bin.sort_by(|a, b| (b.1, &a.0).cmp(&(a.1, &b.0)));
        let mut out = json!({
            // The bin: recently deleted synced photos, newest first, as [name, deleted at].
            "excluded": bin.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            "deleted_at": bin.iter().map(|(_, at)| at).collect::<Vec<_>>(),
            "deleted_for_good": store::load_excluded(cfg).len().saturating_sub(bin.len()),
            "bin_days": BIN_DAYS,
            "now": store::now_showing(cfg).map(|photo| json!({ "kind": photo.source(), "rel": photo })),
            "auto": settings.daily_sync,
            "pin": settings.protection.is_some(),
            "unlocked": auth::allowed(&settings, token.as_deref()),
        });
        if full {
            // [path, kind, taken, added]; "taken" falls back to when the photo arrived.
            out["photos"] = store::list_photos(cfg)
                .into_iter()
                .map(|p| json!([p.path, p.source, shared.index.get(&p.path).taken.unwrap_or(p.mtime), p.mtime]))
                .collect();
            out["disk"] = json!(store::disk_space());
            out["sources"] = json!(settings.albums);
        }
        out
    })
    .await;
    out["job"] = job;
    Json(out)
}

/// Storage, sync health and server facts for the status page.
async fn status(State(app): State<Shared>) -> Json<Value> {
    let running = app.job.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_running();
    let uptime = app.started.elapsed().as_secs();
    let shared = Arc::clone(&app);
    Json(
        blocking(move || {
            let cfg = &shared.cfg;
            let photos = store::list_photos(cfg);
            let settings = store::load_settings(cfg);
            let by_source: serde_json::Map<String, Value> = Source::ALL
                .into_iter()
                .map(|source| {
                    let from_it = photos.iter().filter(|p| p.source == source);
                    let totals = json!({ "count": from_it.clone().count(), "bytes": from_it.map(|p| p.size).sum::<u64>() });
                    (source.as_str().to_string(), totals)
                })
                .collect();
            let last = settings.last_sync.as_ref();
            let analysed = shared.index.analysed();
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "uptime_secs": uptime,
                "disk": store::disk_space(),
                "photos": { "count": photos.len(), "bytes": photos.iter().map(|p| p.size).sum::<u64>(), "by_source": by_source },
                "prepared": {
                    "thumbnails": store::thumbnail_count(cfg),
                    "fingerprints": analysed.fingerprints,
                    "quality": analysed.quality,
                    "people": analysed.people,
                    "face_models": faces::available(cfg),
                    "with_look_alikes": shared.with_look_alikes.load(Ordering::Relaxed),
                    "face_queue": shared.face_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).background_left(),
                },
                "sync": {
                    "running": running,
                    "albums": settings.albums.len(),
                    "last": last.map(|l| l.at),
                    "ok": last.map(|l| l.succeeded),
                    "summary": last.and_then(|l| l.summary.as_deref()),
                    "auto": settings.daily_sync,
                    "next": last.filter(|_| settings.daily_sync).map(|l| l.at + DAY),
                },
                "pin": settings.protection.is_some(),
                "excluded": store::load_excluded(cfg).len(),
            })
        })
        .await,
    )
}

/// Groups of photos that look alike, each with its best photo first.
/// `?d=` is how alike they must be (see `Likeness`).
async fn duplicates(State(app): State<Shared>, Query(query): Query<HashMap<String, String>>) -> Json<Value> {
    let likeness = Likeness::from_query(query.get("d").map(String::as_str));
    Json(
        blocking(move || {
            let photos = store::list_photos(&app.cfg);
            let mut pending = 0usize;
            // Oldest first, so each group reads in the order the shots were taken.
            let mut known: Vec<(u64, PhotoPath, u64)> = Vec::new();
            for photo in photos {
                let meta = app.index.get(&photo.path);
                pending += usize::from(meta.hash.is_none() || meta.quality.is_none());
                if let Some(hash) = meta.hash {
                    known.push((meta.taken.unwrap_or(photo.mtime), photo.path, hash));
                }
            }
            known.sort();
            let fingerprints: Vec<(PhotoPath, u64)> = known.into_iter().map(|(_, photo, hash)| (photo, hash)).collect();
            let groups = index::similar_groups(&fingerprints, likeness.bits());
            let ranked: Vec<Value> =
                groups.iter().take(MAX_DUPLICATE_GROUPS).map(|group| ranked_group(group, &app.index)).collect();
            json!({ "found": groups.len(), "groups": ranked, "pending": pending })
        })
        .await,
    )
}

/// One group of look-alikes for the page: best photo first, with what is known about each.
fn ranked_group(group: &[PhotoPath], index: &Index) -> Value {
    let known: Vec<index::Meta> = group.iter().map(|photo| index.get(photo)).collect();
    let ranking = index::rank(&known);
    // Best first; a photo not measured yet keeps its place after the measured ones.
    let mut order: Vec<usize> = (0..group.len()).collect();
    order.sort_by(|&a, &b| ranking.scores[b].partial_cmp(&ranking.scores[a]).unwrap_or(std::cmp::Ordering::Equal));
    let photos: Vec<&PhotoPath> = order.iter().map(|&i| &group[i]).collect();
    let quality: Vec<Value> =
        order.iter().map(|&i| json!(known[i].quality.map(|q| [q.sharpness, u32::from(q.clipped)]))).collect();
    // Per photo: faces found, and the average eyes-open and smiling percentages.
    let people: Vec<Value> = order
        .iter()
        .map(|&i| {
            json!(known[i].people.map(|p| {
                let faces = u16::from(p.faces).max(1);
                [u16::from(p.faces), p.eyes_open / faces, p.smiling / faces]
            }))
        })
        .collect();
    json!({
        "photos": photos,
        "ranked": ranking.scores.iter().all(Option::is_some),
        "by": if ranking.by_expressions { "expressions" } else { "sharpness" },
        "quality": quality,
        "people": people,
    })
}

/// Look at the faces in the photos of one group, then say how they rank. This is done when
/// someone opens a group, not ahead of time: each photo takes a few seconds on the frame.
async fn analyse(State(app): State<Shared>, Parsed(request): Parsed<AnalyseRequest>) -> ApiResult {
    let group = request.paths.into_inner();
    let cfg = Arc::clone(&app.cfg);
    if !blocking(move || faces::available(&cfg)).await {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "the face models are not installed on the frame"));
    }
    let started = Instant::now();
    let limit = Arc::new(tokio::sync::Semaphore::new(ANALYSE_IN_PARALLEL));
    let mut tasks = tokio::task::JoinSet::new();
    for photo in group.clone() {
        let permit = Arc::clone(&limit).acquire_owned().await.expect("semaphore is never closed");
        let shared = Arc::clone(&app);
        tasks.spawn_blocking(move || {
            let _permit = permit;
            let models = shared.models.get(&shared.cfg).map_err(|e| store::PhotoError::Analysis(e.to_string()))?;
            index::look_at_people(&shared.cfg, &shared.index, &photo, &models)
        });
    }
    let mut failed = 0usize;
    while let Some(outcome) = tasks.join_next().await {
        match outcome {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                failed += 1;
                debug!(error = %e, "could not look at the faces in a photo");
            }
            Err(e) => {
                failed += 1;
                warn!(error = %e, "face analysis task failed");
            }
        }
    }
    let shared = Arc::clone(&app);
    let photos = group.len();
    let ranked = blocking(move || {
        shared.index.save();
        ranked_group(&group, &shared.index)
    })
    .await;
    info!(photos, failed, ms = started.elapsed().as_millis(), "faces analysed");
    Ok(Json(json!({ "group": ranked, "failed": failed })))
}

/// The page reports which groups are on screen. Their photos move to the front of the face
/// queue, and the reply says how each group ranks with what is known so far.
async fn watching(State(app): State<Shared>, Parsed(request): Parsed<WatchingRequest>) -> Json<Value> {
    let on_screen: Vec<PhotoPath> = request.groups.iter().flat_map(|group| group.iter().cloned()).collect();
    app.face_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).watch(on_screen);
    let ranked: Vec<Value> = request.groups.iter().map(|group| ranked_group(group, &app.index)).collect();
    Json(json!({ "groups": ranked }))
}

/// Work out which photos' faces are worth looking at in the background, and queue them.
async fn plan_face_checks(app: &Shared) {
    let shared = Arc::clone(app);
    let (todo, total) = blocking(move || index::people_to_look_at(&shared.cfg, &shared.index)).await;
    app.with_look_alikes.store(total, Ordering::Relaxed);
    debug!(todo = todo.len(), total, "planned background face checks");
    app.face_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).refill(todo);
}

/// Look at the faces in the next queued photo. Returns how long to rest before the next
/// one, or None when the queue is empty.
async fn check_next_face(app: &Shared) -> Option<Duration> {
    let next = {
        let mut queue = app.face_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.next(|photo| app.index.get(photo).people.is_some())
    }?;
    let (photo, pause) = match next {
        faces::Next::Watched(photo) => (photo, Duration::ZERO),
        faces::Next::Background(photo) => (photo, BACKGROUND_PAUSE),
    };
    let shared = Arc::clone(app);
    let outcome = blocking(move || {
        let models = shared.models.get(&shared.cfg).map_err(|e| store::PhotoError::Analysis(e.to_string()))?;
        let looked = index::look_at_people(&shared.cfg, &shared.index, &photo, &models);
        if looked.is_err() {
            // Do not come back to a photo that cannot be read: record that nobody was found.
            shared.index.set_people(&photo, faces::People::default());
        }
        looked.map(|_| ())
    })
    .await;
    if let Err(e) = outcome {
        debug!(error = %e, "could not look at the faces in a photo");
    }
    Some(pause)
}

/// Look at faces in the background, one photo at a time: first whatever is on someone's
/// screen, then, with a rest between photos, the other photos that have look-alikes.
async fn look_at_faces(app: Shared) {
    let mut planned: Option<Instant> = None;
    let mut since_save = 0u32;
    let save = |app: &Shared| {
        let shared = Arc::clone(app);
        blocking(move || shared.index.save())
    };
    loop {
        let cfg = Arc::clone(&app.cfg);
        if !blocking(move || faces::available(&cfg)).await {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        if planned.is_none_or(|at| at.elapsed() >= REPLAN_EVERY) {
            plan_face_checks(&app).await;
            planned = Some(Instant::now());
        }
        let Some(pause) = check_next_face(&app).await else {
            if since_save > 0 {
                save(&app).await;
                since_save = 0;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        since_save += 1;
        if since_save >= 25 {
            save(&app).await;
            since_save = 0;
        }
        tokio::time::sleep(pause).await;
    }
}

/// Give back the memory the face models use once nobody has compared photos for a while.
async fn unload_idle_models(app: Shared) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        if app.models.unload_if_idle(MODELS_IDLE) {
            info!("face models unloaded after being idle");
        }
    }
}

/// The photo a request asks for. Anything that is not a valid photo path simply is not there.
fn photo_param(query: &HashMap<String, String>) -> ApiResult<PhotoPath> {
    query.get("p").and_then(|rel| PhotoPath::parse(rel)).ok_or_else(ApiError::not_found)
}

fn image_reply(data: Vec<u8>, content_type: &'static str) -> Response {
    // A photo's name never refers to different content, so browsers may keep it for good.
    ([(CONTENT_TYPE, HeaderValue::from_static(content_type)), (CACHE_CONTROL, KEEP_FOREVER)], data).into_response()
}

async fn file(State(app): State<Shared>, Query(query): Query<HashMap<String, String>>) -> ApiResult<Response> {
    let photo = photo_param(&query)?;
    let data = tokio::fs::read(photo.within(&app.cfg.cloud_dir))
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "that photo is no longer on the frame"))?;
    Ok(image_reply(data, content_type(&photo)))
}

/// A small preview; if one cannot be made, the photo itself.
async fn thumb(State(app): State<Shared>, Query(query): Query<HashMap<String, String>>) -> ApiResult<Response> {
    let photo = photo_param(&query)?;
    let cfg = Arc::clone(&app.cfg);
    match blocking(move || store::thumbnail(&cfg, &photo)).await {
        Ok(data) => Ok(image_reply(data, "image/jpeg")),
        Err(_) => file(State(app), Query(query)).await,
    }
}

// --- changing --------------------------------------------------------------------------------

async fn add(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let name = header(&headers, "x-filename").map_or_else(|| "photo".into(), percent_decode);
    // The browser tells us when the file was last modified, which for camera photos is when it was taken.
    let now = now_secs();
    let taken = header(&headers, "x-taken").and_then(|t| t.parse::<u64>().ok()).filter(|t| (1..=now + DAY).contains(t));
    let (shared, bytes) = (Arc::clone(&app), body.len());
    let photo = blocking(move || {
        let photo = store::add(&shared.cfg, &body)?;
        shared.index.set_taken(&photo, taken.unwrap_or(now));
        if let Err(e) = index::prepare(&shared.cfg, &shared.index, &photo) {
            debug!(%photo, error = %e, "could not prepare the new photo yet");
        }
        Ok::<_, store::PhotoError>(photo)
    })
    .await
    .map_err(|e| ApiError::bad_request(format!("{name}: {e}")))?;
    info!(file = %name, %photo, bytes, "photo added");
    Ok(Json(json!({ "rel": photo })))
}

async fn delete(State(app): State<Shared>, Parsed(request): Parsed<DeleteRequest>) -> Json<Value> {
    let photos = request.paths.into_inner();
    let count = photos.len();
    blocking(move || {
        store::delete(&app.cfg, &photos);
        app.index.forget(&photos);
        app.index.save();
        let paths = photos.iter().map(PhotoPath::as_str).collect::<Vec<_>>().join(" ");
        info!(count = photos.len(), paths, "photos deleted");
    })
    .await;
    Json(json!({ "deleted": count }))
}

/// Let the next sync bring back photos from the bin: the named ones, or all of them.
async fn restore(State(app): State<Shared>, Parsed(request): Parsed<RestoreRequest>) -> Json<Value> {
    Json(json!({ "restored": blocking(move || store::restore(&app.cfg, &request.names)).await }))
}

/// Empty the bin now: those photos stay off the frame and leave the list of deleted photos.
async fn empty_bin(State(app): State<Shared>) -> Json<Value> {
    Json(json!({ "emptied": blocking(move || store::empty_bin(&app.cfg, None)).await }))
}

/// Allow photos that were deleted for good to be downloaded again by a sync.
async fn forget_emptied(State(app): State<Shared>) -> Json<Value> {
    let forgotten = blocking(move || store::forget_emptied(&app.cfg)).await;
    info!(forgotten, "permanently deleted photos may be synced again");
    Json(json!({ "forgotten": forgotten }))
}

/// Once a day, empty photos from the bin that were deleted long enough ago.
async fn empty_old_bin_entries(app: Shared) {
    loop {
        let cfg = Arc::clone(&app.cfg);
        blocking(move || store::empty_bin(&cfg, Some(BIN_DAYS * DAY))).await;
        tokio::time::sleep(Duration::from_secs(DAY)).await;
    }
}

async fn save_albums(State(app): State<Shared>, Parsed(request): Parsed<AlbumsRequest>) -> Json<Value> {
    let albums = request.sources.0;
    let saved = albums.clone();
    blocking(move || app.update_settings(|settings| settings.albums = saved)).await;
    info!(albums = albums.len(), "album links saved");
    Json(json!({ "sources": albums }))
}

async fn set_daily_sync(State(app): State<Shared>, Parsed(request): Parsed<DailySyncRequest>) -> Json<Value> {
    let enabled = request.enabled;
    blocking(move || app.update_settings(|settings| settings.daily_sync = enabled)).await;
    info!(enabled, "daily sync setting changed");
    Json(json!({ "auto": enabled }))
}

async fn refresh_slideshow() -> Json<Value> {
    Json(json!({ "closed": blocking(store::refresh_slideshow).await }))
}

/// Start a sync in the background. Returns false if one is already running.
fn start_sync(app: &Shared, options: gphotos::Options) -> bool {
    if !gphotos::try_start(&app.job) {
        return false;
    }
    let app = Arc::clone(app);
    tokio::spawn(async move {
        gphotos::run(app.sync_context(), options).await;
        let last_sync = {
            let job = app.job.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            LastSync { at: now_secs(), succeeded: job.phase == Phase::Finished(Ok(())), summary: job.summary() }
        };
        blocking(move || app.update_settings(|settings| settings.last_sync = Some(last_sync))).await;
    });
    true
}

async fn sync(State(app): State<Shared>, Parsed(request): Parsed<SyncRequest>) -> ApiResult {
    let albums = request.sources.0;
    if albums.is_empty() {
        return Err(ApiError::bad_request("add at least one Google Photos shared-album link (https://...)"));
    }
    let (shared, saved) = (Arc::clone(&app), albums.clone());
    blocking(move || shared.update_settings(|settings| settings.albums = saved)).await;
    let options = gphotos::Options { albums, delete: request.delete, refresh: request.refresh, limit: request.limit };
    if start_sync(&app, options) {
        Ok(Json(json!({ "started": true })))
    } else {
        Err(ApiError::new(StatusCode::CONFLICT, "a sync is already running"))
    }
}

/// Exchange the PIN for a session token.
async fn unlock(State(app): State<Shared>, Parsed(request): Parsed<UnlockRequest>) -> ApiResult {
    let outcome = blocking(move || app.update_settings(|settings| app.guard.unlock(settings, &request.pin))).await;
    match outcome {
        Ok(token) => Ok(Json(json!({ "token": token }))),
        Err(auth::Denied::WrongPin { attempts_left }) => {
            let tries = if attempts_left == 1 { "1 try".to_string() } else { format!("{attempts_left} tries") };
            Err(ApiError::new(StatusCode::FORBIDDEN, format!("Wrong PIN. {tries} left.")))
        }
        Err(auth::Denied::TooManyAttempts { wait_secs }) => Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many wrong PINs. Try again in {wait_secs} seconds."),
        )),
        Err(auth::Denied::NoPinSet) => Err(ApiError::bad_request("No PIN is set.")),
    }
}

/// Set, change or remove the PIN. `protect` has already checked the caller may.
async fn change_pin(State(app): State<Shared>, Parsed(request): Parsed<PinRequest>) -> Json<Value> {
    match request.pin {
        PinChange::Remove => {
            blocking(move || app.update_settings(auth::clear_pin)).await;
            info!("PIN removed");
            Json(json!({ "pin": false }))
        }
        PinChange::Set(pin) => {
            let token = blocking(move || app.update_settings(|settings| auth::set_pin(settings, &pin))).await;
            info!("PIN set");
            Json(json!({ "pin": true, "token": token }))
        }
    }
}

// --- wiring -------------------------------------------------------------------------------------

fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/logs", get(logs))
        .route("/file", get(file))
        .route("/thumb", get(thumb))
        .route("/api/state", get(state))
        .route("/api/status", get(status))
        .route("/api/duplicates", get(duplicates))
        .route("/api/analyse", post(analyse))
        .route("/api/watching", post(watching))
        .route("/api/add", post(add))
        .route("/api/delete", post(delete))
        .route("/api/restore", post(restore))
        .route("/api/empty-bin", post(empty_bin))
        .route("/api/forget-emptied", post(forget_emptied))
        .route("/api/sources", post(save_albums))
        .route("/api/auto", post(set_daily_sync))
        .route("/api/sync", post(sync))
        .route("/api/refresh-slideshow", post(refresh_slideshow))
        .route("/api/unlock", post(unlock))
        .route("/api/pin", post(change_pin))
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(Arc::clone(&app), protect))
        .layer(middleware::from_fn(log_requests))
        .with_state(app)
}

/// The sync to start now, if the user wants daily syncs and the last one was a day or more ago.
fn daily_sync_due(settings: Settings, now: u64) -> Option<gphotos::Options> {
    let last = settings.last_sync.map_or(0, |last| last.at);
    let due = settings.daily_sync && now.saturating_sub(last) >= DAY && !settings.albums.is_empty();
    due.then_some(gphotos::Options { albums: settings.albums, delete: false, refresh: true, limit: None })
}

/// Once a day, if the user asked for it, sync the saved albums.
async fn auto_sync(app: Shared) {
    loop {
        tokio::time::sleep(Duration::from_secs(600)).await;
        let cfg = Arc::clone(&app.cfg);
        let settings = blocking(move || store::load_settings(&cfg)).await;
        if let Some(options) = daily_sync_due(settings, now_secs()) {
            info!(albums = options.albums.len(), "starting the daily album sync");
            start_sync(&app, options);
        }
    }
}

/// Log to stderr and to daily files in `<data dir>/logs` (a week is kept).
/// `FRAME_LOG` sets the level: error, warn, info (default), debug or trace.
fn init_logging(cfg: &Config) {
    let level = std::env::var("FRAME_LOG").ok().and_then(|l| l.parse::<Level>().ok()).unwrap_or(Level::INFO);
    let builder = tracing_subscriber::fmt().with_max_level(level).with_ansi(false);
    let files = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("frameserver")
        .filename_suffix("log")
        .max_log_files(7)
        .build(store::log_dir(cfg));
    match files {
        Ok(files) => builder.with_writer(std::io::stderr.and(files)).init(),
        Err(e) => {
            builder.with_writer(std::io::stderr).init();
            warn!(error = %e, "cannot write log files; logging to stderr only");
        }
    }
}

#[tokio::main]
async fn main() {
    let env_path = |name: &str, default: &str| PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.into()));
    let cfg = Config {
        cloud_dir: env_path("FRAME_CLOUD_DIR", "/storage/emulated/0/CloudAlbum/cloud"),
        data_dir: env_path("FRAME_DATA_DIR", "/data/local/frameserver"),
    };
    let port = std::env::var("FRAME_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080u16);
    if let Err(e) = std::fs::create_dir_all(cfg.data_dir.join("thumbs")) {
        eprintln!("cannot create {}: {e}", cfg.data_dir.display());
        std::process::exit(1);
    }
    init_logging(&cfg);

    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(listener) => listener,
        Err(e) => {
            error!(port, error = %e, "cannot listen");
            std::process::exit(1);
        }
    };
    let app = App::new(cfg);
    info!(
        port,
        photos = %app.cfg.cloud_dir.display(),
        data = %app.cfg.data_dir.display(),
        version = env!("CARGO_PKG_VERSION"),
        "frameserver listening"
    );
    tokio::spawn(index::maintain(Arc::clone(&app.cfg), Arc::clone(&app.index)));
    tokio::spawn(auto_sync(Arc::clone(&app)));
    tokio::spawn(unload_idle_models(Arc::clone(&app)));
    tokio::spawn(look_at_faces(Arc::clone(&app)));
    tokio::spawn(empty_old_bin_entries(Arc::clone(&app)));
    if let Err(e) = axum::serve(listener, router(app)).await {
        error!(error = %e, "the server stopped");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
