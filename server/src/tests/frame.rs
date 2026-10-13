//! Route tests: the frame itself.

use super::*;

/// A POST from the frame's own browser: the page's headers, arriving from the frame itself.
async fn post_from_frame(app: &Shared, path: &str, body: &str) -> Reply {
    let mut request = page_post(path).body(Body::from(body.to_string())).unwrap();
    request.extensions_mut().insert(ConnectInfo(Peer(SocketAddr::from(([127, 0, 0, 1], 40_000)))));
    send(app, request).await
}

#[tokio::test]
async fn the_simulator_page_is_served() {
    let (app, _frame) = app("simulator");
    let reply = get(&app, "/simulator").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&reply.body).contains("<title>Frame simulator</title>"));
}

#[tokio::test]
async fn the_frame_reports_its_wifi_screen_and_slideshow() {
    let (app, _frame) = app("device");
    let device = get(&app, "/api/device").await.json();
    assert_eq!(device["simulated"], true);
    assert_eq!(device["wifi"]["network"], "Simulated network");
    assert_eq!((device["wifi"]["on"].as_bool(), device["wifi"]["signal"].as_i64()), (Some(true), Some(-52)));
    assert_eq!(device["screen"], "legacy", "the frame keeps its own screen until someone chooses the new one");
    assert_eq!(device["fell_back"], false);
    assert_eq!(
        device["slideshow"],
        json!({ "every": 15, "order": "shuffled", "memories": true, "morning": true, "weather": true, "home_after": 5 })
    );
    assert_eq!(device["night"], json!({ "on": false, "from": "23:00", "until": "07:00", "when_dark": false }));
    assert_eq!((&device["chime"], &device["place"], &device["asleep"]), (&Value::Null, &Value::Null, &json!(false)));
    assert_eq!(device["light"], 110.0);
}

#[tokio::test]
async fn the_address_for_a_phone_is_given_as_text_and_as_a_qr_code() {
    let (app, _frame) = app("address");
    let device = get(&app, "/api/device").await.json();
    let address = device["address"].as_str().unwrap().to_string();
    // The frame's own address on its network, not "localhost": a phone has to be able to open it.
    assert_eq!(address, format!("http://{}:8080", device["wifi"]["address"].as_str().unwrap()));
    let code = get(&app, "/api/address.svg").await;
    assert_eq!((code.status, code.content_type()), (StatusCode::OK, "image/svg+xml"));
    let picture = String::from_utf8_lossy(&code.body).into_owned();
    assert!(picture.contains("<svg") && picture.contains("</svg>"));
    // The same address always draws the same code, and it is the code of that address.
    let expected = qrcode::QrCode::new(address.as_bytes()).unwrap();
    assert_eq!(picture, expected.render::<qrcode::render::svg::Color>().min_dimensions(240, 240).build());
    assert_eq!(manage_address("192.168.4.23", 8080), "http://192.168.4.23:8080");
}

#[tokio::test]
async fn a_frame_that_will_not_say_where_it_is_has_no_address_or_code() {
    let frame = TempFrame::new("route-no-address");
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    // On a computer there is no Android to ask, so a frame that is not simulated knows nothing.
    let app = App::new(cfg, Device::Android, 8080);
    let device = get(&app, "/api/device").await.json();
    assert_eq!((&device["address"], &device["wifi"]), (&Value::Null, &Value::Null));
    assert_eq!(get(&app, "/api/address.svg").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_screen_can_be_chosen_and_the_choice_is_kept() {
    let (app, frame) = app("screen");
    let chosen = post(&app, "/api/screen", r#"{"show":"new"}"#).await;
    assert_eq!((chosen.status, chosen.json()), (StatusCode::OK, json!({ "screen": "new", "shown": true })));
    assert_eq!(store::load_settings(&frame.0).screen, Screen::New);
    assert_eq!(get(&app, "/api/device").await.json()["screen"], "new");
    assert!(app.watch().shown.is_some(), "the server now expects to hear from the new screen");

    post(&app, "/api/screen", r#"{"show":"legacy"}"#).await;
    assert_eq!(store::load_settings(&frame.0).screen, Screen::Legacy);
    assert!(app.watch().shown.is_none());
    let refused = post(&app, "/api/screen", r#"{"show":"sideways"}"#).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_slideshow_can_be_set_and_the_screen_learns_of_it_when_it_says_it_is_alive() {
    let (app, _frame) = app("slideshow");
    assert!(app.watch().heard.is_none());
    assert_eq!(
        get(&app, "/api/alive").await.json()["slideshow"],
        json!({ "every": 15, "order": "shuffled", "memories": true, "morning": true, "weather": true, "home_after": 5 })
    );
    assert!(app.watch().heard.is_some(), "the server heard from the screen");

    let set = post(&app, "/api/slideshow", r#"{"every":60,"order":"newest"}"#).await;
    assert_eq!(set.status, StatusCode::OK);
    let alive = get(&app, "/api/alive").await.json();
    let expected =
        json!({ "every": 60, "order": "newest", "memories": true, "morning": true, "weather": true, "home_after": 5 });
    assert_eq!((&alive["slideshow"], &alive["chime"], &alive["asleep"]), (&expected, &Value::Null, &json!(false)));
    post(&app, "/api/slideshow", r#"{"every":60,"order":"newest","memories":false,"morning":false,"weather":false}"#)
        .await;
    assert_eq!(get(&app, "/api/alive").await.json()["slideshow"]["memories"], false);
    for bad in [r#"{"every":1}"#, r#"{"every":15,"order":"backwards"}"#] {
        assert_eq!(post(&app, "/api/slideshow", bad).await.status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_frame_comes_back_from_the_wifi_settings_by_itself() {
    let (app, _frame) = app("wifi-settings");
    post(&app, "/api/screen", r#"{"show":"new"}"#).await;
    let opened = post(&app, "/api/wifi/settings", "{}").await;
    assert_eq!((opened.status, opened.json()["opened"].as_bool()), (StatusCode::OK, Some(true)));
    assert!(app.watch().in_wifi_settings);
    assert_eq!(post(&app, "/api/wifi/settings", "{}").await.status, StatusCode::OK, "asking twice is harmless");
    // The stand-in frame is never touched, so the first look finds it idle.
    for _ in 0..200 {
        if !app.watch().in_wifi_settings {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let watch = app.watch();
    assert!(!watch.in_wifi_settings);
    assert!(watch.shown.is_some(), "the screen that was chosen is in front again");
}

#[tokio::test(start_paused = true)]
async fn a_new_screen_that_goes_quiet_sends_the_frame_back_to_its_own() {
    let (app, frame) = app("fall-back");
    tokio::spawn(fall_back_if_quiet(Arc::clone(&app)));
    post(&app, "/api/screen", r#"{"show":"new"}"#).await;
    // While it keeps saying it is alive, nothing happens.
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_secs(60)).await;
        get(&app, "/api/alive").await;
    }
    assert!(app.watch().shown.is_some());
    assert_eq!(get(&app, "/api/device").await.json()["fell_back"], false);
    // Then it stops.
    tokio::time::sleep(device::QUIET_LIMIT + Duration::from_secs(61)).await;
    // Putting the other screen in front happens off the test's paused clock; give it a real moment.
    for _ in 0..500 {
        if app.watch().fell_back {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        tokio::task::yield_now().await;
    }
    assert!(app.watch().shown.is_none());
    let device = get(&app, "/api/device").await.json();
    assert_eq!((device["fell_back"].as_bool(), device["screen"].as_str()), (Some(true), Some("new")));
    assert_eq!(store::load_settings(&frame.0).screen, Screen::New, "the choice itself is kept");
    // Choosing it again clears the note.
    post(&app, "/api/screen", r#"{"show":"new"}"#).await;
    assert_eq!(get(&app, "/api/device").await.json()["fell_back"], false);
}

#[tokio::test(start_paused = true)]
async fn the_frame_is_not_sent_back_while_the_wifi_settings_are_open() {
    let (app, _frame) = app("fall-back-wifi");
    tokio::spawn(fall_back_if_quiet(Arc::clone(&app)));
    post(&app, "/api/screen", r#"{"show":"new"}"#).await;
    app.watch().in_wifi_settings = true;
    tokio::time::sleep(device::QUIET_LIMIT * 3).await;
    assert!(app.watch().shown.is_some() && !app.watch().fell_back);
}

#[tokio::test]
async fn with_a_pin_the_frames_own_screen_needs_it_too_and_can_lock_again() {
    let (app, _frame) = app("frame-pin");
    assert_eq!(get(&app, "/api/device").await.json()["pin"], false);
    post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    assert_eq!(get(&app, "/api/device").await.json()["pin"], true);
    let changes = [
        ("/api/screen", r#"{"show":"legacy"}"#),
        ("/api/slideshow", r#"{"every":30}"#),
        ("/api/wifi/settings", "{}"),
        ("/api/night", r#"{"on":false}"#),
        ("/api/chime", r#"{"before":null}"#),
    ];
    for (path, body) in changes {
        assert_eq!(post(&app, path, body).await.status, StatusCode::UNAUTHORIZED, "{path} from the network");
        assert_eq!(post_from_frame(&app, path, body).await.status, StatusCode::UNAUTHORIZED, "{path} from the frame");
    }
    // The PIN, typed on the frame, opens them; leaving the settings locks them again.
    let token =
        post_from_frame(&app, "/api/unlock", r#"{"pin":"2468"}"#).await.json()["token"].as_str().unwrap().to_string();
    for (path, body) in changes {
        assert_eq!(post_as(&app, &token, path, body).await.status, StatusCode::OK, "{path} once unlocked");
    }
    assert_eq!(post_as(&app, &token, "/api/lock", "{}").await.json(), json!({ "locked": true }));
    assert_eq!(post_as(&app, &token, "/api/night", r#"{"on":false}"#).await.status, StatusCode::UNAUTHORIZED);
    // The page's own header is still required, so another website cannot drive it.
    let bare = Request::post("/api/unlock").header(HOST, HOST_NAME).body(Body::from(r#"{"pin":"2468"}"#)).unwrap();
    assert_eq!(send(&app, bare).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn locking_without_a_pin_or_a_session_changes_nothing() {
    let (app, _frame) = app("lock-nothing");
    assert_eq!(post(&app, "/api/lock", "{}").await.json(), json!({ "locked": false }));
    assert_eq!(post_as(&app, "made-up", "/api/lock", "{}").await.json(), json!({ "locked": false }));
}

#[tokio::test]
async fn night_and_chime_settings_are_kept_and_reported() {
    let (app, frame) = app("night-settings");
    let set = post(&app, "/api/night", r#"{"on":true,"from":"22:30","until":"06:00","when_dark":true}"#).await;
    assert_eq!(set.status, StatusCode::OK);
    let night = json!({ "on": true, "from": "22:30", "until": "06:00", "when_dark": true });
    assert_eq!(get(&app, "/api/device").await.json()["night"], night);
    assert!(store::load_settings(&frame.0).night.on);
    assert_eq!(post(&app, "/api/night", r#"{"on":true,"from":"late"}"#).await.status, StatusCode::BAD_REQUEST);

    assert_eq!(post(&app, "/api/chime", r#"{"before":10}"#).await.json(), json!({ "chime": 10 }));
    assert_eq!(get(&app, "/api/alive").await.json()["chime"], 10);
    assert_eq!(post(&app, "/api/chime", r#"{"before":null}"#).await.json(), json!({ "chime": null }));
    assert_eq!(post(&app, "/api/chime", r#"{"before":500}"#).await.status, StatusCode::BAD_REQUEST);
}

#[test]
fn the_time_of_day_follows_the_frames_time_zone() {
    let noon_utc = 1_791_892_800; // 13 October 2026, 12:00 UTC
    let at = |zone: Option<&str>| time_of_day(zone, noon_utc).format("%H:%M").to_string();
    assert_eq!(at(Some("Europe/London")), "13:00", "summer time");
    assert_eq!(at(Some("America/New_York")), "08:00");
    assert_eq!(at(None), "12:00");
    assert_eq!(at(Some("Mars/Olympus")), "12:00", "a zone we do not know leaves the clock on UTC");
}

/// Serialises the tests that switch the stand-in frame's one screen.
static ONE_SCREEN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(start_paused = true)]
async fn night_mode_switches_the_screen_off_for_the_night_and_back_on_after() {
    let _screen = ONE_SCREEN.lock().await;
    let (app, _frame) = app("night-mode");
    let mut room = night::Room::default();
    let on = || app.device.power().0;
    post(&app, "/api/screen", r#"{"show":"new"}"#).await;

    // Night mode is off: nothing happens, whatever the hour.
    post(&app, "/api/night", r#"{"on":false,"from":"00:00","until":"23:59"}"#).await;
    mind_the_night(&app, &mut room).await;
    assert_eq!((on(), app.watch().asleep), (Some(true), false));

    // A night that covers every minute but the last of the day: the screen goes off.
    post(&app, "/api/night", r#"{"on":true,"from":"00:00","until":"23:59"}"#).await;
    mind_the_night(&app, &mut room).await;
    assert_eq!((on(), app.watch().asleep), (Some(false), true));
    assert_eq!(get(&app, "/api/device").await.json()["asleep"], true);
    assert_eq!(get(&app, "/api/alive").await.json()["asleep"], true);
    mind_the_night(&app, &mut room).await;
    assert_eq!(on(), Some(false), "and stays off");

    // While it is off the new screen is not expected to say anything.
    tokio::spawn(fall_back_if_quiet(Arc::clone(&app)));
    tokio::time::sleep(device::QUIET_LIMIT * 3).await;
    assert!(app.watch().shown.is_some() && !app.watch().fell_back);

    // The night is over (here: switched off): the screen comes back.
    post(&app, "/api/night", r#"{"on":false}"#).await;
    mind_the_night(&app, &mut room).await;
    assert_eq!((on(), app.watch().asleep), (Some(true), false));
    assert!(app.watch().heard.is_some_and(|heard| heard.elapsed() < Duration::from_secs(5)), "listening starts afresh");
}

#[tokio::test(start_paused = true)]
async fn a_screen_someone_else_switched_off_is_not_switched_on_by_night_mode() {
    let _screen = ONE_SCREEN.lock().await;
    let (app, _frame) = app("night-not-ours");
    let mut room = night::Room::default();
    app.device.switch_screen(false);
    mind_the_night(&app, &mut room).await;
    assert_eq!(app.device.power().0, Some(false));
    app.device.switch_screen(true);
}
