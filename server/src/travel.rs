//! Getting to events: when to leave, and how the journey goes.
//!
//! A trip (see `types::Trip`) says how an event is reached. Public transport, walking
//! and cycling are worked out by Transport for London's journey planner, which needs no
//! account and knows timetables and disruptions. Driving is worked out by the `TomTom`
//! routing service, which needs a key: without one, trips with a car in them are not
//! offered. A trip by car to a station and on by train uses both, one after the other.
//!
//! Places are typed as postcodes, addresses or station names and looked up once each
//! (postcodes at postcodes.io, anything else at OpenStreetMap's search). Plans are kept
//! in memory for a quarter of an hour, so a page asking again costs the services nothing.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::London;
use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use tracing::debug;

use crate::app::now_secs;
use crate::types::{How, RoadsKey, Spot, Trip};

pub const JOURNEYS_URL: &str = "https://api.tfl.gov.uk/Journey/JourneyResults";
pub const ROADS_URL: &str = "https://api.tomtom.com/routing/1/calculateRoute";
pub const POSTCODES_URL: &str = "https://api.postcodes.io/postcodes";
pub const ADDRESSES_URL: &str = "https://nominatim.openstreetmap.org/search";
pub const TILES_URL: &str = "https://tile.openstreetmap.org";
/// The deepest the map zooms, and how many of its squares are kept on the frame.
const DEEPEST_ZOOM: u32 = 18;
const MOST_TILES_KEPT: usize = 1500;
const MOST_TILE_BYTES: usize = 1024 * 1024;
/// A line on the map is drawn with at most this many points.
const MOST_POINTS: usize = 250;
/// What a stretch by car is called, beside the journey planner's own words for the rest.
const DRIVE: &str = "drive";
/// OpenStreetMap's search asks every program that uses it to say what it is.
const WHO_ASKS: &str = "frameserver (a photo frame at home)";
/// What public transport's planner may use between two places.
const PUBLIC_TRANSPORT: &str = "tube,bus,national-rail,overground,dlr,elizabeth-line,tram,walking";
/// A plan this fresh is handed out again without asking the services.
const FRESH_SECS: u64 = 15 * 60;
/// When the services cannot be asked, a plan up to this old is still better than none.
const USABLE_SECS: u64 = 6 * 60 * 60;
/// Getting from the car to the platform.
const PARKING_MINUTES: i64 = 5;
const MOST_PLANS_KEPT: usize = 200;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TravelError {
    #[error("the journey planner could not be reached ({0})")]
    Unreachable(String),
    #[error("the journey planner answered with status {0}")]
    Refused(u16),
    #[error("the driving service did not accept the key")]
    KeyRefused,
    #[error("the journey planner sent something that could not be read")]
    Unreadable,
    #[error("\"{0}\" was not found; a postcode is the surest thing to give")]
    NotFound(String),
    #[error("no way was found of making this journey")]
    NoRoute,
    #[error("say where home is first, or give this trip a place to start from")]
    NoStart,
    #[error("trips by car need a key for the driving service")]
    NoKey,
    #[error("there is no such square of the map")]
    NoSuchTile,
}

impl From<reqwest::Error> for TravelError {
    fn from(error: reqwest::Error) -> Self {
        // Without its address: the driving service's carries the key.
        TravelError::Unreachable(error.without_url().to_string())
    }
}

/// A place on the map.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    latitude: f64,
    longitude: f64,
}

/// A stretch of a journey as a line on the map.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// How that stretch is made: `drive`, or what the journey planner calls it (`walking`,
    /// `bus`, `tube`, `national-rail` and so on). Each is drawn in its own colour.
    pub kind: String,
    /// Where it goes, as latitude and longitude.
    pub points: Vec<[f64; 2]>,
}

impl Line {
    /// A line through these points, thinned if there are more than are worth drawing.
    fn through(kind: &str, points: &[[f64; 2]]) -> Option<Line> {
        let every = points.len().div_ceil(MOST_POINTS).max(1);
        let last = points.last()?;
        let mut thinned: Vec<[f64; 2]> = points.iter().step_by(every).copied().collect();
        if thinned.last() != Some(last) {
            thinned.push(*last);
        }
        (thinned.len() > 1).then(|| Line { kind: kind.to_string(), points: thinned })
    }
}

/// When to leave for an event, and how the journey goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// When to leave, in seconds since 1970, time to spare included.
    pub leave: i64,
    /// When the journey gets there.
    pub arrive: i64,
    /// What to do, in order: "Walk to Orpington Station (13 min)".
    pub steps: Vec<String>,
    /// What there is to catch first, with its time: "17:20 Southeastern to London Bridge".
    pub catch: Option<String>,
    /// How each step is made, one for one with `steps`, in the words `Line::kind` uses.
    pub modes: Vec<String>,
    /// The way there, for drawing on a map.
    pub lines: Vec<Line>,
}

impl Plan {
    pub fn view(&self) -> Value {
        let lines: Vec<Value> =
            self.lines.iter().map(|line| json!({ "kind": line.kind, "points": line.points })).collect();
        json!({
            "leave": self.leave, "arrive": self.arrive, "steps": self.steps, "modes": self.modes,
            "catch": self.catch, "lines": lines,
        })
    }
}

/// A journey by public transport, on foot or by bicycle, as the planner gave it.
struct Journey {
    start: i64,
    arrival: i64,
    steps: Vec<String>,
    modes: Vec<String>,
    catch: Option<String>,
    lines: Vec<Line>,
}

/// A time as the planner writes it: `2026-10-14T17:07:00`, on London's clocks.
fn clock_time<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<NaiveDateTime, D::Error> {
    let text = String::deserialize(deserializer)?;
    NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M:%S").map_err(serde::de::Error::custom)
}

#[derive(Deserialize)]
struct Planned {
    #[serde(default)]
    journeys: Vec<PlannedJourney>,
}

#[derive(Deserialize)]
struct PlannedJourney {
    #[serde(rename = "startDateTime", deserialize_with = "clock_time")]
    start: NaiveDateTime,
    #[serde(rename = "arrivalDateTime", deserialize_with = "clock_time")]
    arrival: NaiveDateTime,
    #[serde(default)]
    legs: Vec<Leg>,
}

#[derive(Deserialize)]
struct Leg {
    duration: u32,
    #[serde(rename = "departureTime", deserialize_with = "clock_time")]
    departure: NaiveDateTime,
    instruction: Instruction,
    mode: Named,
    #[serde(default)]
    path: Option<LegPath>,
}

#[derive(Deserialize)]
struct LegPath {
    /// The planner writes the line as text holding a list of `[latitude, longitude]`.
    #[serde(rename = "lineString", default)]
    line: String,
}

#[derive(Deserialize)]
struct Instruction {
    summary: String,
}

#[derive(Deserialize)]
struct Named {
    id: String,
}

#[derive(Deserialize)]
struct Postcode {
    result: Coordinates,
}

#[derive(Deserialize)]
struct Coordinates {
    latitude: f64,
    longitude: f64,
}

/// OpenStreetMap's search writes its numbers as text.
#[derive(Deserialize)]
struct Address {
    lat: String,
    lon: String,
}

#[derive(Deserialize)]
struct Driven {
    #[serde(default)]
    routes: Vec<Route>,
}

#[derive(Deserialize)]
struct Route {
    summary: RouteSummary,
    #[serde(default)]
    legs: Vec<RouteLeg>,
}

#[derive(Deserialize)]
struct RouteLeg {
    #[serde(default)]
    points: Vec<Coordinates>,
}

#[derive(Deserialize)]
struct RouteSummary {
    #[serde(rename = "travelTimeInSeconds")]
    seconds: i64,
}

/// Where the services are: the real ones, or stand-ins during tests.
#[derive(Clone, Debug)]
pub struct Services {
    pub journeys: String,
    pub roads: String,
    pub postcodes: String,
    pub addresses: String,
    pub tiles: String,
}

impl Default for Services {
    fn default() -> Self {
        Services {
            journeys: JOURNEYS_URL.into(),
            roads: ROADS_URL.into(),
            postcodes: POSTCODES_URL.into(),
            addresses: ADDRESSES_URL.into(),
            tiles: TILES_URL.into(),
        }
    }
}

/// Plans by the trip, where it starts and the moment to arrive, each with when it was made.
type Plans = HashMap<(Trip, Option<Spot>, i64), (Plan, u64)>;

#[derive(Default)]
pub struct Travel {
    services: Services,
    /// Places already looked up, by what was typed.
    found: Mutex<HashMap<Spot, Point>>,
    /// Plans already made.
    plans: Mutex<Plans>,
}

async fn fetch<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T, TravelError> {
    let response = request.send().await?;
    match response.status().as_u16() {
        200..=299 => serde_json::from_slice(&response.bytes().await?).map_err(|_| TravelError::Unreadable),
        401 | 403 => Err(TravelError::KeyRefused),
        // How both planners say that there is no such journey.
        400 | 404 => Err(TravelError::NoRoute),
        status => Err(TravelError::Refused(status)),
    }
}

/// A time on London's clocks as a moment. (The planner answers in London time, whatever
/// the season; an hour that happens twice in autumn is taken as the first.)
fn in_london(time: NaiveDateTime) -> i64 {
    London.from_local_datetime(&time).earliest().map_or_else(|| time.and_utc().timestamp(), |at| at.timestamp())
}

fn moment(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap_or_default()
}

fn minutes(secs: i64) -> i64 {
    (secs + 59) / 60
}

impl Travel {
    #[cfg(test)]
    pub fn new(services: Services) -> Travel {
        Travel { services, ..Travel::default() }
    }

    /// Where on the map a typed place is. Each is looked up once.
    async fn locate(&self, http: &reqwest::Client, spot: &Spot) -> Result<Point, TravelError> {
        if let Some(known) = self.found.lock().unwrap_or_else(PoisonError::into_inner).get(spot) {
            return Ok(*known);
        }
        let nowhere = || TravelError::NotFound(spot.as_str().to_string());
        let point = if let Some(code) = spot.postcode() {
            let found: Result<Postcode, _> = fetch(http.get(format!("{}/{code}", self.services.postcodes))).await;
            let at = found.map_err(|e| if e == TravelError::NoRoute { nowhere() } else { e })?.result;
            Point { latitude: at.latitude, longitude: at.longitude }
        } else {
            let asked = [("q", spot.as_str()), ("format", "jsonv2"), ("limit", "1")];
            let request = http.get(&self.services.addresses).query(&asked).header("User-Agent", WHO_ASKS);
            let found: Vec<Address> = fetch(request).await?;
            let first = found.first().ok_or_else(nowhere)?;
            let number = |text: &str| text.parse::<f64>().map_err(|_| TravelError::Unreadable);
            Point { latitude: number(&first.lat)?, longitude: number(&first.lon)? }
        };
        debug!(spot = spot.as_str(), "place looked up");
        self.found.lock().unwrap_or_else(PoisonError::into_inner).insert(spot.clone(), point);
        Ok(point)
    }

    /// The journey between two places that gets there by `arrive` and leaves latest.
    async fn journey(
        &self,
        http: &reqwest::Client,
        (from, to): (Point, Point),
        modes: &str,
        arrive: i64,
    ) -> Result<Journey, TravelError> {
        let by = moment(arrive).with_timezone(&London);
        let (date, time) = (by.format("%Y%m%d").to_string(), by.format("%H%M").to_string());
        let url = format!(
            "{}/{},{}/to/{},{}",
            self.services.journeys, from.latitude, from.longitude, to.latitude, to.longitude
        );
        let asked = [("date", date.as_str()), ("time", time.as_str()), ("timeIs", "Arriving"), ("mode", modes)];
        let planned: Planned = fetch(http.get(url).query(&asked)).await?;
        // Of those that are there in time, the one that leaves last; failing any, the first to get there.
        let in_time = planned.journeys.iter().filter(|j| in_london(j.arrival) <= arrive).max_by_key(|j| j.start);
        let best = in_time.or_else(|| planned.journeys.iter().min_by_key(|j| j.arrival)).ok_or(TravelError::NoRoute)?;
        let steps = best.legs.iter().map(|leg| format!("{} ({} min)", leg.instruction.summary, leg.duration));
        let catch = best.legs.iter().find(|leg| !matches!(leg.mode.id.as_str(), "walking" | "cycle"));
        let lines = best.legs.iter().filter_map(|leg| {
            let points: Vec<[f64; 2]> = serde_json::from_str(&leg.path.as_ref()?.line).ok()?;
            Line::through(&leg.mode.id, &points)
        });
        Ok(Journey {
            lines: lines.collect(),
            modes: best.legs.iter().map(|leg| leg.mode.id.clone()).collect(),
            start: in_london(best.start),
            arrival: in_london(best.arrival),
            steps: steps.collect(),
            catch: catch.map(|leg| format!("{} {}", leg.departure.format("%H:%M"), leg.instruction.summary)),
        })
    }

    /// How many seconds the drive between two places takes when arriving at `arrive`, and the way it goes.
    async fn drive(
        &self,
        http: &reqwest::Client,
        key: &RoadsKey,
        (from, to): (Point, Point),
        arrive: i64,
    ) -> Result<(i64, Option<Line>), TravelError> {
        let url = format!(
            "{}/{},{}:{},{}/json",
            self.services.roads, from.latitude, from.longitude, to.latitude, to.longitude
        );
        let by = moment(arrive).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let asked = [("key", key.as_str()), ("arriveAt", by.as_str()), ("travelMode", "car")];
        let driven: Driven = fetch(http.get(url).query(&asked)).await?;
        let route = driven.routes.first().ok_or(TravelError::NoRoute)?;
        let points: Vec<[f64; 2]> =
            route.legs.iter().flat_map(|leg| &leg.points).map(|at| [at.latitude, at.longitude]).collect();
        Ok((route.summary.seconds, Line::through(DRIVE, &points)))
    }

    async fn work_out(
        &self,
        http: &reqwest::Client,
        key: Option<&RoadsKey>,
        home: Option<&Spot>,
        trip: &Trip,
        arrive: i64,
    ) -> Result<Plan, TravelError> {
        let key = if trip.how.drives() { Some(key.ok_or(TravelError::NoKey)?) } else { None };
        let start = trip.from.as_ref().or(home).ok_or(TravelError::NoStart)?;
        let (from, to) = (self.locate(http, start).await?, self.locate(http, &trip.to).await?);
        let spare = i64::from(trip.margin) * 60;
        match (trip.how, key, &trip.via) {
            (How::Car, Some(key), _) => {
                let (seconds, line) = self.drive(http, key, (from, to), arrive).await?;
                let steps = vec![format!("Drive ({} min)", minutes(seconds))];
                let (lines, modes) = (line.into_iter().collect(), vec![DRIVE.to_string()]);
                Ok(Plan { leave: arrive - seconds - spare, arrive, steps, modes, catch: None, lines })
            }
            (How::Park, Some(key), Some(station)) => {
                let at = self.locate(http, station).await?;
                let onward = self.journey(http, (at, to), PUBLIC_TRANSPORT, arrive).await?;
                let parked = onward.start - PARKING_MINUTES * 60;
                let (seconds, line) = self.drive(http, key, (from, at), parked).await?;
                let mut steps = vec![format!("Drive to {} ({} min)", station.as_str(), minutes(seconds))];
                steps.extend(onward.steps);
                let modes = std::iter::once(DRIVE.to_string()).chain(onward.modes).collect();
                let lines = line.into_iter().chain(onward.lines).collect();
                let (leave, arrive) = (parked - seconds - spare, onward.arrival);
                Ok(Plan { leave, arrive, steps, modes, catch: onward.catch, lines })
            }
            (how, _, _) => {
                let modes = match how {
                    How::Walk => "walking",
                    How::Cycle => "cycle",
                    _ => PUBLIC_TRANSPORT,
                };
                let found = self.journey(http, (from, to), modes, arrive).await?;
                let Journey { start, arrival, steps, modes, catch, lines } = found;
                Ok(Plan { leave: start - spare, arrive: arrival, steps, modes, catch, lines })
            }
        }
    }

    /// One square of the map, as a PNG picture: square `x`, `y` of zoom `z`, as map makers
    /// number them. Squares are kept on the frame, so each is fetched from the map's
    /// keepers once.
    pub async fn tile(
        &self,
        http: &reqwest::Client,
        cfg: &std::sync::Arc<crate::store::Config>,
        (z, x, y): (u32, u32, u32),
    ) -> Result<Vec<u8>, TravelError> {
        if z > DEEPEST_ZOOM || x >= 1 << z || y >= 1 << z {
            return Err(TravelError::NoSuchTile);
        }
        let folder = cfg.data_dir.join("tiles");
        let kept = folder.join(format!("{z}-{x}-{y}.png"));
        let at = kept.clone();
        if let Some(picture) = crate::app::blocking(move || std::fs::read(at).ok()).await {
            return Ok(picture);
        }
        let request = http.get(format!("{}/{z}/{x}/{y}.png", self.services.tiles)).header("User-Agent", WHO_ASKS);
        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(TravelError::Refused(response.status().as_u16()));
        }
        let picture = response.bytes().await?.to_vec();
        // A square is a PNG picture of modest size; anything else is not kept or handed on.
        if !picture.starts_with(b"\x89PNG") || picture.len() > MOST_TILE_BYTES {
            return Err(TravelError::Unreadable);
        }
        let keep = picture.clone();
        crate::app::blocking(move || {
            // Starting again when there are very many is simpler than choosing which to drop.
            if std::fs::read_dir(&folder).is_ok_and(|squares| squares.count() >= MOST_TILES_KEPT) {
                let _ = std::fs::remove_dir_all(&folder);
            }
            let _ = std::fs::create_dir_all(&folder);
            let _ = crate::store::write_atomically(&kept, &keep);
        })
        .await;
        Ok(picture)
    }

    /// When to leave for something that starts at `arrive`, making the trip as it says.
    /// Asked again within a quarter of an hour, the same plan is given; and if the services
    /// cannot be asked, the last plan stands for some hours.
    pub async fn plan(
        &self,
        http: &reqwest::Client,
        key: Option<&RoadsKey>,
        home: Option<&Spot>,
        trip: &Trip,
        arrive: i64,
    ) -> Result<Plan, TravelError> {
        let which = (trip.clone(), home.cloned(), arrive);
        let kept = self.plans.lock().unwrap_or_else(PoisonError::into_inner).get(&which).cloned();
        let age = |made: u64| now_secs().saturating_sub(made);
        if let Some((plan, _)) = kept.as_ref().filter(|(_, made)| age(*made) < FRESH_SECS) {
            return Ok(plan.clone());
        }
        match (self.work_out(http, key, home, trip, arrive).await, kept) {
            (Ok(plan), _) => {
                let mut plans = self.plans.lock().unwrap_or_else(PoisonError::into_inner);
                if plans.len() >= MOST_PLANS_KEPT {
                    plans.clear();
                }
                plans.insert(which, (plan.clone(), now_secs()));
                Ok(plan)
            }
            (Err(TravelError::Unreachable(_) | TravelError::Refused(_)), Some((plan, made)))
                if age(made) < USABLE_SECS =>
            {
                Ok(plan)
            }
            (Err(e), _) => Err(e),
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use axum::extract::{Path, Query};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;

    /// What the stand-in for the map hands out as a square: the start of a PNG file.
    pub const PICTURE: &[u8] = b"\x89PNG\r\n\x1a\n a picture";
    /// The key the stand-in for the driving service accepts.
    pub const KEY: &str = "0123456789abcdefKEY";

    /// Start stand-ins for all four services and say where they are.
    ///
    /// Postcodes: BR6 0AA (home, latitude 51.37) and SE1 9SG (51.50). Addresses: "Orpington
    /// station" (51.373) and "Wembley Stadium" (51.556). Public transport to SE1 9SG takes
    /// 37 minutes on the 17:20 train, with a slower journey that arrives late; walking takes
    /// 50. Nothing goes to Wembley except from the station. Driving takes 43 minutes, or 8
    /// to the station.
    pub async fn services() -> Services {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let postcodes = |Path(code): Path<String>| async move {
            let at = match code.as_str() {
                "BR60AA" => (51.37, 0.09),
                "SE19SG" => (51.50, -0.09),
                _ => return StatusCode::NOT_FOUND.into_response(),
            };
            axum::Json(json!({ "status": 200, "result": { "latitude": at.0, "longitude": at.1 } })).into_response()
        };
        let addresses = |Query(asked): Query<HashMap<String, String>>| async move {
            let found = match asked["q"].as_str() {
                "Orpington station" => json!([{ "lat": "51.373", "lon": "0.089", "name": "Orpington" }]),
                "Wembley Stadium" => json!([{ "lat": "51.556", "lon": "-0.2796" }]),
                "Garbled" => json!([{ "lat": "north", "lon": "west" }]),
                _ => json!([]),
            };
            axum::Json(found)
        };
        let journeys = |Path((from, to)): Path<(String, String)>, Query(asked): Query<HashMap<String, String>>| async move {
            let leg = |mode: &str, minutes: u32, leaves: &str, summary: &str| json!({ "duration": minutes, "departureTime": leaves, "instruction": { "summary": summary }, "mode": { "id": mode } });
            let day = format!("{}-{}-{}", &asked["date"][..4], &asked["date"][4..6], &asked["date"][6..]);
            let at = |time: &str| format!("{day}T{time}:00");
            let reply: Response = match (from.as_str(), to.as_str(), asked["mode"].as_str()) {
                (_, "51.5,-0.09", "walking") => axum::Json(json!({ "journeys": [{
                    "startDateTime": at("17:35"), "arrivalDateTime": at("18:25"),
                    "legs": [leg("walking", 50, &at("17:35"), "Walk to SE1 9SG")],
                }] }))
                .into_response(),
                ("51.37,0.09", "51.5,-0.09", _) => axum::Json(json!({ "journeys": [
                    { "startDateTime": at("16:50"), "arrivalDateTime": at("17:27"), "legs": [] },
                    { "startDateTime": at("17:07"), "arrivalDateTime": at("17:44"), "legs": [
                        json!({ "duration": 13, "departureTime": at("17:07"), "mode": { "id": "walking" },
                            "instruction": { "summary": "Walk to Orpington Station" },
                            "path": { "lineString": "[[51.37, 0.09],[51.372, 0.0895],[51.373, 0.089]]" } }),
                        leg("national-rail", 15, &at("17:20"), "Southeastern to London Bridge"),
                        leg("walking", 9, &at("17:35"), "Walk to SE1 9SG"),
                    ] },
                    { "startDateTime": at("17:40"), "arrivalDateTime": at("18:40"), "legs": [] },
                ] }))
                .into_response(),
                ("51.373,0.089", "51.556,-0.2796", _) => axum::Json(json!({ "journeys": [{
                    "startDateTime": at("16:30"), "arrivalDateTime": at("17:50"),
                    "legs": [leg("national-rail", 80, &at("16:30"), "Southeastern to Charing Cross")],
                }] }))
                .into_response(),
                ("51.5,-0.09", ..) => "not json".into_response(),
                _ => StatusCode::NOT_FOUND.into_response(),
            };
            reply
        };
        let roads = |Path(ends): Path<String>, Query(asked): Query<HashMap<String, String>>| async move {
            if asked.get("key").map(String::as_str) != Some(KEY) {
                return StatusCode::FORBIDDEN.into_response();
            }
            let seconds = if ends.ends_with(":51.373,0.089") { 8 * 60 } else { 43 * 60 };
            let points = json!([{ "latitude": 51.37, "longitude": 0.09 }, { "latitude": 51.5, "longitude": -0.09 }]);
            axum::Json(json!({ "routes": [{
                "summary": { "travelTimeInSeconds": seconds, "arriveAt": asked["arriveAt"] },
                "legs": [{ "points": points }],
            }] }))
            .into_response()
        };
        let app = Router::new()
            .route("/postcodes/{code}", get(postcodes))
            .route("/addresses", get(addresses))
            .route("/journeys/{from}/to/{to}", get(journeys))
            .route("/roads/{ends}/json", get(roads))
            .route(
                "/tiles/{z}/{x}/{y}",
                get(|Path((z, _, _)): Path<(u32, u32, String)>| async move {
                    match z {
                        9 => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                        8 => "<html>not a picture</html>".into_response(),
                        _ => PICTURE.into_response(),
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Services {
            journeys: format!("{base}/journeys"),
            roads: format!("{base}/roads"),
            postcodes: format!("{base}/postcodes"),
            addresses: format!("{base}/addresses"),
            tiles: format!("{base}/tiles"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 18:30 on 14 October 2026 in London (British Summer Time), as a moment.
    const HALF_PAST_SIX: i64 = 1_791_999_000;

    fn trip(how: &str, to: &str, more: &Value) -> Trip {
        let mut trip = json!({ "title": "Dentist", "to": to, "how": how, "margin": 10 });
        trip.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
        serde_json::from_value(trip).unwrap()
    }

    fn london(secs: i64) -> String {
        moment(secs).with_timezone(&London).format("%H:%M").to_string()
    }

    #[tokio::test]
    async fn public_transport_is_planned_to_arrive_in_time_leaving_as_late_as_that_allows() {
        let travel = Travel::new(testing::services().await);
        let (http, home) = (reqwest::Client::new(), Spot::parse("br6 0aa"));
        assert_eq!(london(HALF_PAST_SIX), "18:30");

        let plan =
            travel.plan(&http, None, home.as_ref(), &trip("transit", "SE1 9SG", &json!({})), HALF_PAST_SIX).await;
        let plan = plan.unwrap();
        assert_eq!((london(plan.leave), london(plan.arrive)), ("16:57".into(), "17:44".into()), "ten minutes to spare");
        assert_eq!(plan.catch.as_deref(), Some("17:20 Southeastern to London Bridge"));
        assert_eq!(plan.steps[0], "Walk to Orpington Station (13 min)");
        assert_eq!(plan.steps.len(), 3);
        // The way there for the map: the stretch the planner drew.
        assert_eq!(plan.lines.len(), 1);
        assert_eq!((plan.lines[0].kind.as_str(), plan.lines[0].points.len()), ("walking", 3));
        assert_eq!(plan.modes, ["walking", "national-rail", "walking"], "one for each step");
        assert_eq!(plan.view()["lines"][0]["points"][0], json!([51.37, 0.09]));

        let on_foot =
            travel.plan(&http, None, home.as_ref(), &trip("walk", "SE1 9SG", &json!({})), HALF_PAST_SIX).await;
        let on_foot = on_foot.unwrap();
        assert_eq!((london(on_foot.leave), on_foot.catch), ("17:25".into(), None));
    }

    #[tokio::test]
    async fn driving_needs_the_key_and_can_be_joined_to_a_train() {
        let travel = Travel::new(testing::services().await);
        let (http, home) = (reqwest::Client::new(), Spot::parse("BR6 0AA"));
        let key = RoadsKey::parse(testing::KEY);
        let by_car = trip("car", "SE1 9SG", &json!({ "margin": 0 }));

        assert_eq!(travel.plan(&http, None, home.as_ref(), &by_car, HALF_PAST_SIX).await, Err(TravelError::NoKey));
        let wrong = RoadsKey::parse("not-the-right-key-at-all");
        let refused = travel.plan(&http, wrong.as_ref(), home.as_ref(), &by_car, HALF_PAST_SIX).await;
        assert_eq!(refused, Err(TravelError::KeyRefused));

        let plan = travel.plan(&http, key.as_ref(), home.as_ref(), &by_car, HALF_PAST_SIX).await.unwrap();
        assert_eq!((london(plan.leave), &plan.steps), ("17:47".into(), &vec!["Drive (43 min)".to_string()]));
        assert_eq!(plan.lines, [Line { kind: "drive".into(), points: vec![[51.37, 0.09], [51.5, -0.09]] }]);
        assert_eq!(plan.modes, ["drive"]);

        // To the station by car, five minutes to park, and on by the 16:30 train.
        let park = trip("park", "Wembley Stadium", &json!({ "via": "Orpington station" }));
        let plan = travel.plan(&http, key.as_ref(), home.as_ref(), &park, HALF_PAST_SIX).await.unwrap();
        assert_eq!((london(plan.leave), london(plan.arrive)), ("16:07".into(), "17:50".into()));
        assert_eq!(plan.steps, ["Drive to Orpington station (8 min)", "Southeastern to Charing Cross (80 min)"]);
        assert_eq!(plan.catch.as_deref(), Some("16:30 Southeastern to Charing Cross"));
        assert_eq!(plan.modes, ["drive", "national-rail"]);
    }

    #[tokio::test]
    async fn a_trip_that_cannot_be_planned_says_why() {
        let travel = Travel::new(testing::services().await);
        let (http, home) = (reqwest::Client::new(), Spot::parse("BR6 0AA"));
        let plan = |trip: Trip, home: Option<Spot>| {
            let (travel, http) = (&travel, &http);
            async move { travel.plan(http, None, home.as_ref(), &trip, HALF_PAST_SIX).await }
        };
        assert_eq!(plan(trip("transit", "SE1 9SG", &json!({})), None).await, Err(TravelError::NoStart));
        assert_eq!(
            plan(trip("transit", "ZZ9 9ZZ", &json!({})), home.clone()).await,
            Err(TravelError::NotFound("ZZ9 9ZZ".into()))
        );
        let nowhere = plan(trip("transit", "Nan's house", &json!({})), home.clone()).await.unwrap_err();
        assert!(nowhere.to_string().contains("\"Nan's house\" was not found"), "{nowhere}");
        assert_eq!(plan(trip("transit", "Garbled", &json!({})), home.clone()).await, Err(TravelError::Unreadable));
        assert_eq!(plan(trip("transit", "Wembley Stadium", &json!({})), home.clone()).await, Err(TravelError::NoRoute));
        let from_town = trip("transit", "Wembley Stadium", &json!({ "from": "SE1 9SG" }));
        assert_eq!(plan(from_town, home.clone()).await, Err(TravelError::Unreadable));
    }

    #[tokio::test]
    async fn a_plan_is_kept_for_a_while_and_outlives_a_service_that_has_gone_quiet() {
        let services = testing::services().await;
        let travel = Travel::new(services.clone());
        let (http, home) = (reqwest::Client::new(), Spot::parse("BR6 0AA"));
        let to_town = trip("transit", "SE1 9SG", &json!({}));
        let first = travel.plan(&http, None, home.as_ref(), &to_town, HALF_PAST_SIX).await.unwrap();

        // The planner goes away. A fresh plan is not asked for again; an older one stands in.
        let gone = Travel {
            services: Services { journeys: "http://127.0.0.1:1/journeys".into(), ..services },
            ..Travel::default()
        };
        *gone.found.lock().unwrap() = travel.found.lock().unwrap().clone();
        *gone.plans.lock().unwrap() = travel.plans.lock().unwrap().clone();
        assert_eq!(gone.plan(&http, None, home.as_ref(), &to_town, HALF_PAST_SIX).await.as_ref(), Ok(&first));
        let age = |secs: u64| gone.plans.lock().unwrap().values_mut().for_each(|kept| kept.1 = now_secs() - secs);
        age(FRESH_SECS + 1);
        assert_eq!(gone.plan(&http, None, home.as_ref(), &to_town, HALF_PAST_SIX).await.as_ref(), Ok(&first));
        age(USABLE_SECS + 1);
        let stale = gone.plan(&http, None, home.as_ref(), &to_town, HALF_PAST_SIX).await.unwrap_err();
        assert!(matches!(stale, TravelError::Unreachable(_)), "{stale}");
        // Another time to arrive is another plan.
        assert!(gone.plan(&http, None, home.as_ref(), &to_town, HALF_PAST_SIX + 3600).await.is_err());
    }

    #[test]
    fn a_long_line_is_thinned_but_keeps_both_its_ends() {
        let long: Vec<[f64; 2]> = (0..1001).map(|n| [f64::from(n), 0.0]).collect();
        let line = Line::through("drive", &long).unwrap();
        assert!(line.points.len() <= MOST_POINTS + 1, "{}", line.points.len());
        assert_eq!((line.points[0], line.points.last()), ([0.0, 0.0], Some(&[1000.0, 0.0])));
        assert_eq!(Line::through("walk", &long[..3]).unwrap().points.len(), 3);
        assert_eq!(Line::through("walk", &long[..1]), None);
        assert_eq!(Line::through("walk", &[]), None);
    }

    #[tokio::test]
    async fn a_square_of_the_map_is_fetched_once_and_kept() {
        let frame = crate::store::testing::TempFrame::new("tiles");
        let cfg = std::sync::Arc::new(crate::store::Config {
            cloud_dir: frame.0.cloud_dir.clone(),
            data_dir: frame.0.data_dir.clone(),
        });
        let services = testing::services().await;
        let (travel, http) = (Travel::new(services.clone()), reqwest::Client::new());
        assert_eq!(travel.tile(&http, &cfg, (10, 512, 341)).await.unwrap(), testing::PICTURE);
        assert!(cfg.data_dir.join("tiles/10-512-341.png").is_file());
        // Kept: it is still had when the map's keepers cannot be reached.
        let gone = Travel::new(Services { tiles: "http://127.0.0.1:1".into(), ..services });
        assert_eq!(gone.tile(&http, &cfg, (10, 512, 341)).await.unwrap(), testing::PICTURE);
        assert!(matches!(gone.tile(&http, &cfg, (10, 0, 0)).await, Err(TravelError::Unreachable(_))));
        assert_eq!(travel.tile(&http, &cfg, (9, 1, 1)).await, Err(TravelError::Refused(503)));
        assert_eq!(travel.tile(&http, &cfg, (8, 1, 1)).await, Err(TravelError::Unreadable), "not a picture");
        assert!(!cfg.data_dir.join("tiles/8-1-1.png").exists(), "and so not kept");
        for nowhere in [(19, 0, 0), (2, 4, 0), (2, 0, 4)] {
            assert_eq!(travel.tile(&http, &cfg, nowhere).await, Err(TravelError::NoSuchTile), "{nowhere:?}");
        }
    }
}
