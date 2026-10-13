//! The server's routes: which address does what, who may make changes, and how a
//! request that cannot be honoured is answered.
//!
//! Requests are parsed into types where they arrive (see `types`): a handler that takes a
//! `DeleteRequest` already holds valid photo paths and has nothing left to check.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, HOST, ORIGIN, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::app::{blocking, Shared};
use crate::routes::access::{change_pin, lock, unlock};
use crate::routes::calendars::{add_event, calendar_view, delete_event, save_calendars, save_reminders};
use crate::routes::frame::{
    address_code, alive, device_view, forget_place, open_wifi_settings, set_chime, set_night, set_place, set_screen,
    set_slideshow, weather_view,
};
use crate::routes::home::{forget_home, home_picture, home_report, home_view, keep_home_picture, new_home_key};
use crate::routes::pages::{frame_screen, logs, page, simulator};
use crate::routes::photos::{
    add, analyse, delete, duplicates, empty_bin, file, forget_emptied, mark, refresh_slideshow, restore, save_albums,
    set_daily_sync, slides, state, status, sync, thumb, watching,
};
use crate::{auth, store};

pub(crate) mod access;
pub(crate) mod calendars;
pub(crate) mod frame;
pub(crate) mod home;
pub(crate) mod pages;
pub(crate) mod photos;

pub(crate) const MAX_BODY: usize = 60 * 1024 * 1024;
pub(crate) const NO_STORE: HeaderValue = HeaderValue::from_static("no-store");
pub(crate) const KEEP_FOREVER: HeaderValue = HeaderValue::from_static("private, max-age=31536000, immutable");

/// An error reply: a status and a message the page can show.
#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
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

pub(crate) type ApiResult<T = Json<Value>> = Result<T, ApiError>;

pub(crate) fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A request body parsed into `T`. A body that does not parse is answered with 400 and the
/// reason, so a handler never sees a request it would have to validate.
pub(crate) struct Parsed<T>(pub(crate) T);

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

/// Pages on other websites must not be able to drive this app through a
/// visitor's browser: changes need a header browsers only send same-origin,
/// and the Origin, when present, has to be us.
pub(crate) fn same_origin(headers: &HeaderMap) -> bool {
    let host = header(headers, HOST.as_str()).unwrap_or("");
    let origin_ok = header(headers, ORIGIN.as_str()).is_none_or(|origin| origin.split("://").nth(1) == Some(host));
    origin_ok && headers.contains_key("x-frame-app")
}

/// Every change (POST) must come from our own page and, when a PIN is set, from an unlocked session.
/// What the house's relay sends. It is a program on another machine, not our page in a
/// browser, so it proves itself with its own key rather than with the page's header and the PIN.
fn from_the_relay(path: &str) -> bool {
    matches!(path, "/api/home/report" | "/api/home/picture")
}

pub(crate) async fn protect(State(app): State<Shared>, request: Request, next: Next) -> Response {
    if request.method() == Method::POST && from_the_relay(request.uri().path()) {
        let offered = header(request.headers(), "x-home-key").unwrap_or("").to_string();
        let cfg = Arc::clone(&app.cfg);
        let known =
            blocking(move || store::load_settings(&cfg).home_key.is_some_and(|key| key.matches(&offered))).await;
        if !known {
            return ApiError::new(StatusCode::UNAUTHORIZED, "the relay's key is not the one this frame gave out")
                .into_response();
        }
    } else if request.method() == Method::POST {
        if !same_origin(request.headers()) {
            return ApiError::new(StatusCode::FORBIDDEN, "forbidden").into_response();
        }
        // Unlocking is how a session starts, and analysing photos changes nothing on the frame.
        // Everything else needs the PIN, from the frame's own screen as much as from a phone.
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

pub(crate) async fn log_requests(request: Request, next: Next) -> Response {
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

pub(crate) fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/frame", get(frame_screen))
        .route("/simulator", get(simulator))
        .route("/logs", get(logs))
        .route("/file", get(file))
        .route("/thumb", get(thumb))
        .route("/api/state", get(state))
        .route("/api/status", get(status))
        .route("/api/slides", get(slides))
        .route("/api/calendar", get(calendar_view))
        .route("/api/calendars", post(save_calendars))
        .route("/api/events/add", post(add_event))
        .route("/api/events/delete", post(delete_event))
        .route("/api/reminders", post(save_reminders))
        .route("/api/device", get(device_view))
        .route("/api/address.svg", get(address_code))
        .route("/api/alive", get(alive))
        .route("/api/screen", post(set_screen))
        .route("/api/slideshow", post(set_slideshow))
        .route("/api/wifi/settings", post(open_wifi_settings))
        .route("/api/night", post(set_night))
        .route("/api/chime", post(set_chime))
        .route("/api/weather", get(weather_view))
        .route("/api/weather/place", post(set_place))
        .route("/api/weather/forget", post(forget_place))
        .route("/api/home", get(home_view))
        .route("/api/home/picture", get(home_picture).post(keep_home_picture))
        .route("/api/home/report", post(home_report))
        .route("/api/home/key", post(new_home_key))
        .route("/api/home/forget", post(forget_home))
        .route("/api/duplicates", get(duplicates))
        .route("/api/analyse", post(analyse))
        .route("/api/watching", post(watching))
        .route("/api/add", post(add))
        .route("/api/delete", post(delete))
        .route("/api/mark", post(mark))
        .route("/api/restore", post(restore))
        .route("/api/empty-bin", post(empty_bin))
        .route("/api/forget-emptied", post(forget_emptied))
        .route("/api/sources", post(save_albums))
        .route("/api/auto", post(set_daily_sync))
        .route("/api/sync", post(sync))
        .route("/api/refresh-slideshow", post(refresh_slideshow))
        .route("/api/unlock", post(unlock))
        .route("/api/lock", post(lock))
        .route("/api/pin", post(change_pin))
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(Arc::clone(&app), protect))
        .layer(middleware::from_fn(log_requests))
        .with_state(app)
}
