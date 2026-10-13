//! What every part of the server shares: where things are kept, what is known about the
//! photos, the sync job, the calendars, the weather, and the frame itself.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::calendar::Calendars;
use crate::device::Device;
use crate::home::Home;
use crate::index::Index;
use crate::marks::Marks;
use crate::store::{Config, Settings};
use crate::todo::Todo;
use crate::travel::Travel;
use crate::types::Screen;
use crate::weather::Weather;
use crate::{auth, faces, gphotos, store, weather};

pub(crate) const DAY: u64 = 24 * 60 * 60;

pub(crate) struct App {
    pub(crate) cfg: Arc<Config>,
    pub(crate) index: Arc<Index>,
    /// Which photos are favourites, and which are hidden from the slideshow.
    pub(crate) marks: Marks,
    pub(crate) job: Arc<Mutex<gphotos::Job>>,
    pub(crate) guard: auth::Guard,
    pub(crate) http: reqwest::Client,
    pub(crate) started: Instant,
    /// Serialises read-modify-write of the settings file.
    pub(crate) settings_lock: Mutex<()>,
    /// The face models, held only while there are faces to look at.
    pub(crate) models: faces::OnDemand,
    /// Which photos to look at next.
    pub(crate) face_queue: Mutex<faces::Queue>,
    /// How many photos have look-alikes, as of the last time the background work was planned.
    pub(crate) with_look_alikes: AtomicUsize,
    /// Where album listings come from: Google, or a stand-in during tests.
    pub(crate) rpc_url: String,
    /// Subscribed calendars and the events added by hand.
    pub(crate) calendars: Arc<Calendars>,
    /// The frame itself: its Wi-Fi and which screen is in front.
    pub(crate) device: Device,
    /// The port this server answers on, which the frame's browser is pointed at.
    pub(crate) port: u16,
    pub(crate) watch: Mutex<ScreenWatch>,
    pub(crate) weather: Arc<Weather>,
    /// What the house's relay has said about the doorbell, cameras, alarm and sensors.
    pub(crate) home: Home,
    /// Working out when to leave for events.
    pub(crate) travel: Travel,
    /// Things to do, who does them, and the points they earn.
    pub(crate) todo: Todo,
    /// How many times the calendar has been changed from the page (an event, a calendar, a
    /// rule, a trip), so the screen can tell there is something new to fetch.
    pub(crate) calendar_changes: AtomicU64,
    /// The same for the things to do.
    pub(crate) todo_changes: AtomicU64,
}

/// A moment on the clock that waiting is measured by (tokio's, which tests can move forward).
pub(crate) type Moment = tokio::time::Instant;

/// What the server knows about the new screen while the frame is meant to be showing it.
#[derive(Default)]
pub(crate) struct ScreenWatch {
    /// When the new screen was last put in front; None while the frame shows its own.
    pub(crate) shown: Option<Moment>,
    /// When the new screen last said it is running.
    pub(crate) heard: Option<Moment>,
    /// Android's Wi-Fi settings are in front, so the new screen cannot be heard from.
    pub(crate) in_wifi_settings: bool,
    /// The new screen went quiet and the frame was put back on its own screen.
    pub(crate) fell_back: bool,
    /// Night mode has switched the screen off, so nothing is expected of the new screen.
    pub(crate) asleep: bool,
    /// The screen page asked for the display to stay on until then (a timer is counting down).
    pub(crate) stay_on_until: Option<Moment>,
    /// The screen page asked for the display to be on at this moment, in seconds since 1970 (an alarm).
    pub(crate) wake_at: Option<u64>,
}

pub(crate) type Shared = Arc<App>;

impl App {
    pub(crate) fn new(cfg: Config, device: Device, port: u16) -> Shared {
        let index = Index::load(&cfg);
        let marks = Marks::load(&cfg);
        let todo = Todo::load(&cfg);
        let calendars =
            Calendars::load(&cfg, &store::load_settings(&cfg).calendars).falling_back_to(gphotos::tolerant_client());
        Arc::new(App {
            cfg: Arc::new(cfg),
            index: Arc::new(index),
            marks,
            job: Arc::default(),
            guard: auth::Guard::default(),
            http: gphotos::client(),
            started: Instant::now(),
            settings_lock: Mutex::new(()),
            models: faces::OnDemand::default(),
            face_queue: Mutex::default(),
            with_look_alikes: AtomicUsize::new(0),
            rpc_url: gphotos::RPC_URL.to_string(),
            calendars: Arc::new(calendars),
            device,
            port,
            watch: Mutex::default(),
            weather: Arc::new(Weather::new(weather::FORECAST_URL, weather::PLACES_URL)),
            home: Home::default(),
            travel: Travel::default(),
            todo,
            calendar_changes: AtomicU64::new(0),
            todo_changes: AtomicU64::new(0),
        })
    }

    /// Change the settings file atomically with respect to other requests. Blocking.
    pub(crate) fn update_settings<T>(&self, change: impl FnOnce(&mut Settings) -> T) -> T {
        let _guard = self.settings_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut settings = store::load_settings(&self.cfg);
        let result = change(&mut settings);
        store::save_settings(&self.cfg, &settings);
        result
    }

    /// Note that what the calendar shows has changed.
    pub(crate) fn calendar_changed(&self) {
        self.calendar_changes.fetch_add(1, Ordering::Relaxed);
    }

    /// Note that the things to do have changed.
    pub(crate) fn todo_changed(&self) {
        self.todo_changes.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn watch(&self) -> std::sync::MutexGuard<'_, ScreenWatch> {
        self.watch.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Put a screen in front and start, or stop, expecting to hear from the new one. Blocking.
    pub(crate) fn show(&self, screen: Screen) -> bool {
        let shown = self.device.show(screen, self.port);
        let mut watch = self.watch();
        watch.shown = (screen == Screen::New).then(Moment::now);
        watch.heard = watch.shown;
        watch.fell_back = false;
        shown
    }

    pub(crate) fn sync_context(&self) -> gphotos::Context {
        gphotos::Context {
            cfg: Arc::clone(&self.cfg),
            index: Arc::clone(&self.index),
            job: Arc::clone(&self.job),
            http: self.http.clone(),
            rpc_url: self.rpc_url.clone(),
        }
    }
}

/// Run blocking work (file access, image decoding, Android commands) off the async threads.
///
/// # Panics
/// If the work itself panics.
pub(crate) async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work).await.expect("a blocking task panicked")
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
