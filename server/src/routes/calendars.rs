//! The calendar: what is on it, which calendars the frame follows, and events added by hand.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::app::{blocking, now_secs, Shared};
use crate::routes::{header, ApiError, ApiResult, Parsed};
use crate::types::{CalendarLinks, EventId, Window};
use crate::{auth, calendar, store};

#[derive(Deserialize)]
pub(crate) struct CalendarsRequest {
    pub(crate) links: CalendarLinks,
}

#[derive(Deserialize)]
pub(crate) struct DeleteEventRequest {
    pub(crate) id: EventId,
}

/// What is on the calendar between `?from=` and `?to=` (seconds since 1970), how the
/// subscribed calendars are doing, and the events that were added by hand.
pub(crate) async fn calendar_view(
    State(app): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<Value> {
    let asked = |name: &str| query.get(name).map(String::as_str);
    let window = Window::from_query(asked("from"), asked("to"), i64::try_from(now_secs()).unwrap_or(i64::MAX));
    let token = header(&headers, "x-frame-token").map(str::to_string);
    Json(
        blocking(move || {
            // Calendar links are secrets: only someone who may change them gets to read them.
            let may_change = auth::allowed(&store::load_settings(&app.cfg), token.as_deref());
            json!({
                "events": app.calendars.between(window),
                "feeds": app.calendars.feeds(may_change),
                "added": app.calendars.added(),
                "unlocked": may_change,
            })
        })
        .await,
    )
}

/// Subscribe to exactly these calendars, and fetch them straight away.
pub(crate) async fn save_calendars(
    State(app): State<Shared>,
    Parsed(request): Parsed<CalendarsRequest>,
) -> Json<Value> {
    let links = request.links.0;
    let count = links.len();
    blocking(move || {
        app.update_settings(|settings| settings.calendars.clone_from(&links));
        app.calendars.set_links(&links);
    })
    .await;
    info!(calendars = count, "calendar links saved");
    Json(json!({ "calendars": count }))
}

pub(crate) async fn add_event(State(app): State<Shared>, Parsed(new): Parsed<calendar::NewEvent>) -> ApiResult {
    let added = blocking(move || app.calendars.add(new))
        .await
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!({ "event": added })))
}

pub(crate) async fn delete_event(
    State(app): State<Shared>,
    Parsed(request): Parsed<DeleteEventRequest>,
) -> Json<Value> {
    Json(json!({ "deleted": blocking(move || app.calendars.remove(&request.id)).await }))
}
