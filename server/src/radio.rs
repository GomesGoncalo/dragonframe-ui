//! Finding radio stations by name, in the community's directory at radio-browser.info
//! (free, and needs no account).
//!
//! The frame's screen plays a station itself, straight from the station's own address;
//! this only looks stations up, so that nobody has to hunt for a stream's address by hand.

use reqwest::header::USER_AGENT;
use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;

use crate::types::{Label, PlaceQuery, StreamLink};

pub const STATIONS_URL: &str = "https://all.api.radio-browser.info/json/stations/search";
/// How many stations a search offers.
const MOST_FOUND: usize = 8;
/// The directory asks those who use it to say who they are.
const WHO: &str = "frameserver (a photo frame's own radio)";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RadioError {
    #[error("the station directory could not be reached ({0})")]
    Unreachable(String),
    #[error("the station directory answered with status {0}")]
    Refused(u16),
    #[error("the station directory sent something that could not be read")]
    Unreadable,
}

impl From<reqwest::Error> for RadioError {
    fn from(error: reqwest::Error) -> Self {
        RadioError::Unreachable(error.without_url().to_string())
    }
}

/// A station as the directory describes it (the parts we use).
#[derive(Debug, Deserialize)]
struct Listed {
    name: String,
    url_resolved: String,
    #[serde(default)]
    codec: String,
    #[serde(default)]
    bitrate: u32,
    #[serde(default)]
    country: String,
    /// Whether it is sent in pieces (HLS), which the frame's browser may not play.
    #[serde(default)]
    hls: u8,
}

pub struct Radio {
    stations_url: String,
}

impl Radio {
    /// Stations from the directory at this address (radio-browser's, or a stand-in during tests).
    pub fn new(stations_url: &str) -> Radio {
        Radio { stations_url: stations_url.to_string() }
    }

    /// The stations whose names have what was typed, the most listened to first, as
    /// `{ name, stream, about }`. Only ones the frame can play: an ordinary MP3 or AAC stream.
    pub async fn find(&self, http: &reqwest::Client, query: &PlaceQuery) -> Result<Vec<Value>, RadioError> {
        let asked = [
            ("name", query.as_str()),
            ("limit", "40"),
            ("hidebroken", "true"),
            ("order", "clickcount"),
            ("reverse", "true"),
        ];
        let ask = || http.get(&self.stations_url).query(&asked).header(USER_AGENT, WHO).send();
        // The directory is several servers behind one name, and now and then one of them is
        // away: a request that got nowhere is made once more.
        let response = match ask().await {
            Ok(response) => response,
            Err(_) => ask().await?,
        };
        if !response.status().is_success() {
            return Err(RadioError::Refused(response.status().as_u16()));
        }
        let listed: Vec<Listed> =
            serde_json::from_slice(&response.bytes().await?).map_err(|_| RadioError::Unreadable)?;
        Ok(listed.iter().filter_map(playable).take(MOST_FOUND).collect())
    }
}

/// A listed station as the page is told of it, if the frame can play it.
fn playable(station: &Listed) -> Option<Value> {
    let plain = station.hls == 0 && matches!(station.codec.to_uppercase().as_str(), "MP3" | "AAC" | "AAC+");
    if !plain {
        return None;
    }
    // Names in the directory run long ("… 320k AAC"); what does not fit is cut, not refused.
    let name: String = station.name.trim().chars().filter(|c| !c.is_control()).take(60).collect();
    let (name, stream) = (Label::parse(&name)?, StreamLink::parse(&station.url_resolved)?);
    let mut about = vec![station.codec.to_uppercase()];
    if station.bitrate > 0 {
        about.push(format!("{} kbit/s", station.bitrate));
    }
    if !station.country.trim().is_empty() {
        about.push(station.country.trim().to_string());
    }
    Some(json!({ "name": name, "stream": stream, "about": about.join(" · ") }))
}

#[cfg(test)]
pub mod testing {
    use std::collections::HashMap;

    use axum::extract::Query;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use serde_json::json;

    /// Start a stand-in for the station directory and return the address of its search. It
    /// knows three stations named "Paradise" (one of them HLS, one in a codec the frame
    /// does not play), refuses "broken" and answers "garbled" with something unreadable.
    pub async fn directory() -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let search = |Query(asked): Query<HashMap<String, String>>| async move {
            match asked["name"].as_str() {
                "broken" => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                "garbled" => "not json".into_response(),
                "Paradise" => axum::Json(json!([
                    { "name": " Radio Paradise Main Mix ", "url_resolved": "http://stream.example/aac-320", "codec": "AAC", "bitrate": 320, "country": "The United States Of America", "hls": 0 },
                    { "name": "Paradise in pieces", "url_resolved": "https://stream.example/live.m3u8", "codec": "AAC", "bitrate": 96, "country": "", "hls": 1 },
                    { "name": "Paradise Ogg", "url_resolved": "https://stream.example/ogg", "codec": "OGG", "bitrate": 128, "country": "", "hls": 0 },
                    { "name": "Paradise FM", "url_resolved": "https://stream.example/fm", "codec": "mp3", "bitrate": 0, "country": "", "hls": 0 },
                ]))
                .into_response(),
                _ => axum::Json(json!([])).into_response(),
            }
        };
        let app = Router::new().route("/search", get(search));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("{base}/search")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str) -> PlaceQuery {
        PlaceQuery::parse(name).unwrap()
    }

    #[tokio::test]
    async fn stations_are_found_by_name_and_only_the_ones_the_frame_can_play() {
        let radio = Radio::new(&testing::directory().await);
        let found = radio.find(&reqwest::Client::new(), &query("Paradise")).await.unwrap();
        assert_eq!(
            found,
            vec![
                json!({ "name": "Radio Paradise Main Mix", "stream": "http://stream.example/aac-320", "about": "AAC · 320 kbit/s · The United States Of America" }),
                json!({ "name": "Paradise FM", "stream": "https://stream.example/fm", "about": "MP3" }),
            ]
        );
        assert_eq!(radio.find(&reqwest::Client::new(), &query("Nowhere")).await.unwrap(), Vec::<Value>::new());
    }

    #[tokio::test]
    async fn a_directory_that_cannot_help_says_why() {
        let radio = Radio::new(&testing::directory().await);
        let http = reqwest::Client::new();
        assert_eq!(radio.find(&http, &query("broken")).await, Err(RadioError::Refused(503)));
        assert_eq!(radio.find(&http, &query("garbled")).await, Err(RadioError::Unreadable));
        let nowhere = Radio::new("http://127.0.0.1:1/search");
        assert!(matches!(nowhere.find(&http, &query("x")).await, Err(RadioError::Unreachable(_))));
    }
}
