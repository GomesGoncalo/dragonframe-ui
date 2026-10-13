//! Photos: listing them, serving them, adding and deleting, the bin, look-alikes, and
//! syncing Google Photos albums.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::app::{blocking, now_secs, Shared, DAY};
use crate::gphotos::Phase;
use crate::index::Index;
use crate::routes::{header, ApiError, ApiResult, Parsed, KEEP_FOREVER};
use crate::store::LastSync;
use crate::types::{percent_decode, AlbumLinks, Bounded, Likeness, NonEmpty, PhotoPath, Restore, Source, SyncedName};
use crate::{auth, faces, gphotos, index, store};

/// Deleted synced photos can be restored for this long; after that they leave the bin for good.
pub(crate) const BIN_DAYS: u64 = 30;
const MAX_DUPLICATE_GROUPS: usize = 500;
/// Most photos one request may ask to have their faces looked at (each takes seconds on the frame).
pub(crate) const MAX_ANALYSE: usize = 40;
const ANALYSE_IN_PARALLEL: usize = 2;
/// Most groups the page may report as being on screen at once.
pub(crate) const MAX_WATCHED_GROUPS: usize = 12;

#[derive(Deserialize)]
pub(crate) struct DeleteRequest {
    pub(crate) paths: NonEmpty<PhotoPath>,
}

/// The photos of one group of look-alikes.
pub(crate) type Group = Bounded<PhotoPath, 1, MAX_ANALYSE>;

#[derive(Deserialize)]
pub(crate) struct AnalyseRequest {
    pub(crate) paths: Group,
}

#[derive(Deserialize)]
pub(crate) struct WatchingRequest {
    pub(crate) groups: Bounded<Group, 0, MAX_WATCHED_GROUPS>,
}

#[derive(Deserialize)]
pub(crate) struct RestoreRequest {
    pub(crate) names: Restore,
}

#[derive(Deserialize)]
pub(crate) struct AlbumsRequest {
    pub(crate) sources: AlbumLinks,
}

#[derive(Deserialize)]
pub(crate) struct DailySyncRequest {
    #[serde(default)]
    pub(crate) enabled: bool,
}

#[derive(Deserialize)]
pub(crate) struct SyncRequest {
    pub(crate) sources: AlbumLinks,
    #[serde(default)]
    pub(crate) delete: bool,
    #[serde(default)]
    pub(crate) refresh: bool,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

pub(crate) fn content_type(photo: &PhotoPath) -> &'static str {
    match photo.as_str().rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

/// What the page polls: the running job, what is on screen, and (with `?full`) every photo.
pub(crate) async fn state(
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

/// The photos for the frame's own screen, as [path, taken, width, height]. Width and height
/// are null until the photo has been measured.
pub(crate) async fn slides(State(app): State<Shared>) -> Json<Value> {
    Json(
        blocking(move || {
            let photos: Vec<Value> = store::list_photos(&app.cfg)
                .into_iter()
                .map(|photo| {
                    let meta = app.index.get(&photo.path);
                    let (width, height) = meta.size.unzip();
                    json!([photo.path, meta.taken.unwrap_or(photo.mtime), width, height])
                })
                .collect();
            json!({ "photos": photos })
        })
        .await,
    )
}

/// Storage, sync health and server facts for the status page.
pub(crate) async fn status(State(app): State<Shared>) -> Json<Value> {
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
pub(crate) async fn duplicates(State(app): State<Shared>, Query(query): Query<HashMap<String, String>>) -> Json<Value> {
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
pub(crate) fn ranked_group(group: &[PhotoPath], index: &Index) -> Value {
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
pub(crate) async fn analyse(State(app): State<Shared>, Parsed(request): Parsed<AnalyseRequest>) -> ApiResult {
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
pub(crate) async fn watching(State(app): State<Shared>, Parsed(request): Parsed<WatchingRequest>) -> Json<Value> {
    let on_screen: Vec<PhotoPath> = request.groups.iter().flat_map(|group| group.iter().cloned()).collect();
    app.face_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).watch(on_screen);
    let ranked: Vec<Value> = request.groups.iter().map(|group| ranked_group(group, &app.index)).collect();
    Json(json!({ "groups": ranked }))
}

/// The photo a request asks for. Anything that is not a valid photo path simply is not there.
pub(crate) fn photo_param(query: &HashMap<String, String>) -> ApiResult<PhotoPath> {
    query.get("p").and_then(|rel| PhotoPath::parse(rel)).ok_or_else(ApiError::not_found)
}

pub(crate) fn image_reply(data: Vec<u8>, content_type: &'static str) -> Response {
    // A photo's name never refers to different content, so browsers may keep it for good.
    ([(CONTENT_TYPE, HeaderValue::from_static(content_type)), (CACHE_CONTROL, KEEP_FOREVER)], data).into_response()
}

pub(crate) async fn file(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let photo = photo_param(&query)?;
    let data = tokio::fs::read(photo.within(&app.cfg.cloud_dir))
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "that photo is no longer on the frame"))?;
    Ok(image_reply(data, content_type(&photo)))
}

/// A small preview; if one cannot be made, the photo itself.
pub(crate) async fn thumb(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let photo = photo_param(&query)?;
    let cfg = Arc::clone(&app.cfg);
    match blocking(move || store::thumbnail(&cfg, &photo)).await {
        Ok(data) => Ok(image_reply(data, "image/jpeg")),
        Err(_) => file(State(app), Query(query)).await,
    }
}

pub(crate) async fn add(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> ApiResult {
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

pub(crate) async fn delete(State(app): State<Shared>, Parsed(request): Parsed<DeleteRequest>) -> Json<Value> {
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
pub(crate) async fn restore(State(app): State<Shared>, Parsed(request): Parsed<RestoreRequest>) -> Json<Value> {
    Json(json!({ "restored": blocking(move || store::restore(&app.cfg, &request.names)).await }))
}

/// Empty the bin now: those photos stay off the frame and leave the list of deleted photos.
pub(crate) async fn empty_bin(State(app): State<Shared>) -> Json<Value> {
    Json(json!({ "emptied": blocking(move || store::empty_bin(&app.cfg, None)).await }))
}

/// Allow photos that were deleted for good to be downloaded again by a sync.
pub(crate) async fn forget_emptied(State(app): State<Shared>) -> Json<Value> {
    let forgotten = blocking(move || store::forget_emptied(&app.cfg)).await;
    info!(forgotten, "permanently deleted photos may be synced again");
    Json(json!({ "forgotten": forgotten }))
}

pub(crate) async fn save_albums(State(app): State<Shared>, Parsed(request): Parsed<AlbumsRequest>) -> Json<Value> {
    let albums = request.sources.0;
    let saved = albums.clone();
    blocking(move || app.update_settings(|settings| settings.albums = saved)).await;
    info!(albums = albums.len(), "album links saved");
    Json(json!({ "sources": albums }))
}

pub(crate) async fn set_daily_sync(
    State(app): State<Shared>,
    Parsed(request): Parsed<DailySyncRequest>,
) -> Json<Value> {
    let enabled = request.enabled;
    blocking(move || app.update_settings(|settings| settings.daily_sync = enabled)).await;
    info!(enabled, "daily sync setting changed");
    Json(json!({ "auto": enabled }))
}

pub(crate) async fn refresh_slideshow() -> Json<Value> {
    Json(json!({ "closed": blocking(store::refresh_slideshow).await }))
}

/// Start a sync in the background. Returns false if one is already running.
pub(crate) fn start_sync(app: &Shared, options: gphotos::Options) -> bool {
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

pub(crate) async fn sync(State(app): State<Shared>, Parsed(request): Parsed<SyncRequest>) -> ApiResult {
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
