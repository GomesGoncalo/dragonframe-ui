//! Route tests: reading.

use super::*;

#[tokio::test]
async fn the_page_is_served() {
    let (app, _frame) = app("page");
    let reply = get(&app, "/").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.content_type().starts_with("text/html"));
    assert!(String::from_utf8_lossy(&reply.body).contains("<title>Photo Frame</title>"));
    assert_eq!(reply.headers[X_CONTENT_TYPE_OPTIONS], "nosniff");
}

#[tokio::test]
async fn the_frames_own_screen_is_served() {
    let (app, _frame) = app("frame-screen");
    let reply = get(&app, "/frame").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.content_type().starts_with("text/html"));
    assert!(String::from_utf8_lossy(&reply.body).contains("<title>Frame</title>"));
    assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn slides_list_each_photo_with_its_date_and_size() {
    let (app, frame) = app("slides");
    frame.put(SYNCED, &picture(40, 30, image::ImageFormat::Jpeg));
    frame.put("frame-sync/up_0123456789abcdef.jpg", b"unreadable");
    app.index.set_taken(&p(SYNCED), 1_700_000_000);
    let (cfg, index) = (Arc::clone(&app.cfg), Arc::clone(&app.index));
    blocking(move || index::refresh(&cfg, &index)).await;
    let slides = get(&app, "/api/slides").await.json();
    let listed = slides["photos"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    let synced = listed.iter().find(|photo| photo[0] == SYNCED).unwrap();
    assert_eq!(synced, &json!([SYNCED, 1_700_000_000, 40, 30]));
    let unreadable = listed.iter().find(|photo| photo[0] != SYNCED).unwrap();
    assert_eq!((&unreadable[2], &unreadable[3]), (&Value::Null, &Value::Null), "not measured: the screen works it out");
    assert!(unreadable[1].as_u64().unwrap() > 1_700_000_000, "undated photos use their arrival time");
}
