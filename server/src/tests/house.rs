//! Route tests: the house (doorbell, cameras, alarm and sensors, as a relay reports them).

use super::*;

const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 9, 9, 9];

/// A POST as the relay sends it: no browser headers, only its key.
async fn relay(app: &Shared, key: &str, path: &str, body: impl Into<Body>) -> Reply {
    send(app, Request::post(path).header("X-Home-Key", key).body(body.into()).unwrap()).await
}

/// Connect the house and return the key the relay is to use.
async fn connect(app: &Shared) -> String {
    post(app, "/api/home/key", "{}").await.json()["key"].as_str().unwrap().to_string()
}

fn house(ringing: bool) -> String {
    json!({
        "alarms": [{ "name": "Homebase", "state": "armed_home" }],
        "sensors": [{ "name": "Front Door", "kind": "door", "on": true }],
        "cameras": [{ "id": "doorbell", "name": "Doorbell", "bell": true }],
        "ringing": ringing,
    })
    .to_string()
}

#[tokio::test]
async fn a_house_that_is_not_connected_takes_no_reports() {
    let (app, _frame) = app("house-none");
    assert_eq!(get(&app, "/api/device").await.json()["home"], false);
    assert_eq!(get(&app, "/api/alive").await.json()["home"], false);
    assert_eq!(get(&app, "/api/home").await.json()["connected"], false);
    for key in ["", "0123456789abcdef0123456789abcdef"] {
        assert_eq!(
            relay(&app, key, "/api/home/report", house(false)).await.status,
            StatusCode::UNAUTHORIZED,
            "{key:?}"
        );
    }
    assert_eq!(get(&app, "/api/home/picture?camera=doorbell").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_relay_reports_the_house_with_its_key_and_the_screen_can_read_it() {
    let (app, frame) = app("house");
    let key = connect(&app).await;
    assert_eq!(key.len(), 32);
    assert!(store::load_settings(&frame.0).home_key.is_some());
    assert_eq!(get(&app, "/api/device").await.json()["home"], true);

    let heard = relay(&app, &key, "/api/home/report", house(false)).await;
    let wanted = json!({ "heard": true, "rings": false, "wanted": ["doorbell"] });
    assert_eq!((heard.status, heard.json()), (StatusCode::OK, wanted), "it asks for the picture it does not have");
    let view = get(&app, "/api/home").await.json();
    assert_eq!(view["connected"], true);
    assert_eq!(view["alarms"][0], json!({ "name": "Homebase", "state": "armed_home" }));
    assert_eq!(view["sensors"][0]["on"], true);
    assert_eq!(get(&app, "/api/device").await.json()["home_in_touch"], true);

    // Pictures: kept per camera, served back, and refused when they are not pictures.
    assert_eq!(relay(&app, &key, "/api/home/picture?camera=doorbell", JPEG).await.status, StatusCode::OK);
    let picture = get(&app, "/api/home/picture?camera=doorbell").await;
    assert_eq!((picture.status, picture.content_type(), &picture.body[..]), (StatusCode::OK, "image/jpeg", JPEG));
    assert!(get(&app, "/api/home").await.json()["cameras"][0]["picture"].as_u64().is_some());
    assert_eq!(relay(&app, &key, "/api/home/report", house(false)).await.json()["wanted"], json!([]));
    assert_eq!(relay(&app, &key, "/api/home/picture?camera=garden", JPEG).await.status, StatusCode::NOT_FOUND);
    assert_eq!(relay(&app, &key, "/api/home/picture?camera=doorbell", "<html>").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(relay(&app, &key, "/api/home/picture?camera=../x", JPEG).await.status, StatusCode::NOT_FOUND);

    // A report that does not parse is refused with the reason; the wrong key gets nowhere.
    let bad = relay(&app, &key, "/api/home/report", r#"{"alarms":[{"name":"","state":"disarmed"}]}"#).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let wrong = "f".repeat(32);
    assert_eq!(relay(&app, &wrong, "/api/home/report", house(false)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(relay(&app, &wrong, "/api/home/picture?camera=doorbell", JPEG).await.status, StatusCode::UNAUTHORIZED);

    // A new key replaces the old one; disconnecting forgets the house.
    let newer = connect(&app).await;
    assert_ne!(newer, key);
    assert_eq!(relay(&app, &key, "/api/home/report", house(false)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(relay(&app, &newer, "/api/home/report", house(false)).await.status, StatusCode::OK);
    assert_eq!(post(&app, "/api/home/forget", "{}").await.json(), json!({ "home": false }));
    assert_eq!(relay(&app, &newer, "/api/home/report", house(false)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(get(&app, "/api/home").await.json()["alarms"], json!([]));
}

#[tokio::test]
async fn the_relays_key_opens_nothing_else_and_connecting_the_house_needs_the_pin() {
    let (app, _frame) = app("house-key");
    let key = connect(&app).await;
    // The key is for reporting the house; it does not stand in for the page or the PIN.
    assert_eq!(relay(&app, &key, "/api/delete", r#"{"paths":["a.jpg"]}"#).await.status, StatusCode::FORBIDDEN);
    assert_eq!(relay(&app, &key, "/api/home/key", "{}").await.status, StatusCode::FORBIDDEN);
    let token = post(&app, "/api/pin", r#"{"pin":"2468"}"#).await.json()["token"].as_str().unwrap().to_string();
    assert_eq!(post(&app, "/api/home/key", "{}").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(post(&app, "/api/home/forget", "{}").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(post_as(&app, &token, "/api/home/key", "{}").await.status, StatusCode::OK);
    // With a PIN set the relay still reports with its key alone, and the screen still reads the house.
    let key = post_as(&app, &token, "/api/home/key", "{}").await.json()["key"].as_str().unwrap().to_string();
    assert_eq!(relay(&app, &key, "/api/home/report", house(false)).await.status, StatusCode::OK);
    assert_eq!(get(&app, "/api/home").await.json()["connected"], true);
}

#[tokio::test(start_paused = true)]
async fn a_ring_switches_the_screen_on_and_keeps_it_on_a_while() {
    let _screen = frame::ONE_SCREEN.lock().await;
    let (app, _frame) = app("house-ring");
    let key = connect(&app).await;
    let mut room = night::Room::default();
    post(&app, "/api/night", r#"{"on":true,"from":"00:00","until":"23:59"}"#).await;
    relay(&app, &key, "/api/home/report", house(false)).await;
    mind_the_night(&app, &mut room).await;
    assert_eq!((app.device.power().0, app.watch().asleep), (Some(false), true), "off for the night");

    let rang = relay(&app, &key, "/api/home/report", house(true)).await.json();
    assert_eq!(rang["rings"], true);
    assert_eq!((app.device.power().0, app.watch().asleep), (Some(true), false), "on at once, not at the next check");
    assert!(get(&app, "/api/home").await.json()["rang"].as_u64().is_some());
    mind_the_night(&app, &mut room).await;
    assert_eq!(app.device.power().0, Some(true), "the night does not take it straight back");
    assert_eq!(
        relay(&app, &key, "/api/home/report", house(true)).await.json()["rings"],
        false,
        "one ring, noticed once"
    );

    tokio::time::sleep(Duration::from_mins(3)).await;
    relay(&app, &key, "/api/home/report", house(false)).await;
    mind_the_night(&app, &mut room).await;
    assert_eq!(app.device.power().0, Some(false), "afterwards the night resumes");
    app.device.switch_screen(true);
}
