//! Route tests: the weather.

use super::*;

/// An app whose weather comes from a stand-in for Open-Meteo.
async fn app_with_weather(name: &str) -> (Shared, TempFrame) {
    let base = weather::testing::open_meteo().await;
    let frame = TempFrame::new(&format!("route-{name}"));
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    let mut app = Arc::try_unwrap(App::new(cfg, Device::simulated(), 8080)).ok().expect("a new app has one owner");
    app.weather = Arc::new(Weather::new(&format!("{base}/forecast"), &format!("{base}/places")));
    (Arc::new(app), frame)
}

#[tokio::test]
async fn a_place_is_chosen_by_name_and_its_weather_is_shown() {
    let (app, frame) = app_with_weather("weather").await;
    assert_eq!(get(&app, "/api/weather").await.json(), json!({ "place": null, "now": null, "days": [] }));

    let chosen = post(&app, "/api/weather/place", r#"{"name":" Lisbon "}"#).await;
    assert_eq!((chosen.status, chosen.json()), (StatusCode::OK, json!({ "place": "Lisbon, Portugal" })));
    assert!((store::load_settings(&frame.0).place.unwrap().latitude - 38.72).abs() < 1e-9);
    assert_eq!(get(&app, "/api/device").await.json()["place"], "Lisbon, Portugal");
    let weather = get(&app, "/api/weather").await.json();
    assert_eq!(weather["now"], json!({ "temp": 14.2, "code": 61, "day": true }));
    assert_eq!(get(&app, "/api/alive").await.json()["weather"], weather["fetched"], "the screen is told there is news");
    assert_eq!((weather["place"].as_str(), weather["days"].as_array().unwrap().len()), (Some("Lisbon, Portugal"), 2));

    let unknown = post(&app, "/api/weather/place", r#"{"name":"Atlantis"}"#).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert!(unknown.json()["error"].as_str().unwrap().contains("no place by that name"));
    assert_eq!(post(&app, "/api/weather/place", r#"{"name":"  "}"#).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(get(&app, "/api/device").await.json()["place"], "Lisbon, Portugal", "a failed search changes nothing");

    assert_eq!(post(&app, "/api/weather/forget", "{}").await.json(), json!({ "place": null }));
    assert_eq!(get(&app, "/api/alive").await.json()["weather"], Value::Null);
    assert_eq!(get(&app, "/api/weather").await.json()["now"], Value::Null);
}

#[tokio::test]
async fn the_weather_keeps_itself_fresh_for_the_chosen_place() {
    let (app, _frame) = app_with_weather("weather-fresh").await;
    tokio::spawn(Arc::clone(&app.weather).keep_fresh(reqwest::Client::new(), Arc::clone(&app.cfg)));
    let shared = Arc::clone(&app);
    blocking(move || shared.update_settings(|settings| settings.place = types::Place::new("Porto", 41.15, -8.61)))
        .await;
    app.weather.place_changed();
    for _ in 0..200 {
        if get(&app, "/api/weather").await.json()["now"].is_object() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(get(&app, "/api/weather").await.json()["place"], "Porto");
    assert!(get(&app, "/api/weather").await.json()["now"].is_object(), "fetched without waiting half an hour");
}
