//! The weather where the frame is, from Open-Meteo (free, and needs no account).
//!
//! The user names a town on the page; it is looked up once and kept as a place
//! on the map. The forecast for that place is fetched every half hour and kept
//! in memory: weather that is hours old is not worth keeping across a restart.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Notify;
use tracing::{debug, warn};

use crate::store::{self, Config};
use crate::types::{Place, PlaceQuery};

pub const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
pub const PLACES_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const REFRESH_EVERY: Duration = Duration::from_mins(30);
/// Weather older than this is no longer shown as the weather now.
const STALE_AFTER_SECS: u64 = 3 * 60 * 60;
const DAYS: usize = 4;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WeatherError {
    #[error("the weather service could not be reached ({0})")]
    Unreachable(String),
    #[error("the weather service answered with status {0}")]
    Refused(u16),
    #[error("the weather service sent something that could not be read")]
    Unreadable,
    #[error("no place by that name was found; try the nearest town or add the country")]
    NoSuchPlace,
}

impl From<reqwest::Error> for WeatherError {
    fn from(error: reqwest::Error) -> Self {
        WeatherError::Unreachable(error.without_url().to_string())
    }
}

/// What Open-Meteo answers a forecast request with (the parts we ask for).
#[derive(Debug, Deserialize)]
struct Forecast {
    current: Current,
    daily: Daily,
}

#[derive(Debug, Deserialize)]
struct Current {
    temperature_2m: f64,
    /// A WMO weather code: 0 clear, 3 overcast, 61 rain, and so on.
    weather_code: u16,
    is_day: u8,
}

#[derive(Debug, Deserialize)]
struct Daily {
    time: Vec<String>,
    weather_code: Vec<u16>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<Option<f64>>,
}

#[derive(Debug, Deserialize)]
struct Found {
    #[serde(default)]
    results: Vec<FoundPlace>,
}

#[derive(Debug, Deserialize)]
struct FoundPlace {
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    admin1: Option<String>,
    #[serde(default)]
    country: Option<String>,
}

struct Reading {
    place: Place,
    forecast: Forecast,
    fetched: u64,
}

pub struct Weather {
    forecast_url: String,
    places_url: String,
    latest: Mutex<Option<Reading>>,
    /// Signalled when the place changes, so its weather is fetched at once.
    place_changed: Notify,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

async fn fetch<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T, WeatherError> {
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(WeatherError::Refused(response.status().as_u16()));
    }
    serde_json::from_slice(&response.bytes().await?).map_err(|_| WeatherError::Unreadable)
}

impl Weather {
    /// Weather from the services at these addresses (Open-Meteo's, or stand-ins during tests).
    pub fn new(forecast_url: &str, places_url: &str) -> Weather {
        Weather {
            forecast_url: forecast_url.to_string(),
            places_url: places_url.to_string(),
            latest: Mutex::default(),
            place_changed: Notify::new(),
        }
    }

    /// Find the place someone typed the name of.
    pub async fn find(&self, http: &reqwest::Client, query: &PlaceQuery) -> Result<Place, WeatherError> {
        let asked = [("name", query.as_str()), ("count", "1"), ("language", "en"), ("format", "json")];
        let found: Found = fetch(http.get(&self.places_url).query(&asked)).await?;
        let first = found.results.into_iter().next().ok_or(WeatherError::NoSuchPlace)?;
        // "Lisbon, Portugal" rather than only "Lisbon": there is more than one of most towns.
        let mut name = first.name.clone();
        for part in [first.admin1, first.country].into_iter().flatten() {
            if !name.split(", ").any(|known| known == part) {
                name = format!("{name}, {part}");
            }
        }
        Place::new(&name, first.latitude, first.longitude).ok_or(WeatherError::Unreadable)
    }

    /// Fetch the forecast for a place.
    pub async fn refresh(&self, http: &reqwest::Client, place: &Place) -> Result<(), WeatherError> {
        let (latitude, longitude, days) = (place.latitude.to_string(), place.longitude.to_string(), DAYS.to_string());
        let asked = [
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            ("current", "temperature_2m,weather_code,is_day"),
            ("daily", "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max"),
            ("timezone", "auto"),
            ("forecast_days", days.as_str()),
        ];
        let forecast: Forecast = fetch(http.get(&self.forecast_url).query(&asked)).await?;
        let days = forecast.daily.time.len();
        let daily = &forecast.daily;
        let complete = [
            daily.weather_code.len(),
            daily.temperature_2m_max.len(),
            daily.temperature_2m_min.len(),
            daily.precipitation_probability_max.len(),
        ];
        if days == 0 || complete.iter().any(|count| *count != days) {
            return Err(WeatherError::Unreadable);
        }
        debug!(place = place.name, "weather fetched");
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Reading { place: place.clone(), forecast, fetched: now_secs() });
        Ok(())
    }

    /// Say that the place has changed, so its weather is fetched without waiting.
    pub fn place_changed(&self) {
        self.place_changed.notify_one();
    }

    /// Keep the weather for the chosen place up to date, for as long as the server runs.
    pub async fn keep_fresh(self: Arc<Self>, http: reqwest::Client, cfg: Arc<Config>) {
        loop {
            let settings = Arc::clone(&cfg);
            if let Some(place) = crate::app::blocking(move || store::load_settings(&settings).place).await {
                if let Err(e) = self.refresh(&http, &place).await {
                    warn!(error = %e, "could not fetch the weather");
                }
            }
            tokio::select! {
                () = tokio::time::sleep(REFRESH_EVERY) => {}
                () = self.place_changed.notified() => {}
            }
        }
    }

    /// The weather for the page: now, and day by day. Null where it is not known (no place
    /// chosen, another place's weather, or weather too old to call "now").
    pub fn view(&self, place: Option<&Place>) -> Value {
        let latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        let known = latest.as_ref().filter(|reading| {
            Some(&reading.place) == place && now_secs().saturating_sub(reading.fetched) <= STALE_AFTER_SECS
        });
        let Some(reading) = known else {
            return json!({ "place": place.map(|place| &place.name), "now": null, "days": [] });
        };
        let (now, daily) = (&reading.forecast.current, &reading.forecast.daily);
        let days: Vec<Value> = (0..daily.time.len())
            .map(|day| {
                json!({
                    "date": daily.time[day],
                    "code": daily.weather_code[day],
                    "max": daily.temperature_2m_max[day],
                    "min": daily.temperature_2m_min[day],
                    "rain": daily.precipitation_probability_max[day],
                })
            })
            .collect();
        json!({
            "place": reading.place.name,
            "now": { "temp": now.temperature_2m, "code": now.weather_code, "day": now.is_day != 0 },
            "days": days,
            "fetched": reading.fetched,
        })
    }
}

#[cfg(test)]
pub mod testing {
    use std::collections::HashMap;

    use axum::extract::Query;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use serde_json::json;

    /// Start a stand-in for Open-Meteo and return its address. `/forecast` answers for
    /// anywhere except latitude 0 (which it refuses); `/places` knows Lisbon and Springfield.
    pub async fn open_meteo() -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let forecast = |Query(asked): Query<HashMap<String, String>>| async move {
            if asked["latitude"] == "0" {
                return StatusCode::BAD_REQUEST.into_response();
            }
            if asked["latitude"] == "1" {
                return "not json".into_response();
            }
            let days = if asked["latitude"] == "2" { json!([5]) } else { json!([61, 3]) };
            axum::Json(json!({
                "current": { "temperature_2m": 14.2, "weather_code": 61, "is_day": 1 },
                "daily": {
                    "time": ["2026-10-13", "2026-10-14"],
                    "weather_code": days,
                    "temperature_2m_max": [16.0, 18.5],
                    "temperature_2m_min": [9.1, 10.0],
                    "precipitation_probability_max": [80, null],
                },
            }))
            .into_response()
        };
        let places = |Query(asked): Query<HashMap<String, String>>| async move {
            let found: Response = match asked["name"].as_str() {
                "Lisbon" => axum::Json(json!({ "results": [
                    { "name": "Lisbon", "latitude": 38.72, "longitude": -9.14, "admin1": "Lisbon", "country": "Portugal" }
                ] }))
                .into_response(),
                "Springfield" => axum::Json(json!({ "results": [
                    { "name": "Springfield", "latitude": 39.8, "longitude": -89.6, "admin1": "Illinois", "country": "United States" }
                ] }))
                .into_response(),
                _ => axum::Json(json!({ "generationtime_ms": 0.5 })).into_response(),
            };
            found
        };
        let app = Router::new().route("/forecast", get(forecast)).route("/places", get(places));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn weather() -> Weather {
        let base = testing::open_meteo().await;
        Weather::new(&format!("{base}/forecast"), &format!("{base}/places"))
    }

    fn query(name: &str) -> PlaceQuery {
        serde_json::from_value(json!(name)).unwrap()
    }

    #[tokio::test]
    async fn a_named_place_is_found_with_enough_of_its_name_to_tell_it_apart() {
        let weather = weather().await;
        let http = reqwest::Client::new();
        let lisbon = weather.find(&http, &query("Lisbon")).await.unwrap();
        assert_eq!((lisbon.name.as_str(), lisbon.latitude, lisbon.longitude), ("Lisbon, Portugal", 38.72, -9.14));
        let springfield = weather.find(&http, &query("Springfield")).await.unwrap();
        assert_eq!(springfield.name, "Springfield, Illinois, United States");
        assert_eq!(weather.find(&http, &query("Atlantis")).await, Err(WeatherError::NoSuchPlace));
    }

    #[tokio::test]
    async fn the_forecast_for_the_chosen_place_is_fetched_and_shown() {
        let weather = weather().await;
        let http = reqwest::Client::new();
        let lisbon = Place::new("Lisbon, Portugal", 38.72, -9.14).unwrap();
        assert_eq!(weather.view(Some(&lisbon)), json!({ "place": "Lisbon, Portugal", "now": null, "days": [] }));
        assert_eq!(weather.view(None), json!({ "place": null, "now": null, "days": [] }));

        weather.refresh(&http, &lisbon).await.unwrap();
        let view = weather.view(Some(&lisbon));
        assert_eq!(view["now"], json!({ "temp": 14.2, "code": 61, "day": true }));
        assert_eq!(view["days"][0], json!({ "date": "2026-10-13", "code": 61, "max": 16.0, "min": 9.1, "rain": 80.0 }));
        assert_eq!(view["days"][1]["rain"], Value::Null);
        assert!(view["fetched"].as_u64().unwrap() > 1_700_000_000);

        let porto = Place::new("Porto, Portugal", 41.15, -8.61).unwrap();
        assert_eq!(weather.view(Some(&porto))["now"], Value::Null, "one place's weather is not shown for another");
        weather.latest.lock().unwrap().as_mut().unwrap().fetched -= STALE_AFTER_SECS + 1;
        assert_eq!(weather.view(Some(&lisbon))["now"], Value::Null, "old weather is not the weather now");
    }

    #[tokio::test]
    async fn a_forecast_that_cannot_be_had_says_why_and_keeps_the_last_one() {
        let weather = weather().await;
        let http = reqwest::Client::new();
        let lisbon = Place::new("Lisbon", 38.72, -9.14).unwrap();
        weather.refresh(&http, &lisbon).await.unwrap();
        let at = |latitude: f64| Place::new("Lisbon", latitude, -9.14).unwrap();
        assert_eq!(weather.refresh(&http, &at(0.0)).await, Err(WeatherError::Refused(400)));
        assert_eq!(weather.refresh(&http, &at(1.0)).await, Err(WeatherError::Unreadable));
        assert_eq!(weather.refresh(&http, &at(2.0)).await, Err(WeatherError::Unreadable), "days that do not line up");
        assert!(weather.view(Some(&lisbon))["now"].is_object());
        let nowhere = Weather::new("http://127.0.0.1:1/forecast", "http://127.0.0.1:1/places");
        let unreachable = nowhere.refresh(&http, &lisbon).await.unwrap_err();
        assert!(unreachable.to_string().contains("could not be reached"), "{unreachable}");
        assert!(matches!(nowhere.find(&http, &query("Lisbon")).await, Err(WeatherError::Unreachable(_))));
    }
}
