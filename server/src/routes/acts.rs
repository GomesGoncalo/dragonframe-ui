//! Telling the frame's screen to do things, and the music it can play: radio stations, and
//! Spotify when someone chooses the frame in their Spotify app.

use std::collections::HashMap;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tracing::info;

use crate::app::{blocking, Shared};
use crate::routes::{ApiError, ApiResult, Asked, Parsed, NO_STORE};
use crate::spotify::{wav_header, Order};
use crate::store;
use crate::types::{Action, PlaceQuery, Said, Stations};

/// A request for what to do is held open this long when there is nothing to say. Shorter
/// than the half minute after which browsers and the things between tend to give up.
const HELD_FOR: Duration = Duration::from_secs(25);

#[derive(Deserialize)]
pub(crate) struct StationsRequest {
    pub(crate) stations: Stations,
}

#[derive(Deserialize)]
pub(crate) struct SpotifyRequest {
    pub(crate) on: bool,
}

/// Have the frame's screen do something. The screen hears of it within a moment, if it is
/// running; the answer says whether it is.
pub(crate) async fn act(State(app): State<Shared>, Parsed(action): Parsed<Action>) -> ApiResult {
    if let Some(wanted) = action.station() {
        let cfg = std::sync::Arc::clone(&app.cfg);
        let name = wanted.as_str().to_lowercase();
        let known = blocking(move || {
            store::load_settings(&cfg).stations.iter().any(|s| s.name.as_str().to_lowercase() == name)
        })
        .await;
        if !known {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                format!("there is no station called {:?}", wanted.as_str()),
            ));
        }
    }
    info!(?action, "the screen was told to do something");
    let number = app.acts.tell(action);
    Ok(Json(json!({ "told": number, "listening": app.acts.listening() })))
}

/// What the screen is to do, asked by the screen page itself: `?after=` is the number of
/// the last action it has had, and the answer waits until there is a later one (or for 25
/// seconds). With the question the page says how it stands: `?tab=`, `?paused=1`,
/// `?playing=<station>`, `?volume=<0 to 100>`.
pub(crate) async fn acts_to_do(State(app): State<Shared>, Query(query): Query<HashMap<String, String>>) -> Json<Value> {
    app.acts.heard(Said::from_query(|name| query.get(name).cloned()));
    let after = query.get("after").and_then(|number| number.parse().ok());
    // The answer also says how Spotify stands, and does not wait when that is news to the
    // page (`?spotify=` is the version of it the page has): a track that is started or
    // paused from someone's phone has to start or stop sounding here at once.
    let seen = query.get("spotify").and_then(|version| version.parse().ok());
    let mut reply = tokio::select! {
        reply = app.acts.after(after, HELD_FOR) => reply,
        () = app.spotify.news_since(seen.unwrap_or_default()), if seen.is_some() && after.is_some() => {
            app.acts.after(after, Duration::ZERO).await
        }
    };
    reply["spotify"] = app.spotify.view();
    Json(reply)
}

/// How the screen stands (what is playing, which tab is open) and the stations there are,
/// for the phone page's remote control.
pub(crate) async fn acts_view(State(app): State<Shared>) -> Json<Value> {
    let cfg = std::sync::Arc::clone(&app.cfg);
    let (stations, spotify_on) = blocking(move || {
        let settings = store::load_settings(&cfg);
        (settings.stations, settings.spotify)
    })
    .await;
    let mut view = app.acts.view();
    view["stations"] = json!(stations);
    view["spotify"] = app.spotify.view();
    view["spotify"]["on"] = json!(spotify_on);
    Json(view)
}

/// Switch on or off whether Spotify's apps may play on the frame.
pub(crate) async fn set_spotify(State(app): State<Shared>, Parsed(request): Parsed<SpotifyRequest>) -> Json<Value> {
    let (shared, on) = (std::sync::Arc::clone(&app), request.on);
    blocking(move || shared.update_settings(|settings| settings.spotify = on)).await;
    app.spotify.setting_changed();
    info!(on, "Spotify on the frame was switched");
    Json(json!({ "on": on }))
}

/// An order for the Spotify player (`{"do": "pause"}`), from the buttons on the frame's
/// screen. It takes no PIN: it does nothing a touch on the phone that is playing would not.
pub(crate) async fn spotify_order(State(app): State<Shared>, Parsed(order): Parsed<Order>) -> ApiResult {
    if !app.spotify.order(order) {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Spotify is not running on the frame"));
    }
    Ok(Json(json!({ "done": true })))
}

/// What Spotify is playing, as sound: a WAV file that never ends, for the screen page to
/// play as it plays a radio station. It starts with whatever is played from now on.
pub(crate) async fn spotify_stream(State(app): State<Shared>) -> Response {
    let sound = app.spotify.listen();
    let pieces = futures_util::stream::unfold((Some(wav_header()), sound), |(header, mut sound)| async move {
        if let Some(header) = header {
            return Some((Ok::<_, std::convert::Infallible>(header), (None, sound)));
        }
        loop {
            match sound.recv().await {
                Ok(piece) => return Some((Ok(piece), (None, sound))),
                // Fell behind: what was missed is skipped, and the listening goes on.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return None,
            }
        }
    });
    let headers = [(CONTENT_TYPE, HeaderValue::from_static("audio/wav")), (CACHE_CONTROL, NO_STORE)];
    (headers, Body::from_stream(pieces)).into_response()
}

/// Keep exactly these radio stations. The frame's screen picks them up within half a minute.
pub(crate) async fn save_stations(State(app): State<Shared>, Parsed(request): Parsed<StationsRequest>) -> Json<Value> {
    let stations = request.stations.into_inner();
    let count = stations.len();
    blocking(move || app.update_settings(|settings| settings.stations = stations)).await;
    info!(stations = count, "radio stations saved");
    Json(json!({ "stations": count }))
}

/// What was typed to look for a radio station: `?name=`.
#[derive(Deserialize)]
pub(crate) struct StationQuery {
    pub(crate) name: PlaceQuery,
}

/// Radio stations whose names have what was typed, to choose one from.
pub(crate) async fn find_stations(State(app): State<Shared>, Asked(query): Asked<StationQuery>) -> ApiResult {
    let found = app
        .radio
        .find(&app.http, &query.name)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(json!({ "stations": found })))
}
