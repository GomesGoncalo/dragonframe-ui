//! The frame's calendar: the calendars it subscribes to, and events added by hand.
//!
//! A subscribed calendar is an iCalendar feed at an https address. The frame
//! fetches each one every quarter of an hour and keeps the last good copy on
//! disk, so the calendar is there after a restart and while the network is down.
//! Events added through this app live in `events.json`; they never leave the frame.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::ical::{self, Occurrence, Start};
use crate::store::{self, Config};
use crate::types::{CalendarLink, ClockTime, Day, EventId, Repeat, Title, Window};

/// How often subscribed calendars are fetched again.
const REFRESH_EVERY: Duration = Duration::from_mins(15);
const MAX_CALENDAR_BYTES: usize = 8 * 1024 * 1024;
/// Most events added by hand.
const MAX_ADDED: usize = 2000;
/// Most occurrences in one answer.
const MAX_LISTED: usize = 3000;

#[derive(Debug, Error)]
pub enum FeedError {
    #[error("the calendar could not be reached ({0})")]
    Unreachable(String),
    #[error("the calendar's server answered with status {0}")]
    Refused(u16),
    #[error(
        "Google has no public calendar at that link. If the calendar is private, use its \"Secret address in iCal format\", found in the calendar's settings in Google Calendar"
    )]
    NotPublic,
    #[error("the calendar is larger than 8 MB")]
    TooLarge,
    #[error("the link does not lead to a calendar")]
    NotACalendar,
}

impl From<reqwest::Error> for FeedError {
    fn from(error: reqwest::Error) -> Self {
        // The address is a secret; reqwest would otherwise print it with the error.
        let error = error.without_url();
        // The first line only says "error sending request"; what went wrong is underneath it.
        let mut reason = error.to_string();
        let mut cause = std::error::Error::source(&error);
        while let Some(deeper) = cause {
            reason = deeper.to_string();
            cause = deeper.source();
        }
        FeedError::Unreachable(reason)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AddError {
    #[error("there are already {MAX_ADDED} events; delete some first")]
    Full,
}

/// One subscribed calendar and how fetching it has gone.
struct Feed {
    link: CalendarLink,
    calendar: Arc<ical::Calendar>,
    /// When the copy we hold was fetched, seconds since 1970.
    fetched: Option<u64>,
    /// Why the latest fetch failed, if it did.
    error: Option<String>,
}

/// An event added through this app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Added {
    pub id: EventId,
    pub title: Title,
    pub day: Day,
    /// When in the day it starts; without one it takes the whole day.
    #[serde(default)]
    pub time: Option<ClockTime>,
    #[serde(default)]
    pub repeat: Repeat,
}

/// What the page sends to add an event.
#[derive(Debug, Deserialize)]
pub struct NewEvent {
    pub title: Title,
    pub day: Day,
    #[serde(default)]
    pub time: Option<ClockTime>,
    #[serde(default)]
    pub repeat: Repeat,
}

impl Added {
    fn event(&self) -> ical::Event {
        let start = match self.time {
            Some(time) => Start::Local(self.day.0.and_time(time.0)),
            None => Start::Day(self.day.0),
        };
        let rule = match self.repeat {
            Repeat::Never => None,
            Repeat::Daily => Some("FREQ=DAILY"),
            Repeat::Weekdays => Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
            Repeat::Weekly => Some("FREQ=WEEKLY"),
            Repeat::Monthly => Some("FREQ=MONTHLY"),
            Repeat::Yearly => Some("FREQ=YEARLY"),
        };
        ical::Event::new(self.title.as_str(), start, rule)
    }
}

pub struct Calendars {
    /// Where the last good copy of each subscribed calendar is kept.
    dir: PathBuf,
    added_file: PathBuf,
    feeds: Mutex<Vec<Feed>>,
    added: Mutex<Vec<Added>>,
    /// Signalled when the links change, so the new ones are fetched at once.
    links_changed: Notify,
    /// A second client for calendar servers the usual one cannot reach (see `gphotos::tolerant_client`).
    tolerant: Option<reqwest::Client>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Calendars {
    /// The calendar as it was left: subscribed calendars from their kept copies, and the
    /// events added by hand. Blocking.
    pub fn load(cfg: &Config, links: &[CalendarLink]) -> Calendars {
        let added_file = cfg.data_dir.join("events.json");
        let saved: Vec<Value> =
            fs::read_to_string(&added_file).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        // Leniently: one entry that does not read must not cost the others.
        let added = saved.into_iter().filter_map(|entry| serde_json::from_value(entry).ok()).collect();
        let calendars = Calendars {
            dir: cfg.data_dir.join("calendars"),
            added_file,
            feeds: Mutex::default(),
            added: Mutex::new(added),
            links_changed: Notify::new(),
            tolerant: None,
        };
        calendars.set_links(links);
        calendars
    }

    /// Also try this client when the usual one cannot reach a calendar's server.
    pub fn falling_back_to(mut self, tolerant: Option<reqwest::Client>) -> Calendars {
        self.tolerant = tolerant;
        self
    }

    fn kept_copy(&self, link: &CalendarLink) -> PathBuf {
        self.dir.join(format!("{}.ics", store::sha1_hex(link.as_str().as_bytes())))
    }

    /// Subscribe to exactly these calendars. One that was subscribed already keeps what it
    /// holds; one that is dropped has its kept copy removed. Blocking.
    pub fn set_links(&self, links: &[CalendarLink]) {
        let mut feeds = locked(&self.feeds);
        let mut before: Vec<Feed> = std::mem::take(&mut *feeds);
        for link in links {
            let feed = if let Some(known) = before.iter().position(|feed| feed.link == *link) {
                before.swap_remove(known)
            } else {
                let copy = self.kept_copy(link);
                let fetched = fs::metadata(&copy)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let calendar = fs::read_to_string(&copy).map(|ics| ical::parse(&ics)).unwrap_or_default();
                Feed { link: link.clone(), calendar: Arc::new(calendar), fetched, error: None }
            };
            feeds.push(feed);
        }
        for dropped in before {
            let _ = fs::remove_file(self.kept_copy(&dropped.link));
        }
        drop(feeds);
        self.links_changed.notify_one();
    }

    /// Fetch every subscribed calendar again. One that cannot be fetched keeps what it held.
    pub async fn refresh(self: &Arc<Self>, http: &reqwest::Client) {
        let links: Vec<CalendarLink> = locked(&self.feeds).iter().map(|feed| feed.link.clone()).collect();
        for link in links {
            let mut fetched = fetch(http, &link).await;
            if let (Err(FeedError::Unreachable(_)), Some(tolerant)) = (&fetched, &self.tolerant) {
                // Not reachable our usual way; some servers only speak an older dialect of TLS.
                if let Ok(ics) = fetch(tolerant, &link).await {
                    debug!(host = link.host(), "calendar fetched with the tolerant client");
                    fetched = Ok(ics);
                }
            }
            let outcome = match fetched {
                Ok(ics) => {
                    let (calendars, kept) = (Arc::clone(self), link.clone());
                    crate::app::blocking(move || {
                        let calendar = ical::parse(&ics);
                        let _ = fs::create_dir_all(&calendars.dir);
                        if let Err(e) = store::write_atomically(&calendars.kept_copy(&kept), ics.as_bytes()) {
                            debug!(error = %e, "could not keep a copy of a calendar");
                        }
                        Ok(calendar)
                    })
                    .await
                }
                Err(e) => Err(e),
            };
            let mut feeds = locked(&self.feeds);
            // The links may have changed while this one was being fetched.
            let Some(feed) = feeds.iter_mut().find(|feed| feed.link == link) else { continue };
            match outcome {
                Ok(calendar) => {
                    debug!(host = link.host(), events = calendar.events.len(), "calendar fetched");
                    feed.calendar = Arc::new(calendar);
                    feed.fetched = Some(now_secs());
                    feed.error = None;
                }
                Err(e) => {
                    warn!(host = link.host(), error = %e, "could not fetch a calendar");
                    feed.error = Some(e.to_string());
                }
            }
        }
    }

    /// Keep the subscribed calendars up to date, for as long as the server runs.
    pub async fn keep_fresh(self: Arc<Self>, http: reqwest::Client) {
        loop {
            self.refresh(&http).await;
            tokio::select! {
                () = tokio::time::sleep(REFRESH_EVERY) => {}
                () = self.links_changed.notified() => {}
            }
        }
    }

    /// Add an event by hand. Blocking.
    pub fn add(&self, new: NewEvent) -> Result<Added, AddError> {
        let mut added = locked(&self.added);
        if added.len() >= MAX_ADDED {
            return Err(AddError::Full);
        }
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let mut seed = format!("{}|{}|{nanos}", new.title.as_str(), new.day);
        let id = loop {
            let id = EventId::from_digest(&store::sha1(seed.as_bytes()));
            if added.iter().all(|event| event.id != id) {
                break id;
            }
            seed.push('+');
        };
        let event = Added { id, title: new.title, day: new.day, time: new.time, repeat: new.repeat };
        added.push(event.clone());
        self.save(&added);
        info!(day = %event.day, repeat = ?event.repeat, "calendar event added");
        Ok(event)
    }

    /// Delete an event that was added by hand. Returns whether there was one. Blocking.
    pub fn remove(&self, id: &EventId) -> bool {
        let mut added = locked(&self.added);
        let before = added.len();
        added.retain(|event| event.id != *id);
        let removed = added.len() < before;
        if removed {
            self.save(&added);
            info!("calendar event deleted");
        }
        removed
    }

    fn save(&self, added: &[Added]) {
        if let Err(e) = store::write_atomically(&self.added_file, json!(added).to_string().as_bytes()) {
            warn!(error = %e, "could not save the events added to the calendar");
        }
    }

    /// The events added by hand, soonest first, for the page that manages them.
    pub fn added(&self) -> Vec<Added> {
        let mut added = locked(&self.added).clone();
        added.sort_by_key(|event| (event.day, event.time));
        added
    }

    /// Everything that happens in the window, in order, for showing.
    pub fn between(&self, window: Window) -> Vec<Value> {
        let feeds: Vec<Arc<ical::Calendar>> =
            locked(&self.feeds).iter().map(|feed| Arc::clone(&feed.calendar)).collect();
        let mut found: Vec<(Occurrence, Value)> = Vec::new();
        for (feed, calendar) in feeds.into_iter().enumerate() {
            // Which subscribed calendar it is from, by position, so the page can give each its own colour.
            let from = json!({ "calendar": calendar.name, "feed": feed });
            for event in &calendar.events {
                found.extend(event.between(window.from, window.to).into_iter().map(|o| (o, from.clone())));
            }
        }
        for event in locked(&self.added).iter() {
            let from = json!({ "added": event.id });
            found.extend(event.event().between(window.from, window.to).into_iter().map(|o| (o, from.clone())));
        }
        found.sort_by(|(a, _), (b, _)| (a.order, &a.title).cmp(&(b.order, &b.title)));
        found
            .into_iter()
            .take(MAX_LISTED)
            .map(|(occurrence, from)| {
                json!({
                    "title": occurrence.title,
                    "place": occurrence.place,
                    "start": occurrence.start.view(),
                    "end": occurrence.end.view(),
                    "all_day": matches!(occurrence.start, ical::Moment::Day(_)),
                    "calendar": from["calendar"],
                    "feed": from["feed"],
                    "added": from["added"],
                })
            })
            .collect()
    }

    /// How each subscribed calendar is doing. The links themselves are secrets, so they
    /// are included only for someone who may change them.
    pub fn feeds(&self, with_links: bool) -> Vec<Value> {
        locked(&self.feeds)
            .iter()
            .map(|feed| {
                json!({
                    "link": with_links.then(|| feed.link.as_str()),
                    "host": feed.link.host(),
                    "name": feed.calendar.name,
                    "events": feed.calendar.events.len(),
                    "fetched": feed.fetched,
                    "error": feed.error,
                })
            })
            .collect()
    }
}

/// Why a calendar's server would not hand it over. Google answers "not found" for the public
/// feed of a calendar that is not public, which is worth saying in so many words.
fn refusal(link: &CalendarLink, status: u16) -> FeedError {
    if status == 404 && link.is_google_public_feed() {
        FeedError::NotPublic
    } else {
        FeedError::Refused(status)
    }
}

/// Download the text of a calendar.
async fn fetch(http: &reqwest::Client, link: &CalendarLink) -> Result<String, FeedError> {
    let mut response = http.get(link.as_str()).header("Accept", "text/calendar, */*").send().await?;
    if !response.status().is_success() {
        return Err(refusal(link, response.status().as_u16()));
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_CALENDAR_BYTES {
            return Err(FeedError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let ics = String::from_utf8_lossy(&body).into_owned();
    if !ics.to_ascii_uppercase().contains("BEGIN:VCALENDAR") {
        return Err(FeedError::NotACalendar);
    }
    Ok(ics)
}

#[cfg(test)]
pub mod testing {
    use std::sync::{Arc, Mutex};

    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Router;

    /// What the fake calendar server hands out at `/family.ics`; change it to change the calendar.
    pub type Served = Arc<Mutex<String>>;

    pub fn ics(events: &[(&str, &str)]) -> String {
        let mut out = String::from("BEGIN:VCALENDAR\r\nX-WR-CALNAME:Family\r\n");
        for (title, start) in events {
            out += &["BEGIN:VEVENT", &format!("SUMMARY:{title}"), &format!("DTSTART:{start}"), "END:VEVENT", ""]
                .join("\r\n");
        }
        out + "END:VCALENDAR\r\n"
    }

    /// Start a fake calendar server and return its address, such as `http://127.0.0.1:4821`.
    /// It serves a calendar at `/family.ics`, a web page at `/page`, an endless calendar at
    /// `/huge.ics`, and refuses `/secret.ics`.
    pub async fn server(served: Served) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/family.ics", get(move || async move { served.lock().unwrap().clone() }))
            .route("/page", get(|| async { "<html>sign in</html>" }))
            .route("/huge.ics", get(|| async { format!("BEGIN:VCALENDAR\r\n{}", "X".repeat(9 * 1024 * 1024)) }))
            .route("/secret.ics", get(|| async { StatusCode::FORBIDDEN }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{ics, server, Served};
    use super::*;
    use crate::store::testing::TempFrame;

    const OCTOBER: Window = Window { from: 1_790_812_800, to: 1_793_491_200 }; // 1 October to 1 November 2026, UTC

    fn link(base: &str, path: &str) -> CalendarLink {
        CalendarLink::local(&format!("{base}{path}"))
    }

    fn new_event(title: &str, day: &str, time: Option<&str>, repeat: Repeat) -> NewEvent {
        NewEvent {
            title: Title::parse(title).unwrap(),
            day: Day::parse(day).unwrap(),
            time: time.map(|t| ClockTime::parse(t).unwrap()),
            repeat,
        }
    }

    fn titles(calendars: &Calendars) -> Vec<String> {
        calendars.between(OCTOBER).iter().map(|e| e["title"].as_str().unwrap().to_string()).collect()
    }

    #[tokio::test]
    async fn a_subscribed_calendar_is_fetched_kept_and_survives_a_restart() {
        let frame = TempFrame::new("calendar-fetch");
        let served: Served = Arc::new(Mutex::new(ics(&[("Dentist", "20261013T140000Z")])));
        let base = server(Arc::clone(&served)).await;
        let links = [link(&base, "/family.ics")];
        let calendars = Arc::new(Calendars::load(&frame.0, &links));
        assert!(titles(&calendars).is_empty(), "nothing until it has been fetched");
        assert_eq!(calendars.feeds(true)[0]["fetched"], Value::Null);

        calendars.refresh(&reqwest::Client::new()).await;
        assert_eq!(titles(&calendars), ["Dentist"]);
        let feed = calendars.feeds(true)[0].clone();
        assert_eq!(
            (feed["name"].as_str(), feed["events"].as_u64(), &feed["error"]),
            (Some("Family"), Some(1), &Value::Null)
        );
        assert!(feed["fetched"].as_u64().unwrap() > 1_700_000_000);
        assert_eq!(feed["link"].as_str(), Some(links[0].as_str()));
        assert_eq!(calendars.feeds(false)[0]["link"], Value::Null, "the link is a secret");
        assert_eq!(calendars.feeds(false)[0]["host"].as_str(), base.strip_prefix("http://"));

        let event = &calendars.between(OCTOBER)[0];
        assert_eq!((event["start"].as_i64(), event["all_day"].as_bool()), (Some(1_791_900_000), Some(false)));
        assert_eq!((event["calendar"].as_str(), &event["added"]), (Some("Family"), &Value::Null));

        // The server goes wrong: what we had stays, with the reason.
        *served.lock().unwrap() = "<html>down for maintenance</html>".into();
        calendars.refresh(&reqwest::Client::new()).await;
        assert_eq!(titles(&calendars), ["Dentist"]);
        assert!(calendars.feeds(true)[0]["error"].as_str().unwrap().contains("does not lead to a calendar"));

        // After a restart the kept copy is used, before any fetch.
        let restarted = Calendars::load(&frame.0, &links);
        assert_eq!(titles(&restarted), ["Dentist"]);
        assert!(restarted.feeds(true)[0]["fetched"].as_u64().is_some());

        // Dropping the subscription drops the kept copy.
        restarted.set_links(&[]);
        assert!(restarted.feeds(true).is_empty());
        assert!(titles(&Calendars::load(&frame.0, &links)).is_empty());
    }

    #[tokio::test]
    async fn calendars_that_cannot_be_fetched_say_why_without_giving_the_link_away() {
        let frame = TempFrame::new("calendar-errors");
        let base = server(Served::default()).await;
        let links = [
            link(&base, "/page"),
            link(&base, "/secret.ics"),
            link(&base, "/huge.ics"),
            CalendarLink::local("http://127.0.0.1:1/SECRET-TOKEN.ics"),
        ];
        let calendars = Arc::new(Calendars::load(&frame.0, &links));
        calendars.refresh(&reqwest::Client::new()).await;
        let errors: Vec<String> =
            calendars.feeds(false).iter().map(|feed| feed["error"].as_str().unwrap().to_string()).collect();
        assert!(errors[0].contains("does not lead to a calendar"), "{errors:?}");
        assert!(errors[1].contains("status 403"), "{errors:?}");
        assert!(errors[2].contains("larger than 8 MB"), "{errors:?}");
        assert!(errors[3].contains("could not be reached"), "{errors:?}");
        assert!(!errors[3].contains("SECRET-TOKEN"), "{errors:?}");
        assert!(titles(&calendars).is_empty());
    }

    #[tokio::test]
    async fn changing_the_links_keeps_what_is_known_and_fetches_the_rest_at_once() {
        let frame = TempFrame::new("calendar-links");
        let served: Served = Arc::new(Mutex::new(ics(&[("Swim", "20261005T170000Z")])));
        let base = server(served).await;
        let calendars = Arc::new(Calendars::load(&frame.0, &[]));
        tokio::spawn(Arc::clone(&calendars).keep_fresh(reqwest::Client::new()));
        let (shared, links) = (Arc::clone(&calendars), vec![link(&base, "/family.ics"), link(&base, "/page")]);
        crate::app::blocking(move || shared.set_links(&links)).await;
        for _ in 0..200 {
            if !titles(&calendars).is_empty() && calendars.feeds(false)[1]["error"].is_string() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(titles(&calendars), ["Swim"], "fetched without waiting a quarter of an hour");
        let fetched = calendars.feeds(false)[0]["fetched"].clone();
        calendars.set_links(&[link(&base, "/page"), link(&base, "/family.ics")]);
        let feeds = calendars.feeds(false);
        assert_eq!((feeds.len(), &feeds[1]["fetched"]), (2, &fetched), "a known calendar keeps what it holds");
        assert_eq!(titles(&calendars), ["Swim"]);
    }

    /// Run by hand (`cargo test old_fashioned -- --ignored`): it needs the internet, and a
    /// server that still only offers TLS 1.2 with CBC ciphers.
    #[tokio::test]
    #[ignore = "needs the network and a particular server"]
    async fn a_server_with_old_fashioned_encryption_is_reached_by_the_tolerant_client() {
        let link = CalendarLink::parse("https://greenstreet.greenhousecms.co.uk/").unwrap();
        let usual = fetch(&crate::gphotos::client(), &link).await.unwrap_err();
        assert!(matches!(usual, FeedError::Unreachable(_)), "{usual}");
        println!("the usual client: {usual}");
        let tolerant = crate::gphotos::tolerant_client().expect("the tolerant client can be set up");
        // Its front page is not a calendar, but being told so means the connection was made.
        let reached = fetch(&tolerant, &link).await.unwrap_err();
        assert!(matches!(reached, FeedError::NotACalendar | FeedError::Refused(_)), "{reached}");
    }

    #[test]
    fn a_google_calendar_that_is_not_public_is_explained() {
        let shared =
            CalendarLink::parse("https://calendar.google.com/calendar/u/0?cid=c29tZW9uZUBnbWFpbC5jb20").unwrap();
        let explained = refusal(&shared, 404).to_string();
        assert!(
            explained.contains("no public calendar") && explained.contains("Secret address in iCal format"),
            "{explained}"
        );
        assert!(matches!(refusal(&shared, 500), FeedError::Refused(500)));
        let elsewhere = CalendarLink::parse("https://example.org/family.ics").unwrap();
        assert!(matches!(refusal(&elsewhere, 404), FeedError::Refused(404)));
    }

    #[test]
    fn an_added_event_can_come_every_day_or_monday_to_friday() {
        let frame = TempFrame::new("calendar-daily");
        let calendars = Calendars::load(&frame.0, &[]);
        calendars.add(new_event("Lunch", "2026-10-30", Some("12:00"), Repeat::Daily)).unwrap();
        calendars.add(new_event("Nursery", "2026-10-29", Some("09:00"), Repeat::Weekdays)).unwrap();
        let listed: Vec<String> = calendars
            .between(OCTOBER)
            .iter()
            .map(|e| format!("{} {}", e["title"].as_str().unwrap(), e["start"].as_str().unwrap()))
            .collect();
        // The nursery on Thursday 29 and Friday 30 October, not the weekend; lunch every day,
        // to 1 November, as clock times are matched a day generously.
        assert_eq!(
            listed,
            [
                "Nursery 2026-10-29T09:00:00",
                "Nursery 2026-10-30T09:00:00",
                "Lunch 2026-10-30T12:00:00",
                "Lunch 2026-10-31T12:00:00",
                "Lunch 2026-11-01T12:00:00",
            ]
        );
    }

    #[test]
    fn events_added_by_hand_are_listed_repeat_and_survive_a_restart() {
        let frame = TempFrame::new("calendar-added");
        let calendars = Calendars::load(&frame.0, &[]);
        let party = calendars.add(new_event("  Party ", "2026-10-17", Some("19:30"), Repeat::Never)).unwrap();
        calendars.add(new_event("Bins", "2026-10-01", None, Repeat::Weekly)).unwrap();
        calendars.add(new_event("Ana's birthday", "1990-10-13", None, Repeat::Yearly)).unwrap();
        calendars.add(new_event("Rent", "2026-01-28", Some("09:00"), Repeat::Monthly)).unwrap();
        calendars.add(new_event("Next year", "2027-03-01", None, Repeat::Never)).unwrap();

        let found = calendars.between(OCTOBER);
        let listed: Vec<(&str, &str)> =
            found.iter().map(|e| (e["title"].as_str().unwrap(), e["start"].as_str().unwrap())).collect();
        assert_eq!(
            listed,
            [
                ("Bins", "2026-10-01"),
                ("Bins", "2026-10-08"),
                ("Ana's birthday", "2026-10-13"),
                ("Bins", "2026-10-15"),
                ("Party", "2026-10-17T19:30:00"),
                ("Bins", "2026-10-22"),
                ("Rent", "2026-10-28T09:00:00"),
                ("Bins", "2026-10-29"),
            ]
        );
        let shown = found.iter().find(|e| e["title"] == "Party").unwrap();
        assert_eq!(
            (shown["all_day"].as_bool(), &shown["added"], &shown["calendar"]),
            (Some(false), &json!(party.id), &Value::Null)
        );
        assert_eq!(found[0]["end"], "2026-10-02", "a whole day ends the day after");

        let order: Vec<String> = calendars.added().iter().map(|e| e.title.as_str().to_string()).collect();
        assert_eq!(order, ["Ana's birthday", "Rent", "Bins", "Party", "Next year"], "by date");

        assert!(calendars.remove(&party.id));
        assert!(!calendars.remove(&party.id), "it is gone already");
        let restarted = Calendars::load(&frame.0, &[]);
        assert_eq!(restarted.added().len(), 4);
        assert!(!titles(&restarted).contains(&"Party".to_string()));
    }

    #[test]
    fn a_damaged_events_file_costs_only_the_damaged_entries() {
        let frame = TempFrame::new("calendar-damaged");
        let saved = json!([
            { "id": "0123456789abcdef", "title": "Kept", "day": "2026-10-13" },
            { "id": "not an id", "title": "Bad id", "day": "2026-10-13" },
            { "id": "0123456789abcde0", "title": "Bad day", "day": "13 October" },
            "nonsense",
        ]);
        fs::write(frame.0.data_dir.join("events.json"), saved.to_string()).unwrap();
        let calendars = Calendars::load(&frame.0, &[]);
        assert_eq!(titles(&calendars), ["Kept"]);
        assert_eq!((calendars.added()[0].time, calendars.added()[0].repeat), (None, Repeat::Never));
        fs::write(frame.0.data_dir.join("events.json"), "{{{").unwrap();
        assert!(Calendars::load(&frame.0, &[]).added().is_empty());
    }

    #[test]
    fn there_is_a_limit_to_how_many_events_can_be_added() {
        let frame = TempFrame::new("calendar-full");
        let calendars = Calendars::load(&frame.0, &[]);
        let full: Vec<Added> = (0..MAX_ADDED)
            .map(|n| Added {
                id: EventId::from_digest(&store::sha1(n.to_string().as_bytes())),
                title: Title::parse("x").unwrap(),
                day: Day::parse("2026-10-13").unwrap(),
                time: None,
                repeat: Repeat::Never,
            })
            .collect();
        *locked(&calendars.added) = full;
        let refused = calendars.add(new_event("One more", "2026-10-13", None, Repeat::Never)).unwrap_err();
        assert_eq!(refused, AddError::Full);
        assert!(refused.to_string().contains("already 2000 events"));
    }
}
