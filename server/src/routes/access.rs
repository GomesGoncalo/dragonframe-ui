//! The PIN: unlocking with it, locking again, and setting, changing or removing it.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::app::{blocking, Shared};
use crate::auth;
use crate::routes::{header, ApiError, ApiResult, Parsed};
use crate::types::PinChange;

#[derive(Deserialize)]
pub(crate) struct UnlockRequest {
    /// Whatever was typed: a wrong guess need not even look like a PIN.
    #[serde(default)]
    pub(crate) pin: String,
}

#[derive(Deserialize)]
pub(crate) struct PinRequest {
    pub(crate) pin: PinChange,
}

/// Exchange the PIN for a session token.
pub(crate) async fn unlock(State(app): State<Shared>, Parsed(request): Parsed<UnlockRequest>) -> ApiResult {
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

/// End the caller's session: its token unlocks nothing after this.
pub(crate) async fn lock(State(app): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let token = header(&headers, "x-frame-token").map(str::to_string);
    let ended = blocking(move || {
        app.update_settings(|settings| token.is_some_and(|token| auth::end_session(settings, &token)))
    })
    .await;
    Json(json!({ "locked": ended }))
}

/// Set, change or remove the PIN. `protect` has already checked the caller may.
pub(crate) async fn change_pin(State(app): State<Shared>, Parsed(request): Parsed<PinRequest>) -> Json<Value> {
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
