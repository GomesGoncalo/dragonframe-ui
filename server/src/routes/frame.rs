//! The frame itself: its Wi-Fi, which screen it shows, how that screen behaves (slideshow,
//! night, chime), and the weather.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::app::{blocking, Moment, Shared};
use crate::routes::{ApiError, ApiResult, Parsed, NO_STORE};
use crate::types::{Chime, Night, PlaceQuery, Screen, Slideshow, Vigil};
use crate::{store, weather};

/// Android's Wi-Fi settings are left again once the frame has not been touched for this long.
const WIFI_SETTINGS_IDLE: Duration = Duration::from_mins(1);
/// However long someone stays in Android's Wi-Fi settings, the frame comes back after this.
const WIFI_SETTINGS_LONGEST: Duration = Duration::from_mins(15);

#[derive(Deserialize)]
pub(crate) struct ScreenRequest {
    pub(crate) show: Screen,
}

#[derive(Deserialize)]
pub(crate) struct ChimeRequest {
    pub(crate) before: Chime,
}

#[derive(Deserialize)]
pub(crate) struct PlaceRequest {
    pub(crate) name: PlaceQuery,
}

/// The frame's Wi-Fi, which screen it is set to show, and how its slideshow runs.
pub(crate) async fn device_view(State(app): State<Shared>) -> Json<Value> {
    let (fell_back, asleep) = {
        let watch = app.watch();
        (watch.fell_back, watch.asleep)
    };
    Json(
        blocking(move || {
            let settings = store::load_settings(&app.cfg);
            let wifi = app.device.wifi();
            json!({
                "simulated": app.device.is_simulated(),
                "pin": settings.protection.is_some(),
                "reminders": settings.reminders,
                // Whether the house is connected (a key was given out), and whether its relay is in touch.
                "home": settings.home_key.is_some(),
                "home_in_touch": app.home.view()["connected"],
                // Whether Yoto Limiter is read (a key is kept), and how that is going.
                "yoto": settings.yoto_key.is_some(),
                "yoto_now": app.yoto.view(),
                "address": wifi.as_ref().and_then(|wifi| wifi.address.as_deref()).map(|at| manage_address(at, app.port)),
                "wifi": wifi,
                "screen": settings.screen,
                "fell_back": fell_back,
                "slideshow": settings.slideshow,
                "night": settings.night,
                "asleep": asleep,
                "light": app.device.room_light().map(|light| light.lux),
                "chime": settings.chime,
                "place": settings.place.map(|place| place.name),
                "stations": settings.stations,
            })
        })
        .await,
    )
}

/// Where a phone or computer on the same network finds this app.
pub(crate) fn manage_address(frame: &str, port: u16) -> String {
    format!("http://{frame}:{port}")
}

/// That address as a QR code, for a phone's camera. There is none while the frame is off Wi-Fi.
pub(crate) async fn address_code(State(app): State<Shared>) -> ApiResult<Response> {
    let address = blocking(move || {
        let wifi = app.device.wifi()?;
        Some(manage_address(&wifi.address?, app.port))
    })
    .await
    .ok_or_else(ApiError::not_found)?;
    let code = qrcode::QrCode::new(address.as_bytes()).map_err(|_| ApiError::not_found())?;
    let picture = code.render::<qrcode::render::svg::Color>().min_dimensions(240, 240).build();
    let headers = [(CONTENT_TYPE, HeaderValue::from_static("image/svg+xml")), (CACHE_CONTROL, NO_STORE)];
    Ok((headers, picture).into_response())
}

/// The new screen calls this every half minute to say it is running, and learns how it
/// should behave. If the calls stop, the frame goes back to its own screen.
///
/// With it the screen says what it needs of the display: `?busy=<seconds>` to keep it on
/// (a timer is counting down) and `?wake=<seconds since 1970>` to have it on then (an alarm).
/// Night mode honours both.
pub(crate) async fn alive(State(app): State<Shared>, Query(query): Query<HashMap<String, String>>) -> Json<Value> {
    let asked = |name: &str| query.get(name).map(String::as_str);
    let vigil = Vigil::from_query(asked("busy"), asked("wake"));
    let asleep = {
        let mut watch = app.watch();
        let now = Moment::now();
        watch.heard = Some(now);
        watch.stay_on_until = vigil.stay_on.map(|long| now + long);
        watch.wake_at = vigil.wake_at;
        watch.asleep
    };
    let cfg = Arc::clone(&app.cfg);
    let settings = blocking(move || store::load_settings(&cfg)).await;
    // When the weather it would get was fetched, so the screen knows at once when there is news.
    let weather = app.weather.view(settings.place.as_ref())["fetched"].clone();
    Json(json!({
        "slideshow": settings.slideshow,
        "chime": settings.chime,
        "asleep": asleep,
        "weather": weather,
        "home": settings.home_key.is_some(),
        // Yoto, in Utilities, likewise once Yoto Limiter can be read.
        "yoto": settings.yoto_key.is_some(),
        // Whether changing anything, a photo's mark included, takes the PIN.
        "pin": settings.protection.is_some(),
        "reminders": settings.reminders,
        "stations": settings.stations,
        // Counts changes to the calendar, so the screen fetches it when there is news.
        "calendar": app.calendar_changes.load(std::sync::atomic::Ordering::Relaxed),
        "todo": crate::routes::todo::changes(&app),
    }))
}

/// Choose which screen the frame shows, and put it in front.
pub(crate) async fn set_screen(State(app): State<Shared>, Parsed(request): Parsed<ScreenRequest>) -> Json<Value> {
    let screen = request.show;
    let shown = blocking(move || {
        app.update_settings(|settings| settings.screen = screen);
        app.show(screen)
    })
    .await;
    info!(?screen, shown, "the frame's screen was chosen");
    Json(json!({ "screen": screen, "shown": shown }))
}

pub(crate) async fn set_slideshow(State(app): State<Shared>, Parsed(slideshow): Parsed<Slideshow>) -> Json<Value> {
    blocking(move || app.update_settings(|settings| settings.slideshow = slideshow)).await;
    Json(json!({ "slideshow": slideshow }))
}

/// Set when the screen is switched off at night. The night task acts on it within half a minute.
pub(crate) async fn set_night(State(app): State<Shared>, Parsed(night): Parsed<Night>) -> Json<Value> {
    blocking(move || app.update_settings(|settings| settings.night = night)).await;
    info!(on = night.on, from = %night.from, until = %night.until, when_dark = night.when_dark, "night mode set");
    Json(json!({ "night": night }))
}

pub(crate) async fn set_chime(State(app): State<Shared>, Parsed(request): Parsed<ChimeRequest>) -> Json<Value> {
    let chime = request.before;
    blocking(move || app.update_settings(|settings| settings.chime = chime)).await;
    Json(json!({ "chime": chime }))
}

/// The weather where the frame is, if a place has been chosen and its weather fetched.
pub(crate) async fn weather_view(State(app): State<Shared>) -> Json<Value> {
    let cfg = Arc::clone(&app.cfg);
    let place = blocking(move || store::load_settings(&cfg).place).await;
    Json(app.weather.view(place.as_ref()))
}

/// Say where the frame is, by the name of a town. It is looked up, kept, and its weather fetched.
pub(crate) async fn set_place(State(app): State<Shared>, Parsed(request): Parsed<PlaceRequest>) -> ApiResult {
    let place = app.weather.find(&app.http, &request.name).await.map_err(|e| {
        let status =
            if e == weather::WeatherError::NoSuchPlace { StatusCode::NOT_FOUND } else { StatusCode::BAD_GATEWAY };
        ApiError::new(status, e.to_string())
    })?;
    let (shared, chosen) = (Arc::clone(&app), place.clone());
    blocking(move || shared.update_settings(|settings| settings.place = Some(chosen))).await;
    info!(place = place.name, "place for the weather chosen");
    if let Err(e) = app.weather.refresh(&app.http, &place).await {
        warn!(error = %e, "could not fetch the weather for the new place yet");
        app.weather.place_changed();
    }
    Ok(Json(json!({ "place": place.name })))
}

/// Stop showing the weather.
pub(crate) async fn forget_place(State(app): State<Shared>) -> Json<Value> {
    blocking(move || app.update_settings(|settings| settings.place = None)).await;
    Json(json!({ "place": null }))
}

/// Open Android's Wi-Fi settings on the frame. The frame has no back button, so the server
/// brings its screen back once the frame has been left alone for a minute.
pub(crate) async fn open_wifi_settings(State(app): State<Shared>) -> ApiResult {
    let device = app.device;
    if !blocking(move || device.open_wifi_settings()).await {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "the Wi-Fi settings could not be opened"));
    }
    let already_there = std::mem::replace(&mut app.watch().in_wifi_settings, true);
    if !already_there {
        tokio::spawn(leave_wifi_settings(app));
    }
    Ok(Json(json!({ "opened": true, "returns_after_secs": WIFI_SETTINGS_IDLE.as_secs() })))
}

/// Wait until nobody has touched the frame for a while, then put its screen back in front.
pub(crate) async fn leave_wifi_settings(app: Shared) {
    let opened = Moment::now();
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let device = app.device;
        let untouched = blocking(move || device.untouched_for()).await;
        // If the frame will not say when it was touched, only the longest stay ends this.
        if untouched.is_some_and(|idle| idle >= WIFI_SETTINGS_IDLE) || opened.elapsed() >= WIFI_SETTINGS_LONGEST {
            break;
        }
    }
    let shared = Arc::clone(&app);
    let screen = blocking(move || {
        let screen = store::load_settings(&shared.cfg).screen;
        shared.show(screen);
        screen
    })
    .await;
    app.watch().in_wifi_settings = false;
    info!(?screen, "left Android's Wi-Fi settings");
}
