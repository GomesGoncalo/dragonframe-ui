//! Route tests: the calendar.

use super::*;

const OCTOBER_2026: &str = "from=1790812800&to=1793491200";

#[tokio::test]
async fn events_can_be_added_listed_and_deleted() {
    let (app, _frame) = app("events");
    let empty = get(&app, "/api/calendar").await.json();
    assert_eq!((&empty["events"], &empty["feeds"], &empty["added"]), (&json!([]), &json!([]), &json!([])));

    let reply = post(&app, "/api/events/add", r#"{"title":" Dentist ","day":"2026-10-13","time":"14:30"}"#).await;
    assert_eq!(reply.status, StatusCode::OK);
    let id = reply.json()["event"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        reply.json()["event"],
        json!({ "id": id, "title": "Dentist", "day": "2026-10-13", "time": "14:30", "repeat": "never" })
    );
    post(&app, "/api/events/add", r#"{"title":"Bins","day":"2026-10-01","time":null,"repeat":"weekly"}"#).await;

    let calendar = get(&app, &format!("/api/calendar?{OCTOBER_2026}")).await.json();
    let events = calendar["events"].as_array().unwrap();
    assert_eq!(events.len(), 6, "five Thursdays and the dentist");
    let dentist = events.iter().find(|event| event["title"] == "Dentist").unwrap();
    assert_eq!(
        (dentist["start"].as_str(), dentist["all_day"].as_bool(), dentist["added"].as_str()),
        (Some("2026-10-13T14:30:00"), Some(false), Some(id.as_str()))
    );
    assert_eq!(
        (&events[0]["start"], &events[0]["end"], &events[0]["all_day"]),
        (&json!("2026-10-01"), &json!("2026-10-02"), &json!(true))
    );
    assert_eq!(calendar["added"].as_array().unwrap().len(), 2);
    assert_eq!(dentist["feed"], Value::Null, "an event added by hand belongs to no subscribed calendar");
    assert!(get(&app, "/api/calendar?from=0&to=1").await.json()["events"].as_array().unwrap().is_empty());

    let deleted = post(&app, "/api/events/delete", &format!(r#"{{"id":"{id}"}}"#)).await.json();
    assert_eq!(deleted["deleted"], true);
    assert_eq!(post(&app, "/api/events/delete", &format!(r#"{{"id":"{id}"}}"#)).await.json()["deleted"], false);
    assert_eq!(get(&app, &format!("/api/calendar?{OCTOBER_2026}")).await.json()["events"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn an_event_that_does_not_parse_is_refused_with_the_reason() {
    let (app, _frame) = app("events-bad");
    for (body, reason) in [
        (r#"{"title":"","day":"2026-10-13"}"#, "1 to 120 characters"),
        (r#"{"title":"x","day":"13/10/2026"}"#, "not a date"),
        (r#"{"title":"x","day":"2026-10-13","time":"2pm"}"#, "not a time"),
        (r#"{"title":"x","day":"2026-10-13","repeat":"hourly"}"#, "unknown variant"),
        (r#"{"title":"x"}"#, "missing field"),
    ] {
        let reply = post(&app, "/api/events/add", body).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        assert!(reply.json()["error"].as_str().unwrap().contains(reason), "{body}: {}", reply.json());
    }
    assert_eq!(post(&app, "/api/events/delete", r#"{"id":"../x"}"#).await.status, StatusCode::BAD_REQUEST);
    assert!(get(&app, "/api/calendar").await.json()["added"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn subscribed_calendars_are_saved_fetched_and_shown() {
    use calendar::testing::{ics, server};
    let (app, frame) = app("calendars");
    let base = server(Arc::new(Mutex::new(ics(&[("School play", "20261020T180000Z")])))).await;
    // A test server cannot offer https, so the link goes in the way the settings file would hold it.
    app.calendars.set_links(&[types::CalendarLink::local(&format!("{base}/family.ics"))]);
    app.calendars.refresh(&reqwest::Client::new()).await;
    let calendar = get(&app, &format!("/api/calendar?{OCTOBER_2026}")).await.json();
    assert_eq!(calendar["events"][0]["title"], "School play");
    assert_eq!(calendar["events"][0]["calendar"], "Family");
    assert_eq!(calendar["events"][0]["feed"], 0, "which subscribed calendar, for its colour");
    assert_eq!(
        (calendar["feeds"][0]["events"].as_u64(), calendar["feeds"][0]["name"].as_str()),
        (Some(1), Some("Family"))
    );
    assert_eq!(calendar["feeds"][0]["link"].as_str(), Some(format!("{base}/family.ics").as_str()));

    // Saving links through the page replaces them, and remembers them.
    let saved = post(
        &app,
        "/api/calendars",
        r#"{"links":["webcal://calendar.invalid/a.ics","","https://calendar.invalid/b.ics"]}"#,
    )
    .await;
    assert_eq!((saved.status, saved.json()["calendars"].as_u64()), (StatusCode::OK, Some(2)));
    let links: Vec<String> =
        store::load_settings(&frame.0).calendars.iter().map(|link| link.as_str().to_string()).collect();
    assert_eq!(links, ["https://calendar.invalid/a.ics", "https://calendar.invalid/b.ics"]);
    let calendar = get(&app, "/api/calendar").await.json();
    assert_eq!(calendar["feeds"].as_array().unwrap().len(), 2);
    assert!(calendar["events"].as_array().unwrap().is_empty(), "the calendar that was dropped is gone");

    let refused = post(&app, "/api/calendars", r#"{"links":["http://plain.example/a.ics"]}"#).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(refused.json()["error"].as_str().unwrap().contains("https:// or webcal://"));
}

#[tokio::test]
async fn with_a_pin_the_calendar_shows_but_its_links_and_changes_need_unlocking() {
    let (app, _frame) = app("calendar-pin");
    post(&app, "/api/calendars", r#"{"links":["https://calendar.invalid/SECRET.ics"]}"#).await;
    post(&app, "/api/events/add", r#"{"title":"Swim","day":"2026-10-13"}"#).await;
    let token = post(&app, "/api/pin", r#"{"pin":"2468"}"#).await.json()["token"].as_str().unwrap().to_string();

    let locked = get(&app, &format!("/api/calendar?{OCTOBER_2026}")).await;
    assert_eq!(locked.json()["events"][0]["title"], "Swim", "the frame's own screen needs no PIN to show the calendar");
    assert_eq!(locked.json()["unlocked"], false);
    assert_eq!(locked.json()["feeds"][0]["link"], Value::Null);
    assert_eq!(locked.json()["feeds"][0]["host"], "calendar.invalid");
    assert!(!String::from_utf8_lossy(&locked.body).contains("SECRET"));
    for (path, body) in [
        ("/api/calendars", r#"{"links":[]}"#),
        ("/api/events/add", r#"{"title":"x","day":"2026-10-13"}"#),
        ("/api/events/delete", r#"{"id":"0123456789abcdef"}"#),
    ] {
        assert_eq!(post(&app, path, body).await.status, StatusCode::UNAUTHORIZED, "{path}");
    }

    let request = Request::get("/api/calendar").header("X-Frame-Token", &token).body(Body::empty()).unwrap();
    let unlocked = send(&app, request).await.json();
    assert_eq!(unlocked["feeds"][0]["link"], "https://calendar.invalid/SECRET.ics");
    assert_eq!(unlocked["unlocked"], true);
    assert_eq!(post_as(&app, &token, "/api/calendars", r#"{"links":[]}"#).await.status, StatusCode::OK);
}

#[tokio::test]
async fn state_lists_photos_only_when_asked() {
    let (app, frame) = app("state");
    frame.put(SYNCED, b"x");
    app.index.set_taken(&p(SYNCED), 1_700_000_000);
    let light = get(&app, "/api/state").await.json();
    assert!(light.get("photos").is_none());
    assert_eq!(light["job"]["running"], false);
    assert_eq!(
        (light["auto"].as_bool(), light["pin"].as_bool(), light["unlocked"].as_bool()),
        (Some(false), Some(false), Some(true))
    );

    let full = get(&app, "/api/state?full=1").await.json();
    let photo = &full["photos"][0];
    assert_eq!(
        (photo[0].as_str(), photo[1].as_str(), photo[2].as_u64()),
        (Some(SYNCED), Some("synced"), Some(1_700_000_000))
    );
    assert!(photo[3].as_u64().unwrap() > 1_700_000_000, "the fourth field is when the file arrived");
    assert_eq!(full["sources"], json!([]));
}

#[tokio::test]
async fn a_photo_without_a_known_date_uses_its_arrival_time() {
    let (app, frame) = app("state-undated");
    frame.put(SYNCED, b"x");
    let photo = get(&app, "/api/state?full=1").await.json()["photos"][0].clone();
    assert_eq!(photo[2], photo[3]);
}

#[tokio::test]
async fn photos_and_thumbnails_are_served_and_cacheable() {
    let (app, frame) = app("files");
    let original = picture(1920, 1080, image::ImageFormat::Jpeg);
    frame.put(SYNCED, &original);
    let reply = get(&app, "/file?p=frame-sync%2Ffs_0123456789abcdef.jpg").await;
    assert_eq!((reply.status, reply.content_type()), (StatusCode::OK, "image/jpeg"));
    assert_eq!(reply.body, original);
    assert!(reply.headers[CACHE_CONTROL].to_str().unwrap().contains("immutable"));

    let thumb = get(&app, &format!("/thumb?p={SYNCED}")).await;
    assert_eq!(thumb.content_type(), "image/jpeg");
    assert!(thumb.body.len() < original.len());
}

#[tokio::test]
async fn a_thumbnail_that_cannot_be_made_falls_back_to_the_photo() {
    let (app, frame) = app("thumb-fallback");
    frame.put(SYNCED, b"not really a jpeg");
    assert_eq!(get(&app, &format!("/thumb?p={SYNCED}")).await.body, "not really a jpeg");
}

#[tokio::test]
async fn files_outside_the_photo_folder_cannot_be_read() {
    let (app, frame) = app("traversal");
    std::fs::write(frame.0.data_dir.join("secret.jpg"), b"secret").unwrap();
    frame.put(".downloading/partial.jpg", b"x");
    for path in [
        "/file?p=../data/secret.jpg",
        "/file?p=..%2Fdata%2Fsecret.jpg",
        "/file?p=%2Fetc%2Fhosts.jpg",
        "/file?p=.downloading/partial.jpg",
        "/thumb?p=../data/secret.jpg",
        "/file?p=frame-sync/missing.jpg",
        "/file",
    ] {
        assert_eq!(get(&app, path).await.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn unknown_routes_are_404_and_changes_are_post_only() {
    let (app, _frame) = app("unknown");
    let reply = get(&app, "/api/nope").await;
    assert_eq!((reply.status, reply.json()["error"].as_str()), (StatusCode::NOT_FOUND, Some("not found")));
    assert_eq!(get(&app, "/api/delete").await.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(post(&app, "/api/nope", "{}").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn recent_log_lines_are_served_as_text() {
    let (app, frame) = app("logs");
    let empty = get(&app, "/logs").await;
    assert!(empty.content_type().starts_with("text/plain"));
    assert_eq!(empty.body, "\n", "no log files yet");

    let dir = store::log_dir(&frame.0);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("frameserver.2026-10-11.log"), "old day\n").unwrap();
    let today = (1..=600).fold(String::new(), |text, n| text + &format!("line {n}\n"));
    std::fs::write(dir.join("frameserver.2026-10-12.log"), today).unwrap();
    let text = String::from_utf8(get(&app, "/logs").await.body.to_vec()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!((lines.len(), lines[0], lines[499]), (500, "line 101", "line 600"));
}

#[tokio::test]
async fn reminder_rules_are_kept_and_each_event_says_which_rules_are_about_it() {
    let (app, _frame) = app("reminders");
    assert_eq!(get(&app, "/api/calendar").await.json()["reminders"], json!([]));
    post(&app, "/api/events/add", r#"{"title":"Black bin","day":"2026-10-01","time":null,"repeat":"weekly"}"#).await;
    post(&app, "/api/events/add", r#"{"title":"Robin's swimming","day":"2026-10-13","time":"16:00"}"#).await;

    let rules = json!([
        { "words": "bin, recycling", "when": { "days": 1, "at": "19:00" }, "say": "Put the bins out", "chime": true, "then": null },
        { "words": "Swimming", "when": { "minutes": 45 }, "say": null, "chime": false, "then": null },
    ]);
    let saved = post(&app, "/api/reminders", &json!({ "rules": rules }).to_string()).await;
    assert_eq!((saved.status, saved.json()), (StatusCode::OK, json!({ "reminders": 2 })));

    let calendar = get(&app, &format!("/api/calendar?{OCTOBER_2026}")).await.json();
    assert_eq!(calendar["reminders"], rules);
    for event in calendar["events"].as_array().unwrap() {
        let about = if event["title"] == "Black bin" { json!([0]) } else { json!([1]) };
        assert_eq!(event["remind"], about, "{event}");
    }
    // The frame's screen is told the rules whenever it reports in.
    assert_eq!(get(&app, "/api/alive").await.json()["reminders"], rules);
    assert_eq!(get(&app, "/api/device").await.json()["reminders"], rules);

    for bad in [
        r#"{"rules":[{"words":"","when":{"minutes":5}}]}"#,
        r#"{"rules":[{"words":"bin","when":{"days":1}}]}"#,
        r#"{"rules":[{"words":"bin"}]}"#,
        "{}",
    ] {
        assert_eq!(post(&app, "/api/reminders", bad).await.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(get(&app, "/api/calendar").await.json()["reminders"], rules, "a refused change changes nothing");
    post(&app, "/api/reminders", r#"{"rules":[]}"#).await;
    assert_eq!(get(&app, "/api/calendar").await.json()["reminders"], json!([]));
}
