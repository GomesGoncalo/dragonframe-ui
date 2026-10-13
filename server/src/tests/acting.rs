//! Route tests: telling the screen to do things, the radio stations, and Spotify.

use futures_util::StreamExt as _;

use super::*;

const STATIONS: &str = r#"{"stations":[
    {"name":"Radio Four","stream":"https://stream.example/four.mp3"},
    {"name":"Jazz","stream":"http://stream.example:8000/jazz"}]}"#;

#[tokio::test]
async fn the_screen_is_handed_what_it_was_told_to_do_since_it_last_asked() {
    let (app, _frame) = app("acts");
    // A page that has just loaded learns where to count from.
    let first = get(&app, "/api/acts").await.json();
    assert_eq!((&first["last"], &first["acts"]), (&json!(0), &json!([])));
    let told = post(&app, "/api/act", r#"{"do":"next"}"#).await;
    assert_eq!((told.status, told.json()), (StatusCode::OK, json!({ "told": 1, "listening": true })));
    post(&app, "/api/act", r#"{"do":"show","tab":"calendar"}"#).await;
    post(&app, "/api/act", r#"{"do":"timer","minutes":10}"#).await;
    let handed = get(&app, "/api/acts?after=0").await.json();
    assert_eq!(handed["last"], 3);
    assert_eq!(
        handed["acts"],
        json!([{ "do": "next" }, { "do": "show", "tab": "calendar" }, { "do": "timer", "minutes": 10 }])
    );
    assert_eq!(get(&app, "/api/acts?after=2").await.json()["acts"], json!([{ "do": "timer", "minutes": 10 }]));
}

#[tokio::test]
async fn an_action_that_makes_no_sense_is_refused() {
    let (app, _frame) = app("acts-refused");
    for bad in ["{}", r#"{"do":"explode"}"#, r#"{"do":"show","tab":"attic"}"#, r#"{"do":"volume","to":300}"#] {
        assert_eq!(post(&app, "/api/act", bad).await.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let unknown = post(&app, "/api/act", r#"{"do":"play","station":"Jazz"}"#).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert!(unknown.json()["error"].as_str().unwrap().contains("no station called"));
    assert_eq!(get(&app, "/api/acts").await.json()["last"], 0, "nothing was told");
}

#[tokio::test]
async fn telling_the_screen_takes_the_pin_when_one_is_set() {
    let (app, _frame) = app("acts-pin");
    let set = post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    assert_eq!(set.status, StatusCode::OK);
    assert_eq!(post(&app, "/api/act", r#"{"do":"next"}"#).await.status, StatusCode::UNAUTHORIZED);
    let token = set.json()["token"].as_str().unwrap().to_string();
    assert_eq!(post_as(&app, &token, "/api/act", r#"{"do":"next"}"#).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_screen_says_how_it_stands_and_the_phone_page_can_ask() {
    let (app, _frame) = app("acting");
    let quiet = get(&app, "/api/acting").await.json();
    assert_eq!((&quiet["listening"], &quiet["said"], &quiet["stations"]), (&json!(false), &Value::Null, &json!([])));
    let unheard = post(&app, "/api/act", r#"{"do":"pause"}"#).await.json();
    assert_eq!(unheard["listening"], false, "the page is told the screen is not there to hear it");
    get(&app, "/api/acts?tab=utilities&paused=1&playing=Radio%20Four&volume=60").await;
    let acting = get(&app, "/api/acting").await.json();
    assert_eq!(acting["listening"], true);
    assert_eq!(acting["said"], json!({ "tab": "utilities", "paused": true, "playing": "Radio Four", "volume": 60 }));
}

#[tokio::test]
async fn stations_are_kept_and_can_then_be_asked_for_by_name() {
    let (app, frame) = app("stations");
    assert_eq!(get(&app, "/api/device").await.json()["stations"], json!([]));
    let saved = post(&app, "/api/stations", STATIONS).await;
    assert_eq!((saved.status, saved.json()), (StatusCode::OK, json!({ "stations": 2 })));
    assert_eq!(store::load_settings(&frame.0).stations.len(), 2);
    let listed = json!([
        { "name": "Radio Four", "stream": "https://stream.example/four.mp3" },
        { "name": "Jazz", "stream": "http://stream.example:8000/jazz" },
    ]);
    assert_eq!(get(&app, "/api/device").await.json()["stations"], listed);
    assert_eq!(get(&app, "/api/alive").await.json()["stations"], listed, "the screen learns of them");
    assert_eq!(get(&app, "/api/acting").await.json()["stations"], listed);

    // Capitals do not matter when a station is asked for; and it need not be named at all.
    assert_eq!(post(&app, "/api/act", r#"{"do":"play","station":"radio four"}"#).await.status, StatusCode::OK);
    assert_eq!(post(&app, "/api/act", r#"{"do":"play"}"#).await.status, StatusCode::OK);
    assert_eq!(post(&app, "/api/act", r#"{"do":"play","station":"Rock"}"#).await.status, StatusCode::NOT_FOUND);

    for bad in [
        r#"{"stations":[{"name":"","stream":"https://a/b"}]}"#,
        r#"{"stations":[{"name":"Jazz","stream":"jazz.mp3"}]}"#,
        r#"{"stations":[{"name":"Jazz","stream":"https://a/b"},{"name":"JAZZ","stream":"https://a/c"}]}"#,
        "{}",
    ] {
        assert_eq!(post(&app, "/api/stations", bad).await.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(store::load_settings(&frame.0).stations.len(), 2, "a refused list changes nothing");
    post(&app, "/api/stations", r#"{"stations":[]}"#).await;
    assert!(store::load_settings(&frame.0).stations.is_empty());
}

#[tokio::test]
async fn stations_are_looked_up_by_name_in_the_directory() {
    let directory = radio::testing::directory().await;
    let frame = TempFrame::new("route-station-search");
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    let mut app = Arc::try_unwrap(App::new(cfg, Device::Simulated, 8080)).ok().expect("a new app has one owner");
    app.radio = radio::Radio::new(&directory);
    let app = Arc::new(app);

    let found = get(&app, "/api/stations/find?name=Paradise").await;
    assert_eq!(found.status, StatusCode::OK);
    let stations = found.json()["stations"].as_array().unwrap().clone();
    assert_eq!(stations.len(), 2, "only the ones the frame can play");
    assert_eq!(stations[0]["name"], "Radio Paradise Main Mix");
    assert_eq!(get(&app, "/api/stations/find?name=Nowhere").await.json(), json!({ "stations": [] }));
    assert_eq!(get(&app, "/api/stations/find").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(get(&app, "/api/stations/find?name=%20").await.status, StatusCode::BAD_REQUEST);
    let broken = get(&app, "/api/stations/find?name=broken").await;
    assert_eq!(broken.status, StatusCode::BAD_GATEWAY);
    assert!(broken.json()["error"].as_str().unwrap().contains("503"));
}

#[tokio::test]
async fn a_reminder_rule_can_have_the_frame_do_something() {
    let (app, frame) = app("rule-acts");
    let rules = r#"{"rules":[{"words":"wake up","when":{"minutes":0},"then":{"do":"play","station":"Jazz"}}]}"#;
    assert_eq!(post(&app, "/api/reminders", rules).await.status, StatusCode::OK);
    assert_eq!(store::load_settings(&frame.0).reminders[0].act, Some(Action::Play { station: Label::parse("Jazz") }));
    assert_eq!(
        get(&app, "/api/alive").await.json()["reminders"][0]["then"],
        json!({ "do": "play", "station": "Jazz" })
    );
    let bad = r#"{"rules":[{"words":"wake up","when":{"minutes":0},"then":{"do":"explode"}}]}"#;
    assert_eq!(post(&app, "/api/reminders", bad).await.status, StatusCode::BAD_REQUEST);
}

/// An app with a stand-in for the Spotify player installed, and the job that runs it started.
fn app_with_spotify(name: &str) -> (Shared, TempFrame) {
    let frame = TempFrame::new(&format!("route-{name}"));
    spotify::testing::program(&frame.0);
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    let app = App::new(cfg, Device::Simulated, 8080);
    tokio::spawn(Arc::clone(&app.spotify).run(Arc::clone(&app.cfg)));
    (app, frame)
}

/// Wait (a few seconds at most) until Spotify has got as far as `stage`.
async fn until_spotify(app: &Shared, stage: &str) -> Value {
    for _ in 0..400 {
        let view = get(app, "/api/acting").await.json()["spotify"].clone();
        if view["stage"] == stage {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Spotify never became {stage}");
}

#[tokio::test]
async fn without_the_player_spotify_is_said_not_to_be_installed() {
    let (app, _frame) = app("spotify-none");
    let view = get(&app, "/api/acting").await.json()["spotify"].clone();
    assert_eq!((&view["installed"], &view["on"], &view["stage"]), (&json!(false), &json!(false), &json!("off")));
    assert_eq!(post(&app, "/api/spotify/do", r#"{"do":"pause"}"#).await.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(post(&app, "/api/spotify/do", r#"{"do":"dance"}"#).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn spotify_is_switched_on_ordered_about_and_heard() {
    let (app, frame) = app_with_spotify("spotify");
    let before = get(&app, "/api/acting").await.json()["spotify"].clone();
    assert_eq!((&before["installed"], &before["on"], &before["stage"]), (&json!(true), &json!(false), &json!("off")));

    let switched = post(&app, "/api/spotify", r#"{"on":true}"#).await;
    assert_eq!((switched.status, switched.json()), (StatusCode::OK, json!({ "on": true })));
    assert!(store::load_settings(&frame.0).spotify);
    let waiting = until_spotify(&app, "waiting").await;
    assert_eq!((&waiting["on"], &waiting["name"]), (&json!(true), &json!("Frame")));

    // The screen is waiting to be told what to do; news of Spotify answers it at once.
    let last = get(&app, "/api/acts").await.json();
    let (shared, version) = (Arc::clone(&app), last["spotify"]["version"].as_u64().unwrap());
    let waiting =
        tokio::spawn(async move { get(&shared, &format!("/api/acts?after=0&spotify={version}")).await.json() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The sound is listened to as a WAV file without an end.
    let listening = router(Arc::clone(&app)).oneshot(Request::get("/api/spotify/stream").body(Body::empty()).unwrap());
    let stream = listening.await.unwrap();
    assert_eq!(stream.headers()[CONTENT_TYPE], "audio/wav");
    let mut sound = stream.into_body().into_data_stream();
    assert_eq!(&sound.next().await.unwrap().unwrap()[..4], b"RIFF");

    assert_eq!(post(&app, "/api/spotify/do", r#"{"do":"play"}"#).await.status, StatusCode::OK);
    let told = tokio::time::timeout(Duration::from_secs(4), waiting).await.expect("did not wait 25 seconds").unwrap();
    assert_eq!(told["acts"], json!([]));
    assert_ne!(told["spotify"]["version"], json!(version));
    let playing = until_spotify(&app, "playing").await;
    assert_eq!(playing["track"]["title"], "Blue in Green");
    let heard = tokio::time::timeout(Duration::from_secs(4), sound.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(&heard[..], b"01234567", "whole frames of sound");

    // The buttons on the frame's screen need no PIN, even when one is set.
    post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    assert_eq!(post(&app, "/api/spotify/do", r#"{"do":"pause"}"#).await.status, StatusCode::OK);
    assert_eq!(post(&app, "/api/spotify/do", r#"{"do":"volume","to":40}"#).await.status, StatusCode::OK);
    assert_eq!(post(&app, "/api/spotify/do", r#"{"do":"volume","to":140}"#).await.status, StatusCode::BAD_REQUEST);
    until_spotify(&app, "paused").await;
    for _ in 0..400 {
        if get(&app, "/api/acting").await.json()["spotify"]["volume"] == 40 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(get(&app, "/api/acting").await.json()["spotify"]["volume"], 40, "the app says the volume back");
    assert_eq!(post(&app, "/api/spotify", r#"{"on":false}"#).await.status, StatusCode::UNAUTHORIZED);
}
