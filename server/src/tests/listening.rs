//! Route tests: the Yoto players' listening time, from a stand-in for Yoto Limiter.

use std::sync::atomic::Ordering;

use super::*;
use crate::yoto::{testing, Yoto};

/// An app that reads its Yoto players from a stand-in for Yoto Limiter.
async fn app_with_yoto(name: &str) -> (Shared, TempFrame, Arc<testing::Seen>) {
    let (base, seen) = testing::yoto_limiter().await;
    let frame = TempFrame::new(&format!("route-{name}"));
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    let mut app = Arc::try_unwrap(App::new(cfg, Device::simulated(), 8080)).ok().expect("a new app has one owner");
    app.yoto = Arc::new(Yoto::new(&base));
    (Arc::new(app), frame, seen)
}

#[tokio::test]
async fn a_key_is_kept_only_once_it_works_and_is_never_handed_back() {
    let (app, frame, _) = app_with_yoto("yoto-key").await;
    let view = get(&app, "/api/yoto").await.json();
    assert_eq!((&view["set_up"], &view["players"]), (&json!(false), &json!([])));
    assert_eq!(get(&app, "/api/alive").await.json()["yoto"], false, "no Yoto in Utilities yet");

    let malformed = post(&app, "/api/yoto/key", r#"{"key":"not a key"}"#).await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    assert!(malformed.json()["error"].as_str().unwrap().contains("yl_"));
    let unknown = post(&app, "/api/yoto/key", r#"{"key":"yl_wrong0123456789"}"#).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown.json()["error"], "Yoto Limiter does not know that key: it may have been mistyped or deleted");
    let lapsed = post(&app, "/api/yoto/key", &json!({ "key": testing::KEY_WITHOUT_PLUS }).to_string()).await;
    assert!(lapsed.json()["error"].as_str().unwrap().contains("not on Plus"));
    assert_eq!(store::load_settings(&frame.0).yoto_key, None, "a key that does not work is not kept");

    let saved = post(&app, "/api/yoto/key", &json!({ "key": testing::KEY }).to_string()).await;
    assert_eq!(saved.status, StatusCode::OK);
    let said = saved.json();
    assert_eq!((&said["set_up"], &said["players"][0]["name"]), (&json!(true), &json!("Mia's Yoto")));
    assert_eq!(store::load_settings(&frame.0).yoto_key.unwrap().as_str(), testing::KEY);
    assert_eq!(get(&app, "/api/alive").await.json()["yoto"], true);
    let device = get(&app, "/api/device").await;
    assert_eq!(device.json()["yoto"], true);
    for reply in [device, get(&app, "/api/yoto").await, get(&app, "/api/alive").await] {
        assert!(!String::from_utf8_lossy(&reply.body).contains(testing::KEY), "the key is never handed back");
    }

    let forgotten = post(&app, "/api/yoto/key", r#"{"key":null}"#).await.json();
    assert_eq!((&forgotten["set_up"], &forgotten["players"]), (&json!(false), &json!([])));
    assert_eq!(store::load_settings(&frame.0).yoto_key, None);
}

#[tokio::test]
async fn changing_the_key_takes_the_pin_when_one_is_set() {
    let (app, _frame, _) = app_with_yoto("yoto-pin").await;
    assert_eq!(post(&app, "/api/pin", r#"{"pin":"2468"}"#).await.status, StatusCode::OK);
    let refused = post(&app, "/api/yoto/key", &json!({ "key": testing::KEY }).to_string()).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(get(&app, "/api/yoto").await.status, StatusCode::OK, "reading needs no PIN");
}

#[tokio::test]
async fn the_players_are_followed_live_once_there_is_a_key() {
    let (app, _frame, seen) = app_with_yoto("yoto-live").await;
    tokio::spawn(Arc::clone(&app.yoto).keep_following(reqwest::Client::new(), Arc::clone(&app.cfg)));
    let shared = Arc::clone(&app);
    blocking(move || shared.update_settings(|settings| settings.yoto_key = YotoKey::parse(testing::KEY))).await;
    app.yoto.key_changed();
    let mut view = Value::Null;
    for _ in 0..200 {
        view = get(&app, "/api/yoto").await.json();
        if view["players"][0]["remaining_secs"] == 1740 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(view["players"][0]["remaining_secs"], 1740, "{view}");
    assert_eq!((&view["in_touch"], &view["problem"]), (&json!(true), &Value::Null));
    assert_eq!(seen.streams.load(Ordering::Relaxed), 1);

    // A key the service no longer knows: the problem is shown and the stream is not opened again and again.
    let shared = Arc::clone(&app);
    blocking(move || shared.update_settings(|settings| settings.yoto_key = YotoKey::parse("yl_deleted0123456789")))
        .await;
    app.yoto.key_changed();
    for _ in 0..200 {
        if get(&app, "/api/yoto").await.json()["problem"].is_string() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let problem = get(&app, "/api/yoto").await.json()["problem"].clone();
    assert!(problem.as_str().unwrap().contains("does not know that key"), "{problem}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(seen.streams.load(Ordering::Relaxed), 1, "a refused key is not tried again until it changes");
}

#[tokio::test]
async fn a_grown_up_gives_time_or_pauses_a_player_only_with_the_pin() {
    let (app, _frame, seen) = app_with_yoto("yoto-order").await;
    let keep = |key: &str| {
        let (shared, key) = (Arc::clone(&app), YotoKey::parse(key));
        async move { blocking(move || shared.update_settings(|settings| settings.yoto_key = key)).await }
    };
    keep(testing::KEY).await;
    app.yoto.check(&app.http, &YotoKey::parse(testing::KEY).unwrap()).await.unwrap();
    let more = r#"{"player":7,"order":{"time":15}}"#;

    // Without a PIN, anyone at the frame could: so nobody may.
    let open = post(&app, "/api/yoto/order", more).await;
    assert_eq!(open.status, StatusCode::FORBIDDEN);
    assert!(open.json()["error"].as_str().unwrap().starts_with("Set a PIN for the frame first"));

    let token = post(&app, "/api/pin", r#"{"pin":"2468"}"#).await.json()["token"].as_str().unwrap().to_string();
    assert_eq!(post(&app, "/api/yoto/order", more).await.status, StatusCode::UNAUTHORIZED, "the PIN is asked for");
    let given = post_as(&app, &token, "/api/yoto/order", more).await;
    assert_eq!(given.status, StatusCode::OK);
    assert_eq!(given.json()["players"][0]["remaining_secs"], 1800 + 15 * 60);
    for order in [r#"{"time":-5}"#, r#""pause""#, r#"{"hold":30}"#, r#""resume""#] {
        let reply = post_as(&app, &token, "/api/yoto/order", &format!(r#"{{"player":7,"order":{order}}}"#)).await;
        assert_eq!(reply.status, StatusCode::OK, "{order}");
    }
    assert_eq!(*seen.orders.lock().unwrap(), ["extend 7 15", "extend 7 -5", "pause 7", "hold 7 30", "resume 7"]);

    let silly = post_as(&app, &token, "/api/yoto/order", r#"{"player":7,"order":{"time":500}}"#).await;
    assert_eq!(silly.status, StatusCode::BAD_REQUEST);
    keep(testing::KEY_READ_ONLY).await;
    let read_only = post_as(&app, &token, "/api/yoto/order", more).await;
    assert_eq!(read_only.status, StatusCode::BAD_GATEWAY);
    assert_eq!(read_only.json()["error"], "The frame's key may only read: make one with the read and control scopes");
    keep("").await;
    let not_set_up = post_as(&app, &token, "/api/yoto/order", more).await;
    assert_eq!(
        (not_set_up.status, not_set_up.json()["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("Yoto Limiter is not set up"))
    );
    assert_eq!(seen.orders.lock().unwrap().len(), 5, "nothing refused reached Yoto Limiter");
}
