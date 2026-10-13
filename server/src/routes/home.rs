//! The house: what the relay tells the frame, and what the frame's pages ask about it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use tracing::info;

use crate::app::{blocking, now_secs, Moment, Shared};
use crate::auth;
use crate::home::{PictureError, Report};
use crate::routes::{ApiError, ApiResult, Parsed, NO_STORE};
use crate::types::{CameraId, HomeKey};

/// When the doorbell rings the screen is switched on, and stays on this long.
const ON_AFTER_A_RING: Duration = Duration::from_mins(2);

/// The camera a request names. Anything that is not the name of a camera simply is not there.
fn camera_param(query: &HashMap<String, String>) -> ApiResult<CameraId> {
    query.get("camera").and_then(|id| CameraId::parse(id)).ok_or_else(ApiError::not_found)
}

/// What the house is doing, for the frame's screen.
pub(crate) async fn home_view(State(app): State<Shared>) -> Json<Value> {
    Json(app.home.view())
}

/// A camera's latest picture.
pub(crate) async fn home_picture(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let (bytes, kind) = app.home.picture(&camera_param(&query)?).ok_or_else(ApiError::not_found)?;
    Ok(([(CONTENT_TYPE, HeaderValue::from_static(kind)), (CACHE_CONTROL, NO_STORE)], bytes).into_response())
}

/// The relay says what the house is doing. `protect` has checked its key. If the doorbell
/// has just started ringing, the screen is switched on so that whoever is there can be seen.
pub(crate) async fn home_report(State(app): State<Shared>, Parsed(report): Parsed<Report>) -> Json<Value> {
    let rings = app.home.hear(report, now_secs());
    if rings {
        info!("the doorbell is ringing");
        let now = Moment::now();
        {
            let mut watch = app.watch();
            let until = now + ON_AFTER_A_RING;
            watch.stay_on_until = Some(watch.stay_on_until.map_or(until, |already| already.max(until)));
        }
        let device = app.device;
        let switched_on = blocking(move || device.power().0 == Some(false) && device.switch_screen(true)).await;
        if switched_on {
            let mut watch = app.watch();
            watch.asleep = false;
            watch.heard = Some(now);
            watch.shown = watch.shown.map(|_| now);
        }
    }
    // Which pictures the frame lacks, so the relay can send them (all of them after a restart here).
    Json(json!({ "heard": true, "rings": rings, "wanted": app.home.without_a_picture() }))
}

/// The relay sends a camera's latest picture. `protect` has checked its key.
pub(crate) async fn keep_home_picture(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult {
    let camera = camera_param(&query)?;
    app.home.keep_picture(camera, body, now_secs()).map_err(|e| {
        let status = if e == PictureError::UnknownCamera { StatusCode::NOT_FOUND } else { StatusCode::BAD_REQUEST };
        ApiError::new(status, e.to_string())
    })?;
    Ok(Json(json!({ "kept": true })))
}

/// Make a new key for the house's relay, replacing any there was, and hand it over this
/// once: it is to be copied to the machine the relay runs on.
pub(crate) async fn new_home_key(State(app): State<Shared>) -> ApiResult {
    let key = HomeKey::parse(&auth::random_hex())
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "could not make a key"))?;
    let (shared, kept) = (Arc::clone(&app), key.clone());
    blocking(move || shared.update_settings(|settings| settings.home_key = Some(kept))).await;
    info!("a key for the house's relay was made");
    Ok(Json(json!({ "key": key })))
}

/// Disconnect the house: the key stops working and what was known is forgotten.
pub(crate) async fn forget_home(State(app): State<Shared>) -> Json<Value> {
    let shared = Arc::clone(&app);
    blocking(move || shared.update_settings(|settings| settings.home_key = None)).await;
    app.home.forget();
    info!("the house was disconnected");
    Json(json!({ "home": false }))
}
