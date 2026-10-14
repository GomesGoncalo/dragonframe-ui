//! Work the server does by itself, for as long as it runs: daily syncs, looking at faces,
//! emptying the bin, night mode, and keeping watch over the frame's screen.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::app::{blocking, now_secs, Moment, Shared, DAY};
use crate::routes::photos::{start_sync, BIN_DAYS};
use crate::store::Settings;
use crate::types::Screen;
use crate::{device, faces, gphotos, index, night, store};

/// The face models are dropped after this long without use.
const MODELS_IDLE: Duration = Duration::from_mins(5);
/// Rest between photos nobody is waiting for, so background face checks stay gentle on the frame.
pub(crate) const BACKGROUND_PAUSE: Duration = Duration::from_secs(3);
/// How often the list of background work is rebuilt, to pick up new photos.
const REPLAN_EVERY: Duration = Duration::from_mins(15);

/// Work out which photos' faces are worth looking at in the background, and queue them.
pub(crate) async fn plan_face_checks(app: &Shared) {
    let shared = Arc::clone(app);
    let (todo, total) = blocking(move || index::people_to_look_at(&shared.cfg, &shared.index)).await;
    app.with_look_alikes.store(total, Ordering::Relaxed);
    debug!(todo = todo.len(), total, "planned background face checks");
    app.face_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).refill(todo);
}

/// Look at the faces in the next queued photo. Returns how long to rest before the next
/// one, or None when the queue is empty.
pub(crate) async fn check_next_face(app: &Shared) -> Option<Duration> {
    let next = {
        let mut queue = app.face_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.next(|photo| app.index.get(photo).people.is_some())
    }?;
    let (photo, pause) = match next {
        faces::Next::Watched(photo) => (photo, Duration::ZERO),
        faces::Next::Background(photo) => (photo, BACKGROUND_PAUSE),
    };
    let shared = Arc::clone(app);
    let outcome = blocking(move || {
        let models = shared.models.get(&shared.cfg).map_err(|e| store::PhotoError::Analysis(e.to_string()))?;
        let looked = index::look_at_people(&shared.cfg, &shared.index, &photo, &models);
        if looked.is_err() {
            // Do not come back to a photo that cannot be read: record that nobody was found.
            shared.index.set_people(&photo, faces::People::default());
        }
        looked.map(|_| ())
    })
    .await;
    if let Err(e) = outcome {
        debug!(error = %e, "could not look at the faces in a photo");
    }
    Some(pause)
}

/// Look at faces in the background, one photo at a time: first whatever is on someone's
/// screen, then, with a rest between photos, the other photos that have look-alikes.
pub(crate) async fn look_at_faces(app: Shared) {
    let mut planned: Option<Instant> = None;
    let mut since_save = 0u32;
    let save = |app: &Shared| {
        let shared = Arc::clone(app);
        blocking(move || shared.index.save())
    };
    loop {
        let cfg = Arc::clone(&app.cfg);
        if !blocking(move || faces::available(&cfg)).await {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        if planned.is_none_or(|at| at.elapsed() >= REPLAN_EVERY) {
            plan_face_checks(&app).await;
            planned = Some(Instant::now());
        }
        let Some(pause) = check_next_face(&app).await else {
            if since_save > 0 {
                save(&app).await;
                since_save = 0;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        since_save += 1;
        if since_save >= 25 {
            save(&app).await;
            since_save = 0;
        }
        tokio::time::sleep(pause).await;
    }
}

/// Give back the memory the face models use once nobody has compared photos for a while.
pub(crate) async fn unload_idle_models(app: Shared) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        if app.models.unload_if_idle(MODELS_IDLE) {
            info!("face models unloaded after being idle");
        }
    }
}

/// Once a day, empty photos from the bin that were deleted long enough ago.
pub(crate) async fn empty_old_bin_entries(app: Shared) {
    loop {
        let cfg = Arc::clone(&app.cfg);
        blocking(move || store::empty_bin(&cfg, Some(BIN_DAYS * DAY))).await;
        tokio::time::sleep(Duration::from_secs(DAY)).await;
    }
}

/// What time of day it is on the frame's own clock.
pub(crate) fn time_of_day(zone: Option<&str>, now_secs: u64) -> chrono::NaiveTime {
    let now =
        i64::try_from(now_secs).ok().and_then(|secs| chrono::DateTime::from_timestamp(secs, 0)).unwrap_or_default();
    match zone.and_then(|name| name.parse::<chrono_tz::Tz>().ok()) {
        Some(zone) => now.with_timezone(&zone).time(),
        None => now.time(),
    }
}

/// One look at whether the screen should be on, and the switch if it should change.
/// Only a screen that night mode switched off is switched back on by it.
pub(crate) async fn mind_the_night(app: &Shared, room: &mut night::Room) {
    let shared = Arc::clone(app);
    let (settings, zone, light, on, untouched) = blocking(move || {
        let device = shared.device;
        let settings = store::load_settings(&shared.cfg).night;
        // The sensor is only worth asking when its answer could matter.
        let light = settings.when_dark.then(|| device.room_light()).flatten();
        let (on, untouched) = device.power();
        (settings, device.time_zone(), light, on, untouched)
    })
    .await;
    let now = Moment::now();
    room.note(light, now);
    let time = time_of_day(zone.as_deref(), now_secs());
    // The screen page may need the display: a timer counting down, or an alarm about to ring.
    let needed = {
        let mut watch = app.watch();
        if night::alarm_due(watch.wake_at, now_secs()) {
            let after = now + night::ON_AFTER_AN_ALARM;
            watch.stay_on_until = Some(watch.stay_on_until.map_or(after, |until| until.max(after)));
        }
        watch.stay_on_until.is_some_and(|until| until > now)
    };
    let off = !needed && night::screen_should_be_off(&settings, time, room.dark_for(now), untouched);
    let we_switched_it_off = app.watch().asleep;
    let device = app.device;
    if needed && on == Some(false) {
        // Whoever switched it off, an alarm or a running timer wants it on.
        blocking(move || device.switch_screen(true)).await;
        let mut watch = app.watch();
        watch.asleep = false;
        watch.heard = Some(now);
        watch.shown = watch.shown.map(|_| now);
        info!("the screen was switched on for an alarm or a timer");
    } else if off && on == Some(true) {
        blocking(move || device.switch_screen(false)).await;
        app.watch().asleep = true;
        info!("night mode switched the screen off");
    } else if !off && we_switched_it_off {
        blocking(move || device.switch_screen(true)).await;
        let mut watch = app.watch();
        watch.asleep = false;
        // The new screen was not expected to say anything while dark; start listening afresh.
        watch.heard = Some(now);
        watch.shown = watch.shown.map(|_| now);
        info!("night mode switched the screen back on");
    }
}

/// Night mode: switch the screen off for the night and when the room is dark, and on again afterwards.
pub(crate) async fn keep_night(app: Shared) {
    let mut room = night::Room::default();
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        mind_the_night(&app, &mut room).await;
    }
}

/// If the new screen stops saying it is running, put the frame back on the screen it came
/// with, so a page that fails to load never leaves the frame showing nothing. If instead the
/// frame's own app has pushed itself in front of a new screen that is still running, put the
/// new screen back: that app does so by itself now and then, and covered, the new screen goes quiet too.
pub(crate) async fn fall_back_if_quiet(app: Shared) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let (expected, quiet) = {
            let watch = app.watch();
            let since = |at: Option<Moment>| at.map(|at| at.elapsed());
            match (watch.in_wifi_settings || watch.asleep, since(watch.shown), since(watch.heard)) {
                (false, Some(shown_for), Some(since_heard)) => (true, device::gone_quiet(shown_for, since_heard)),
                _ => (false, false),
            }
        };
        // Quiet comes first: a new screen that cannot start leaves the frame's own app in front
        // too, and putting it back again and again would never end.
        let device = (app.device, app.port);
        if expected && !quiet && blocking(move || device.0.own_screen_in_front()).await {
            warn!("the frame's own screen came to the front by itself; putting the new screen back");
            blocking(move || device.0.show(Screen::New, device.1)).await;
            continue;
        }
        if quiet {
            warn!("the new screen has gone quiet; putting the frame back on its own screen");
            blocking(move || device.0.show(Screen::Legacy, device.1)).await;
            let mut watch = app.watch();
            watch.shown = None;
            watch.fell_back = true;
        }
    }
}

/// When the server starts and the frame is set to show the new screen, put it in front
/// once the frame has finished starting up.
pub(crate) async fn show_chosen_screen(app: Shared) {
    let cfg = Arc::clone(&app.cfg);
    if blocking(move || store::load_settings(&cfg).screen).await != Screen::New {
        return;
    }
    for _ in 0..120 {
        let device = app.device;
        if blocking(move || device.started()).await {
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let shown = blocking(move || app.show(Screen::New)).await;
    info!(shown, "the new screen was put in front");
}

/// The sync to start now, if the user wants daily syncs and the last one was a day or more ago.
pub(crate) fn daily_sync_due(settings: Settings, now: u64) -> Option<gphotos::Options> {
    let last = settings.last_sync.map_or(0, |last| last.at);
    let due = settings.daily_sync && now.saturating_sub(last) >= DAY && !settings.albums.is_empty();
    due.then_some(gphotos::Options { albums: settings.albums, delete: false, refresh: true, limit: None })
}

/// Once a day, if the user asked for it, sync the saved albums.
pub(crate) async fn auto_sync(app: Shared) {
    loop {
        tokio::time::sleep(Duration::from_secs(600)).await;
        let cfg = Arc::clone(&app.cfg);
        let settings = blocking(move || store::load_settings(&cfg)).await;
        if let Some(options) = daily_sync_due(settings, now_secs()) {
            info!(albums = options.albums.len(), "starting the daily album sync");
            start_sync(&app, options);
        }
    }
}

/// Start everything the server does by itself. Each task runs until the server stops.
pub(crate) async fn start(app: &Shared) {
    // With its screen off the frame would otherwise stop its processor, and nothing here could run.
    let device = app.device;
    blocking(move || device.stay_running()).await;
    tokio::spawn(index::maintain(Arc::clone(&app.cfg), Arc::clone(&app.index)));
    tokio::spawn(auto_sync(Arc::clone(app)));
    tokio::spawn(unload_idle_models(Arc::clone(app)));
    tokio::spawn(look_at_faces(Arc::clone(app)));
    tokio::spawn(empty_old_bin_entries(Arc::clone(app)));
    tokio::spawn(Arc::clone(&app.calendars).keep_fresh(app.http.clone()));
    tokio::spawn(Arc::clone(&app.weather).keep_fresh(app.http.clone(), Arc::clone(&app.cfg)));
    tokio::spawn(show_chosen_screen(Arc::clone(app)));
    tokio::spawn(fall_back_if_quiet(Arc::clone(app)));
    tokio::spawn(keep_night(Arc::clone(app)));
    tokio::spawn(Arc::clone(&app.spotify).run(Arc::clone(&app.cfg)));
}
