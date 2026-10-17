//! How much listening time the Yoto players have left, from Yoto Limiter
//! (<https://yoto-limiter.com/developers>).
//!
//! The frame only reads: it keeps one live stream of Yoto Limiter's status open (server-sent
//! events, sent at once, whenever a player changes, and at least every minute). Pausing, adding time and changing
//! limits stay in Yoto Limiter's own app, so a key with the `read` scope alone is enough.
//! What is heard is kept in memory only: it is out of date within minutes anyway.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use reqwest::header::ACCEPT;
use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Notify;
use tracing::{debug, warn};

use crate::app::{blocking, Moment};
use crate::store::{self, Config};
use crate::types::YotoKey;

pub const YOTO_URL: &str = "https://yoto-limiter.com";
/// The stream says something at least every 25 seconds (a comment, if nothing else); this
/// long without a word and it is given up and opened again.
const QUIET_LIMIT: Duration = Duration::from_secs(90);
/// A status this old is no longer shown as how things are now.
const IN_TOUCH_FOR: Duration = Duration::from_secs(150);
/// After the stream fails it is opened again after this long, doubling up to the longest.
const RETRY_FIRST: Duration = Duration::from_secs(15);
const RETRY_LONGEST: Duration = Duration::from_mins(10);
/// A household's worth of players; any more are left out.
const MOST_PLAYERS: usize = 12;
const LONGEST_NAME: usize = 80;
/// One line of the stream longer than this is not a status.
const LONGEST_LINE: usize = 1024 * 1024;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum YotoError {
    #[error("Yoto Limiter could not be reached ({0})")]
    Unreachable(String),
    #[error("Yoto Limiter does not know that key: it may have been mistyped or deleted")]
    UnknownKey,
    #[error("the Yoto Limiter account is not on Plus, which API keys need")]
    NeedsPlus,
    #[error("the key is not allowed to read: make one with the read scope")]
    NotAllowed,
    #[error(
        "the key already has two live connections open, the most Yoto Limiter allows; another app may be using it"
    )]
    TooManyStreams,
    #[error("Yoto Limiter answered with status {0}")]
    Refused(u16),
    #[error("Yoto Limiter sent something that could not be read")]
    Unreadable,
    #[error("Yoto Limiter's live status went quiet")]
    Quiet,
}

impl YotoError {
    fn from_status(status: u16) -> YotoError {
        match status {
            401 => YotoError::UnknownKey,
            402 => YotoError::NeedsPlus,
            403 => YotoError::NotAllowed,
            429 => YotoError::TooManyStreams,
            other => YotoError::Refused(other),
        }
    }

    /// A problem that trying again will not mend: the key or the account has to change first.
    pub fn lasting(&self) -> bool {
        matches!(self, YotoError::UnknownKey | YotoError::NeedsPlus | YotoError::NotAllowed)
    }
}

impl From<reqwest::Error> for YotoError {
    fn from(error: reqwest::Error) -> Self {
        YotoError::Unreachable(error.without_url().to_string())
    }
}

/// What `/api/status` and each `status` event carry (the parts the frame shows; there are more).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Status {
    pub players: Vec<Player>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Player {
    pub id: i64,
    pub name: String,
    /// Connected to Yoto's cloud now.
    #[serde(default)]
    pub connected: bool,
    /// Outside the allowed hours now.
    #[serde(default)]
    pub locked: bool,
    /// While a hold is on, when it ends.
    #[serde(default)]
    pub held_until: Option<String>,
    /// Today's first stretch of allowed hours, "HH:MM" on the household's clock.
    #[serde(default)]
    pub allowed_from: Option<String>,
    #[serde(default)]
    pub allowed_until: Option<String>,
    #[serde(default)]
    pub budget: Budget,
    /// The card in the player now.
    #[serde(default)]
    pub card: Option<Card>,
}

/// Today's time, as the service gives it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[allow(clippy::struct_excessive_bools, reason = "the service's own fields, as it names them")]
pub struct Budget {
    /// False while the daily limit is off: then the limit and what is left mean nothing.
    #[serde(default)]
    pub limited: bool,
    #[serde(default)]
    pub limit_secs: i64,
    #[serde(default)]
    pub remaining_secs: i64,
    #[serde(default)]
    pub used_secs: i64,
    #[serde(default)]
    pub playing: bool,
    #[serde(default)]
    pub exhausted: bool,
    /// The card in the player is over its own limit.
    #[serde(default)]
    pub blocked: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Card {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Default)]
struct Known {
    players: Vec<Player>,
    heard: Option<Moment>,
    problem: Option<YotoError>,
}

pub struct Yoto {
    base: String,
    known: Mutex<Known>,
    /// Signalled when the key changes, so the stream is opened again with the new one.
    key_changed: Notify,
}

/// An HTTP client for a stream that stays open: unlike the usual one it has no time limit on
/// a whole request, only on connecting (a quiet stream is noticed by `QUIET_LIMIT` instead).
pub fn stream_client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_rustls_tls()
        .dns_resolver(Arc::new(crate::dns::FrameResolver))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .expect("the HTTP client configuration is valid")
}

/// Shorten what another service says to a length the screen can show.
fn trimmed(text: &str) -> String {
    text.trim().chars().take(LONGEST_NAME).collect()
}

/// A moment the service gives (RFC 3339), as seconds since 1970.
fn moment(text: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text?).ok().map(|at| at.timestamp())
}

/// The events of a server-sent event stream, put back together from its pieces, which can
/// break anywhere, a line or a character included.
#[derive(Default)]
struct Events {
    pending: Vec<u8>,
    kind: String,
    data: Vec<String>,
}

impl Events {
    /// Take in a piece of the stream, and give back the data of each `status` event it completes.
    fn feed(&mut self, piece: &[u8]) -> Result<Vec<String>, YotoError> {
        self.pending.extend_from_slice(piece);
        let mut complete = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                let data = std::mem::take(&mut self.data).join("\n");
                if std::mem::take(&mut self.kind) == "status" && !data.is_empty() {
                    complete.push(data);
                }
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.kind = value.to_string(),
                "data" => self.data.push(value.to_string()),
                // A comment (the line starts with ':') or a field the frame has no use for.
                _ => {}
            }
        }
        if self.pending.len() > LONGEST_LINE || self.data.iter().map(String::len).sum::<usize>() > LONGEST_LINE {
            return Err(YotoError::Unreadable);
        }
        Ok(complete)
    }
}

impl Yoto {
    /// Yoto Limiter at this address (its own, or a stand-in during tests).
    pub fn new(base: &str) -> Yoto {
        Yoto { base: base.trim_end_matches('/').to_string(), known: Mutex::default(), key_changed: Notify::new() }
    }

    fn known(&self) -> MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take in a status from the service.
    fn hear(&self, mut status: Status) {
        status.players.truncate(MOST_PLAYERS);
        for player in &mut status.players {
            player.name = trimmed(&player.name);
            if let Some(card) = player.card.as_mut() {
                card.name = card.name.as_deref().map(trimmed);
            }
        }
        let mut known = self.known();
        known.players = status.players;
        known.heard = Some(Moment::now());
        known.problem = None;
    }

    fn trouble(&self, problem: YotoError) {
        self.known().problem = Some(problem);
    }

    /// Forget everything that was heard, as when the key is forgotten.
    pub fn forget(&self) {
        *self.known() = Known::default();
    }

    /// Say that the key has changed, so the stream is opened again with it at once.
    pub fn key_changed(&self) {
        self.key_changed.notify_one();
    }

    /// Ask for the status once with a key, to find out whether the key works. What it says is kept.
    pub async fn check(&self, http: &reqwest::Client, key: &YotoKey) -> Result<(), YotoError> {
        let request = http.get(format!("{}/api/status", self.base)).bearer_auth(key.as_str());
        let status: Status = fetch(request.timeout(Duration::from_secs(30))).await?;
        self.hear(status);
        Ok(())
    }

    /// Follow the live status until the stream ends or fails, and say why it did.
    async fn follow(&self, http: &reqwest::Client, key: &YotoKey) -> YotoError {
        let request = http.get(format!("{}/api/events", self.base)).bearer_auth(key.as_str());
        let mut response = match request.header(ACCEPT, "text/event-stream").send().await {
            Ok(response) => response,
            Err(e) => return e.into(),
        };
        if !response.status().is_success() {
            return YotoError::from_status(response.status().as_u16());
        }
        debug!("following Yoto Limiter's live status");
        let mut events = Events::default();
        loop {
            let piece = match tokio::time::timeout(QUIET_LIMIT, response.chunk()).await {
                Err(_) => return YotoError::Quiet,
                Ok(Err(e)) => return e.into(),
                Ok(Ok(None)) => return YotoError::Unreachable("the live status ended".into()),
                Ok(Ok(Some(piece))) => piece,
            };
            let statuses = match events.feed(&piece) {
                Ok(statuses) => statuses,
                Err(e) => return e,
            };
            for data in statuses {
                match serde_json::from_str::<Status>(&data) {
                    Ok(status) => self.hear(status),
                    Err(e) => debug!(error = %e, "a Yoto Limiter status could not be read"),
                }
            }
        }
    }

    /// Keep following the players for as long as the server runs and a key is kept, opening
    /// the stream again when it fails, more slowly each time it fails in a row.
    pub async fn keep_following(self: Arc<Self>, http: reqwest::Client, cfg: Arc<Config>) {
        let mut pause = RETRY_FIRST;
        loop {
            let settings = Arc::clone(&cfg);
            let Some(key) = blocking(move || store::load_settings(&settings).yoto_key).await else {
                self.forget();
                self.key_changed.notified().await;
                continue;
            };
            let problem = tokio::select! {
                problem = self.follow(&http, &key) => problem,
                () = self.key_changed.notified() => {
                    pause = RETRY_FIRST;
                    continue;
                }
            };
            // Having heard something since the last failure, the next try starts quickly again.
            if self.known().heard.is_some_and(|heard| heard.elapsed() < QUIET_LIMIT) {
                pause = RETRY_FIRST;
            }
            warn!(error = %problem, "Yoto Limiter's live status stopped");
            let lasting = problem.lasting();
            self.trouble(problem);
            if lasting {
                // Nothing will change until there is another key.
                self.key_changed.notified().await;
                pause = RETRY_FIRST;
                continue;
            }
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = self.key_changed.notified() => {}
            }
            pause = (pause * 2).min(RETRY_LONGEST);
        }
    }

    /// The players for the page. `in_touch` is false once nothing has been heard for a while,
    /// and what was last heard is then old news, to be shown as such. Times are seconds since
    /// 1970; `heard` is how many seconds ago the status came, so the page can count down.
    pub fn view(&self) -> Value {
        let known = self.known();
        let players: Vec<Value> = known
            .players
            .iter()
            .map(|player| {
                let budget = &player.budget;
                json!({
                    "id": player.id,
                    "name": player.name,
                    "connected": player.connected,
                    "locked": player.locked,
                    "held_until": moment(player.held_until.as_deref()),
                    "allowed_from": player.allowed_from,
                    "allowed_until": player.allowed_until,
                    "limited": budget.limited,
                    "limit_secs": budget.limit_secs,
                    "remaining_secs": budget.remaining_secs,
                    "used_secs": budget.used_secs,
                    "playing": budget.playing,
                    "exhausted": budget.exhausted,
                    "blocked": budget.blocked,
                    "card": player.card.as_ref().and_then(|card| card.name.as_deref()),
                })
            })
            .collect();
        json!({
            "in_touch": known.heard.is_some_and(|heard| heard.elapsed() < IN_TOUCH_FOR),
            "heard": known.heard.map(|heard| heard.elapsed().as_secs()),
            "problem": known.problem.as_ref().map(ToString::to_string),
            "players": players,
        })
    }
}

async fn fetch<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T, YotoError> {
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(YotoError::from_status(response.status().as_u16()));
    }
    serde_json::from_slice(&response.bytes().await?).map_err(|_| YotoError::Unreadable)
}

#[cfg(test)]
pub mod testing {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::sse::{Event, KeepAlive, Sse};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use futures_util::stream;
    use serde_json::{json, Value};

    /// The key the stand-in knows; any other is unknown, and `KEY_WITHOUT_PLUS` is on an
    /// account that has left Plus.
    pub const KEY: &str = "yl_test0123456789abcdef";
    pub const KEY_WITHOUT_PLUS: &str = "yl_lapsed0123456789abcdef";

    pub fn status(remaining_secs: i64) -> Value {
        json!({ "players": [
            {
                "id": 7, "name": "Mia's Yoto", "role": "owner", "connected": true, "locked": false,
                "held_until": null, "daily_limit": true, "daily_minutes": 60, "time_zone": "Europe/London",
                "today": "2026-10-17", "allowed_from": "07:00", "allowed_until": "19:00",
                "budget": { "used_secs": 3600 - remaining_secs, "limited": true, "limit_secs": 3600,
                    "remaining_secs": remaining_secs, "extension_secs": 0, "playing": true, "exhausted": false,
                    "blocked": false, "card_id": "c1", "total_played_secs": 3700 - remaining_secs },
                "card": { "id": "c1", "name": "The Gruffalo" },
                "something_new": [1, 2, 3],
            },
            {
                "id": 8, "name": "Leo's Yoto Mini", "connected": false, "locked": true,
                "held_until": "2026-10-17T18:30:00+01:00", "daily_limit": false, "daily_minutes": 45,
                "time_zone": "Europe/London", "today": "2026-10-17",
                "budget": { "limited": false, "limit_secs": 2700, "remaining_secs": 0, "used_secs": 0, "playing": false },
            },
        ] })
    }

    #[derive(Default)]
    pub struct Seen {
        /// How many times the live status was opened.
        pub streams: AtomicUsize,
    }

    fn key(headers: &HeaderMap) -> Result<(), StatusCode> {
        let offered = headers.get("authorization").and_then(|value| value.to_str().ok()).unwrap_or("");
        match offered.strip_prefix("Bearer ") {
            Some(KEY) => Ok(()),
            Some(KEY_WITHOUT_PLUS) => Err(StatusCode::PAYMENT_REQUIRED),
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    }

    /// Start a stand-in for Yoto Limiter and return its address. The live status sends a
    /// status with 30 minutes left at once, a comment, then one with 29 minutes left, and stays open.
    pub async fn yoto_limiter() -> (String, Arc<Seen>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Seen::default());
        let status_route = |headers: HeaderMap| async move {
            match key(&headers) {
                Ok(()) => axum::Json(status(1800)).into_response(),
                Err(code) => (code, "unknown API key").into_response(),
            }
        };
        let events = |State(seen): State<Arc<Seen>>, headers: HeaderMap| async move {
            if let Err(code) = key(&headers) {
                return (code, "unknown API key").into_response();
            }
            seen.streams.fetch_add(1, Ordering::Relaxed);
            let first = Event::default().event("status").data(status(1800).to_string());
            let second = Event::default().event("status").data(status(1740).to_string());
            let sent =
                stream::iter([Ok::<_, Infallible>(first), Ok(Event::default().comment("still here")), Ok(second)]);
            let and_then_nothing = stream::pending();
            Sse::new(futures_util::StreamExt::chain(sent, and_then_nothing))
                .keep_alive(KeepAlive::new().interval(Duration::from_secs(25)))
                .into_response()
        };
        let app = Router::new()
            .route("/api/status", get(status_route))
            .route("/api/events", get(events))
            .with_state(Arc::clone(&seen));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> YotoKey {
        YotoKey::parse(text).unwrap()
    }

    #[test]
    fn events_are_put_back_together_however_the_stream_is_broken_up() {
        let stream = "event: status\ndata: {\"a\":\r\ndata: 1}\n\n: still here\n\nevent: other\ndata: x\n\nevent:status\ndata:{\"b\":\"é\"}\n\n";
        let whole = Events::default().feed(stream.as_bytes()).unwrap();
        assert_eq!(whole, ["{\"a\":\n1}", "{\"b\":\"é\"}"]);
        // Byte by byte, the 'é' split in two included.
        let mut events = Events::default();
        let mut pieces = Vec::new();
        for byte in stream.as_bytes() {
            pieces.extend(events.feed(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(pieces, whole);
        let endless = vec![b'x'; LONGEST_LINE + 1];
        assert_eq!(Events::default().feed(&endless), Err(YotoError::Unreadable));
    }

    #[test]
    fn a_status_is_read_leniently_and_shown_with_its_times_as_numbers() {
        let yoto = Yoto::new("http://127.0.0.1:1");
        assert_eq!(yoto.view(), json!({ "in_touch": false, "heard": null, "problem": null, "players": [] }));
        yoto.hear(serde_json::from_value(testing::status(1500)).unwrap());
        let view = yoto.view();
        assert_eq!(view["in_touch"], true);
        let mia = &view["players"][0];
        assert_eq!(
            (&mia["name"], &mia["card"], &mia["remaining_secs"]),
            (&json!("Mia's Yoto"), &json!("The Gruffalo"), &json!(1500))
        );
        assert_eq!(
            (&mia["limited"], &mia["playing"], &mia["allowed_from"]),
            (&json!(true), &json!(true), &json!("07:00"))
        );
        let leo = &view["players"][1];
        assert_eq!(leo["held_until"], 1_792_258_200, "17:30 UTC, 18:30 in London that day");
        assert_eq!((&leo["connected"], &leo["locked"], &leo["card"]), (&json!(false), &json!(true), &Value::Null));

        let long_name = "x".repeat(500);
        yoto.hear(serde_json::from_value(json!({ "players": [{ "id": 1, "name": long_name }] })).unwrap());
        assert_eq!(yoto.view()["players"][0]["name"].as_str().unwrap().len(), LONGEST_NAME);
        assert!(serde_json::from_value::<Status>(json!({ "players": [{ "name": "no id" }] })).is_err());
        yoto.forget();
        assert_eq!(yoto.view()["players"], json!([]));
    }

    #[tokio::test]
    async fn a_key_is_checked_against_the_service() {
        let (base, _) = testing::yoto_limiter().await;
        let yoto = Yoto::new(&base);
        let http = reqwest::Client::new();
        assert_eq!(yoto.check(&http, &key("yl_wrong0123456789")).await, Err(YotoError::UnknownKey));
        assert_eq!(yoto.check(&http, &key(testing::KEY_WITHOUT_PLUS)).await, Err(YotoError::NeedsPlus));
        assert!(YotoError::NeedsPlus.lasting() && !YotoError::Quiet.lasting());
        assert_eq!(yoto.check(&http, &key(testing::KEY)).await, Ok(()));
        assert_eq!(yoto.view()["players"][0]["remaining_secs"], 1800, "what the check heard is kept");
        let nowhere = Yoto::new("http://127.0.0.1:1");
        assert!(matches!(nowhere.check(&http, &key(testing::KEY)).await, Err(YotoError::Unreachable(_))));
    }

    #[tokio::test]
    async fn the_live_status_is_followed() {
        let (base, seen) = testing::yoto_limiter().await;
        let yoto = Arc::new(Yoto::new(&base));
        let http = reqwest::Client::new();
        let following = {
            let yoto = Arc::clone(&yoto);
            tokio::spawn(async move { yoto.follow(&http, &key(testing::KEY)).await })
        };
        for _ in 0..200 {
            if yoto.view()["players"][0]["remaining_secs"] == 1740 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let view = yoto.view();
        assert_eq!(view["players"][0]["remaining_secs"], 1740, "the second status replaced the first");
        assert_eq!(seen.streams.load(std::sync::atomic::Ordering::Relaxed), 1);
        following.abort();

        let refused = Yoto::new(&base).follow(&reqwest::Client::new(), &key("yl_wrong0123456789")).await;
        assert_eq!(refused, YotoError::UnknownKey);
    }
}
