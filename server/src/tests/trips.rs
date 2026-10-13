//! Route tests: getting to events.

use super::*;

/// 18:30 on 14 October 2026 in London, as a moment.
const HALF_PAST_SIX: i64 = 1_791_999_000;

/// An app whose journeys come from stand-ins for the planners.
async fn app_with_planners(name: &str) -> (Shared, TempFrame) {
    let services = travel::testing::services().await;
    let frame = TempFrame::new(&format!("route-{name}"));
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    let mut app = Arc::try_unwrap(App::new(cfg, Device::Simulated, 8080)).ok().expect("a new app has one owner");
    app.travel = travel::Travel::new(services);
    (Arc::new(app), frame)
}

#[tokio::test]
async fn a_trip_is_set_up_for_an_event_and_says_when_to_leave() {
    let (app, _frame) = app_with_planners("trips").await;
    let calendar = get(&app, "/api/calendar").await.json();
    assert_eq!(calendar["travel"], json!({ "home": null, "trips": [] }));
    assert_eq!((&calendar["ways"], &calendar["driving"]), (&json!(["transit", "walk", "cycle"]), &json!(false)));

    post(&app, "/api/events/add", r#"{"title":"Dentist","day":"2026-10-14","time":"18:30"}"#).await;
    post(&app, "/api/events/add", r#"{"title":"Swimming","day":"2026-10-15","time":"10:00"}"#).await;
    let trips = json!({ "home": "BR6 0AA", "trips": [{ "title": "Dentist", "to": "SE1 9SG", "how": "transit" }] });
    let before = get(&app, "/api/alive").await.json()["calendar"].as_u64().unwrap();
    let saved = post(&app, "/api/travel", &trips.to_string()).await;
    let after = get(&app, "/api/alive").await.json()["calendar"].as_u64().unwrap();
    assert!(after > before, "the screen is told the calendar has news");
    assert_eq!((saved.status, saved.json()), (StatusCode::OK, json!({ "trips": 1 })));

    let calendar = get(&app, "/api/calendar?from=1791936000&to=1792195200").await.json();
    assert_eq!(calendar["travel"]["home"], "BR6 0AA");
    assert_eq!(calendar["travel"]["trips"][0]["margin"], 10);
    let trip_of = |title: &str| {
        calendar["events"].as_array().unwrap().iter().find(|event| event["title"] == title).unwrap()["trip"].clone()
    };
    assert_eq!((trip_of("Dentist"), trip_of("Swimming")), (json!(0), Value::Null));

    let plan = get(&app, &format!("/api/travel?trip=0&arrive={HALF_PAST_SIX}")).await;
    assert_eq!(plan.status, StatusCode::OK);
    let plan = plan.json();
    // The 17:07 journey less ten minutes to spare: 16:57 in London is 15:57 UTC.
    assert_eq!(plan["leave"], HALF_PAST_SIX - 93 * 60);
    assert_eq!((&plan["catch"], &plan["how"]), (&json!("17:20 Southeastern to London Bridge"), &json!("transit")));
    assert_eq!(plan["steps"].as_array().unwrap().len(), 3);
    assert_eq!(plan["lines"][0]["kind"], "walking");
    assert_eq!(plan["modes"], json!(["walking", "national-rail", "walking"]));

    // The map the way is drawn on comes through the frame too.
    let square = get(&app, "/api/map/10/512/341").await;
    assert_eq!(
        (square.status, square.content_type(), &square.body[..]),
        (StatusCode::OK, "image/png", &b"a picture"[..])
    );
    assert_eq!(get(&app, "/api/map/30/0/0").await.status, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, "/api/map/9/1/1").await.status, StatusCode::BAD_GATEWAY);

    assert_eq!((&plan["usual"], &plan["ways"]), (&json!("transit"), &json!(["transit", "walk", "cycle"])));

    // The same trip another way, to compare: on foot it means leaving at 17:25.
    let on_foot = get(&app, &format!("/api/travel?trip=0&arrive={HALF_PAST_SIX}&how=walk")).await.json();
    assert_eq!(
        (&on_foot["leave"], &on_foot["how"], &on_foot["usual"]),
        (&json!(HALF_PAST_SIX - 65 * 60), &json!("walk"), &json!("transit"))
    );
    let by_car = get(&app, &format!("/api/travel?trip=0&arrive={HALF_PAST_SIX}&how=car")).await;
    assert_eq!(by_car.status, StatusCode::CONFLICT, "not without the key");
    for bad in ["park", "teleport"] {
        let reply = get(&app, &format!("/api/travel?trip=0&arrive={HALF_PAST_SIX}&how={bad}")).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{bad}");
    }

    assert_eq!(get(&app, "/api/travel?trip=1&arrive=1").await.status, StatusCode::NOT_FOUND, "no such trip");
    assert_eq!(get(&app, "/api/travel?trip=0").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(get(&app, "/api/travel?trip=x&arrive=y").await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn trips_by_car_are_only_there_with_a_driving_key_which_is_never_handed_back() {
    let (app, frame) = app_with_planners("trips-car").await;
    let by_car = json!({ "home": "BR6 0AA", "trips": [
        { "title": "Dentist", "to": "SE1 9SG", "how": "car", "margin": 0 },
        { "title": "Match", "to": "Wembley Stadium", "how": "park", "via": "Orpington station" },
    ] });
    let refused = post(&app, "/api/travel", &by_car.to_string()).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.json()["error"].as_str().unwrap().contains("need a key"));
    assert_eq!(get(&app, "/api/calendar").await.json()["travel"]["trips"], json!([]), "nothing was kept");

    let key = format!(r#"{{"key":"{}"}}"#, travel::testing::KEY);
    let keyed = post(&app, "/api/travel/key", &key).await.json();
    assert_eq!(keyed, json!({ "driving": true, "ways": ["transit", "walk", "cycle", "car", "park"] }));
    assert_eq!(post(&app, "/api/travel", &by_car.to_string()).await.status, StatusCode::OK);
    let calendar = get(&app, "/api/calendar").await;
    assert_eq!(calendar.json()["driving"], true);
    assert!(!String::from_utf8_lossy(&calendar.body).contains(travel::testing::KEY));
    assert!(!String::from_utf8_lossy(&get(&app, "/api/device").await.body).contains(travel::testing::KEY));

    let drive = get(&app, &format!("/api/travel?trip=0&arrive={HALF_PAST_SIX}")).await.json();
    assert_eq!((&drive["leave"], &drive["steps"]), (&json!(HALF_PAST_SIX - 43 * 60), &json!(["Drive (43 min)"])));
    let mixed = get(&app, &format!("/api/travel?trip=1&arrive={HALF_PAST_SIX}")).await.json();
    assert_eq!(mixed["steps"][0], "Drive to Orpington station (8 min)");
    assert_eq!(mixed["catch"], "16:30 Southeastern to Charing Cross");

    // Without the key again, the trips stay but cannot be worked out, and say so.
    assert_eq!(post(&app, "/api/travel/key", r#"{"key":null}"#).await.json()["driving"], false);
    assert!(store::load_settings(&frame.0).roads_key.is_none());
    let keyless = get(&app, &format!("/api/travel?trip=0&arrive={}", HALF_PAST_SIX + 60)).await;
    assert_eq!(keyless.status, StatusCode::CONFLICT);
    assert_eq!(post(&app, "/api/travel/key", r#"{"key":"short"}"#).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_trip_that_cannot_be_planned_is_answered_with_the_reason() {
    let (app, _frame) = app_with_planners("trips-bad").await;
    let trips = json!({ "home": null, "trips": [
        { "title": "A", "to": "SE1 9SG", "how": "transit" },
        { "title": "B", "to": "Nan's house", "how": "walk", "from": "BR6 0AA" },
    ] });
    post(&app, "/api/travel", &trips.to_string()).await;
    let no_home = get(&app, &format!("/api/travel?trip=0&arrive={HALF_PAST_SIX}")).await;
    assert_eq!(no_home.status, StatusCode::CONFLICT);
    assert!(no_home.json()["error"].as_str().unwrap().contains("where home is"));
    let nowhere = get(&app, &format!("/api/travel?trip=1&arrive={HALF_PAST_SIX}")).await;
    assert_eq!(nowhere.status, StatusCode::NOT_FOUND);
    assert!(nowhere.json()["error"].as_str().unwrap().contains("Nan's house"));

    for bad in [
        r#"{"home":"","trips":[]}"#,
        r#"{"home":null}"#,
        r#"{"home":null,"trips":[{"title":"x","to":"y","how":"park"}]}"#,
    ] {
        assert_eq!(post(&app, "/api/travel", bad).await.status, StatusCode::BAD_REQUEST, "{bad}");
    }
}
