//! The Yoto players: how much listening time they have left, for the frame's screen, what a
//! grown-up asks of them there (more time or less, a pause), and the key for Yoto Limiter.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::app::{blocking, Shared};
use crate::routes::{ApiError, ApiResult, Parsed};
use crate::store;
use crate::types::{YotoKey, YotoOrder};

#[derive(Deserialize)]
pub(crate) struct YotoKeyRequest {
    /// The key, or null to stop reading Yoto Limiter.
    pub(crate) key: Option<YotoKey>,
}

/// The players and their time today, and whether a key is kept at all.
pub(crate) async fn yoto_view(State(app): State<Shared>) -> Json<Value> {
    let cfg = Arc::clone(&app.cfg);
    let set_up = blocking(move || store::load_settings(&cfg).yoto_key.is_some()).await;
    let mut view = app.yoto.view();
    view["set_up"] = json!(set_up);
    Json(view)
}

#[derive(Deserialize)]
pub(crate) struct YotoOrderRequest {
    pub(crate) player: i64,
    pub(crate) order: YotoOrder,
}

/// Ask something of a player: more time today or less, or a pause. Only with a PIN set: without
/// one, anyone at the frame could do it, the child whose time it is included. (With a PIN set,
/// the request has already shown it was unlocked with it; see `protect`.)
pub(crate) async fn yoto_order(State(app): State<Shared>, Parsed(request): Parsed<YotoOrderRequest>) -> ApiResult {
    let cfg = Arc::clone(&app.cfg);
    let settings = blocking(move || store::load_settings(&cfg)).await;
    if settings.protection.is_none() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Set a PIN for the frame first, so that only grown-ups can change the Yoto players",
        ));
    }
    let key = settings.yoto_key.ok_or_else(|| ApiError::bad_request("Yoto Limiter is not set up"))?;
    app.yoto
        .order(&app.http, &key, request.player, request.order)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, capitalised(&e.to_string())))?;
    info!(player = request.player, order = ?request.order, "a Yoto player was asked to change");
    let mut view = app.yoto.view();
    view["set_up"] = json!(true);
    Ok(Json(view))
}

/// Keep a key for Yoto Limiter, once it has been tried and works, or with null forget it.
/// It is never handed back.
pub(crate) async fn save_yoto_key(State(app): State<Shared>, Parsed(request): Parsed<YotoKeyRequest>) -> ApiResult {
    if let Some(key) = &request.key {
        app.yoto
            .check(&app.http, key)
            .await
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, capitalised(&e.to_string())))?;
    }
    let set_up = request.key.is_some();
    let shared = Arc::clone(&app);
    blocking(move || shared.update_settings(|settings| settings.yoto_key = request.key)).await;
    if !set_up {
        app.yoto.forget();
    }
    app.yoto.key_changed();
    info!(set_up, "the Yoto Limiter key was changed");
    let mut view = app.yoto.view();
    view["set_up"] = json!(set_up);
    Ok(Json(view))
}

fn capitalised(text: &str) -> String {
    let mut letters = text.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
}
