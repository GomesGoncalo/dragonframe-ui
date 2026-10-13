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
use crate::types::{CalendarLinks, EventId, Reminders, Window};
use crate::{auth, calendar, store};

#[derive(Deserialize)]
pub(crate) struct CalendarsRequest {
    pub(crate) links: CalendarLinks,
}

#[derive(Deserialize)]
pub(crate) struct RemindersRequest {
    pub(crate) rules: Reminders,
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
            let settings = store::load_settings(&app.cfg);
            let may_change = auth::allowed(&settings, token.as_deref());
            // Each event says which reminder rules are about it, by their place in the list.
            let mut events = app.calendars.between(window);
            for event in &mut events {
                let title = event["title"].as_str().unwrap_or_default();
                let about: Vec<usize> = (0..settings.reminders.len())
                    .filter(|&rule| settings.reminders[rule].words.found_in(title))
                    .collect();
                event["remind"] = json!(about);
            }
            json!({
                "events": events,
                "feeds": app.calendars.feeds(may_change),
                "added": app.calendars.added(),
                "reminders": settings.reminders,
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

/// Keep exactly these reminder rules. The frame's screen picks them up within half a minute.
pub(crate) async fn save_reminders(
    State(app): State<Shared>,
    Parsed(request): Parsed<RemindersRequest>,
) -> Json<Value> {
    let rules = request.rules.into_inner();
    let count = rules.len();
    blocking(move || app.update_settings(|settings| settings.reminders = rules)).await;
    info!(rules = count, "reminder rules saved");
    Json(json!({ "reminders": count }))
}
