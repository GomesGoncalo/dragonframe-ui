//! The Yoto players: how much listening time they have left, for the frame's screen, and the
//! key the frame reads Yoto Limiter with.

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
use crate::types::YotoKey;

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
