//! The calendar: what is on it, which calendars the frame follows, and events added by hand.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::app::{blocking, now_secs, Shared};
use crate::routes::{header, ApiError, ApiResult, Asked, Parsed};
use crate::types::{Arrival, CalendarLinks, EventId, EventPictures, How, Reminders, RoadsKey, Spot, Trips, Window};
use crate::{auth, calendar, store, travel};

/// A place typed for a trip, to look up: `?place=`.
#[derive(Deserialize)]
pub(crate) struct PlaceQuery {
    pub(crate) place: Spot,
}

/// A journey to plan: `?trip=` is the trip's place in the list and `?arrive=` when the event
/// starts. With `?how=`, the same trip made another way, for comparing.
#[derive(Deserialize)]
pub(crate) struct PlanQuery {
    pub(crate) trip: usize,
    pub(crate) arrive: Arrival,
    #[serde(default)]
    pub(crate) how: Option<How>,
}

#[derive(Deserialize)]
pub(crate) struct CalendarsRequest {
    pub(crate) links: CalendarLinks,
}

#[derive(Deserialize)]
pub(crate) struct RemindersRequest {
    pub(crate) rules: Reminders,
}

#[derive(Deserialize)]
pub(crate) struct EventPicturesRequest {
    pub(crate) rules: EventPictures,
}

#[derive(Deserialize)]
pub(crate) struct TravelRequest {
    pub(crate) home: Option<Spot>,
    pub(crate) trips: Trips,
}

#[derive(Deserialize)]
pub(crate) struct RoadsKeyRequest {
    /// The key, or null to stop using the driving service.
    pub(crate) key: Option<RoadsKey>,
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
                let title = event["title"].as_str().unwrap_or_default().to_string();
                let about: Vec<usize> = (0..settings.reminders.len())
                    .filter(|&rule| settings.reminders[rule].words.found_in(&title))
                    .collect();
                event["remind"] = json!(about);
                // Its picture, from the first picture rule about it, for Now and Next.
                event["picture"] = json!(settings.event_pictures.iter().find(|rule| rule.words.found_in(&title)));
                // And which trip, if any, says how it is reached.
                let place = event["place"].as_str().map(str::to_string);
                event["trip"] = json!(settings.trips.iter().position(|trip| trip.is_for(&title, place.as_deref())));
            }
            json!({
                "events": events,
                "feeds": app.calendars.feeds(may_change),
                "added": app.calendars.added(),
                "reminders": settings.reminders,
                "event_pictures": settings.event_pictures,
                // Home and the trips are addresses, and are shown like the calendar links are:
                // to whoever may change them.
                "travel": may_change.then(|| json!({ "home": settings.home, "trips": settings.trips })),
                "ways": How::available(settings.roads_key.is_some()),
                "driving": settings.roads_key.is_some(),
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
        app.calendar_changed();
    })
    .await;
    info!(calendars = count, "calendar links saved");
    Json(json!({ "calendars": count }))
}

pub(crate) async fn add_event(State(app): State<Shared>, Parsed(new): Parsed<calendar::NewEvent>) -> ApiResult {
    let added = blocking(move || {
        app.calendar_changed();
        app.calendars.add(new)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!({ "event": added })))
}

pub(crate) async fn delete_event(
    State(app): State<Shared>,
    Parsed(request): Parsed<DeleteEventRequest>,
) -> Json<Value> {
    let deleted = blocking(move || {
        app.calendar_changed();
        app.calendars.remove(&request.id)
    })
    .await;
    Json(json!({ "deleted": deleted }))
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

/// Keep exactly these picture rules. Like an added event, they show on the frame's screen
/// within a few seconds.
pub(crate) async fn save_event_pictures(
    State(app): State<Shared>,
    Parsed(request): Parsed<EventPicturesRequest>,
) -> Json<Value> {
    let rules = request.rules.into_inner();
    let count = rules.len();
    blocking(move || {
        app.update_settings(|settings| settings.event_pictures = rules);
        app.calendar_changed();
    })
    .await;
    info!(rules = count, "picture rules saved");
    Json(json!({ "event_pictures": count }))
}

/// Keep where home is and exactly these trips. A trip with a car in it needs the driving key.
pub(crate) async fn save_travel(State(app): State<Shared>, Parsed(request): Parsed<TravelRequest>) -> ApiResult {
    let TravelRequest { home, trips } = request;
    let trips = trips.into_inner();
    let count = trips.len();
    blocking(move || {
        app.update_settings(|settings| {
            if settings.roads_key.is_none() && trips.iter().any(|trip| trip.how.drives()) {
                return Err(ApiError::new(StatusCode::CONFLICT, travel::TravelError::NoKey.to_string()));
            }
            settings.home = home;
            settings.trips = trips;
            Ok(())
        })?;
        app.calendar_changed();
        Ok(())
    })
    .await?;
    info!(trips = count, "trips saved");
    Ok(Json(json!({ "trips": count })))
}

/// Keep the key for the driving service, or with null forget it. It is never handed back.
pub(crate) async fn save_roads_key(State(app): State<Shared>, Parsed(request): Parsed<RoadsKeyRequest>) -> Json<Value> {
    let driving = request.key.is_some();
    blocking(move || {
        app.update_settings(|settings| settings.roads_key = request.key);
        app.calendar_changed();
    })
    .await;
    info!(driving, "driving key changed");
    Json(json!({ "driving": driving, "ways": How::available(driving) }))
}

/// Where a place typed for a trip was found (`?place=`), so whoever typed it can see it is
/// the one they meant.
pub(crate) async fn find_place(State(app): State<Shared>, Asked(query): Asked<PlaceQuery>) -> ApiResult {
    let found = app.travel.find(&app.http, &query.place).await.map_err(|e| {
        let status =
            if matches!(e, travel::TravelError::NotFound(_)) { StatusCode::NOT_FOUND } else { StatusCode::BAD_GATEWAY };
        ApiError::new(status, e.to_string())
    })?;
    Ok(Json(found))
}

/// When to leave for an event (see [`PlanQuery`]); the answer says which ways there are of
/// making the trip.
pub(crate) async fn travel_plan(State(app): State<Shared>, Asked(query): Asked<PlanQuery>) -> ApiResult {
    let cfg = std::sync::Arc::clone(&app.cfg);
    let settings = blocking(move || store::load_settings(&cfg)).await;
    let set_up = settings.trips.get(query.trip).ok_or_else(ApiError::not_found)?;
    let arrive = query.arrive.get();
    let trip = match query.how {
        None => set_up.clone(),
        Some(how) => set_up.by(how).ok_or_else(|| ApiError::bad_request("this trip cannot be made that way"))?,
    };
    let plan = app.travel.plan(&app.http, settings.roads_key.as_ref(), settings.home.as_ref(), &trip, arrive).await;
    let plan = plan.map_err(|e| {
        use travel::TravelError::{KeyRefused, NoKey, NoRoute, NoStart, NotFound};
        let status = match e {
            NotFound(_) | NoRoute => StatusCode::NOT_FOUND,
            NoStart | NoKey | KeyRefused => StatusCode::CONFLICT,
            _ => StatusCode::BAD_GATEWAY,
        };
        ApiError::new(status, e.to_string())
    })?;
    let mut view = plan.view();
    view["how"] = json!(trip.how);
    view["margin"] = json!(trip.margin);
    view["usual"] = json!(set_up.how);
    view["ways"] = json!(set_up.ways(settings.roads_key.is_some()));
    Ok(Json(view))
}

/// One square of the map (OpenStreetMap's), for drawing the way to an event on. They are
/// fetched through here, and kept, so the screen never talks to the map's keepers itself.
pub(crate) async fn map_tile(State(app): State<Shared>, Path(square): Path<(u32, u32, u32)>) -> ApiResult<Response> {
    let picture = app.travel.tile(&app.http, &app.cfg, square).await.map_err(|e| {
        let status = if e == travel::TravelError::NoSuchTile { StatusCode::NOT_FOUND } else { StatusCode::BAD_GATEWAY };
        ApiError::new(status, e.to_string())
    })?;
    let headers = [(CONTENT_TYPE, HeaderValue::from_static("image/png")), (CACHE_CONTROL, crate::routes::KEEP_FOREVER)];
    Ok((headers, picture).into_response())
}
