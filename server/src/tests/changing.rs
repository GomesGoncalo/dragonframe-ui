//! Route tests: changing.

use super::*;

#[tokio::test]
async fn delete_validates_its_input() {
    let (app, _frame) = app("delete-input");
    for body in [
        "{}",
        r#"{"paths":[]}"#,
        r#"{"paths":"x.jpg"}"#,
        r#"{"paths":["../x.jpg"]}"#,
        r#"{"paths":["ok.jpg", 5]}"#,
        "not json",
    ] {
        assert_eq!(post(&app, "/api/delete", body).await.status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn deleted_synced_photos_can_be_restored() {
    let (app, frame) = app("restore");
    frame.put(SYNCED, b"x");
    frame.put("frame-sync/fs_ffffffffffffffff.jpg", b"x");
    app.index.set_hash(&p(SYNCED), 5);
    let reply = post(
        &app,
        "/api/delete",
        r#"{"paths":["frame-sync/fs_0123456789abcdef.jpg","frame-sync/fs_ffffffffffffffff.jpg"]}"#,
    )
    .await;
    assert_eq!((reply.status, reply.json()["deleted"].as_u64()), (StatusCode::OK, Some(2)));
    assert_eq!(get(&app, "/api/state").await.json()["excluded"].as_array().unwrap().len(), 2);
    assert_eq!(app.index.get(&p(SYNCED)), index::Meta::default(), "the index forgets deleted photos");

    let reply = post(&app, "/api/restore", r#"{"names":["fs_0123456789abcdef.jpg","fs_dddddddddddddddd.jpg"]}"#).await;
    assert_eq!(reply.json()["restored"], 1);
    assert_eq!(get(&app, "/api/state").await.json()["excluded"], json!(["fs_ffffffffffffffff.jpg"]));
    assert_eq!(post(&app, "/api/restore", r#"{"names":"all"}"#).await.json()["restored"], 1);
    assert_eq!(get(&app, "/api/state").await.json()["excluded"], json!([]));
    for bad in [r#"{"names":"some"}"#, r#"{"names":["holiday.jpg"]}"#, "{}"] {
        assert_eq!(post(&app, "/api/restore", bad).await.status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn a_deleted_synced_photo_still_has_a_thumbnail_but_no_file() {
    let (app, frame) = app("deleted-thumb");
    frame.put(SYNCED, &picture(800, 600, image::ImageFormat::Jpeg));
    assert_eq!(
        post(&app, "/api/delete", r#"{"paths":["frame-sync/fs_0123456789abcdef.jpg"]}"#).await.status,
        StatusCode::OK
    );
    let thumb = get(&app, &format!("/thumb?p={SYNCED}")).await;
    assert_eq!((thumb.status, thumb.content_type()), (StatusCode::OK, "image/jpeg"));
    assert_eq!(get(&app, &format!("/file?p={SYNCED}")).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&app, "/api/state?full=1").await.json()["photos"],
        json!([]),
        "it is not listed as being on the frame"
    );
}

#[tokio::test]
async fn the_bin_can_be_emptied_and_emptied_photos_stay_off_the_frame() {
    let (app, frame) = app("bin-routes");
    frame.put(SYNCED, &picture(64, 48, image::ImageFormat::Jpeg));
    frame.put("frame-sync/fs_ffffffffffffffff.jpg", b"x");
    post(
        &app,
        "/api/delete",
        r#"{"paths":["frame-sync/fs_0123456789abcdef.jpg","frame-sync/fs_ffffffffffffffff.jpg"]}"#,
    )
    .await;
    let state = get(&app, "/api/state").await.json();
    assert_eq!(state["excluded"].as_array().unwrap().len(), 2);
    assert_eq!(state["deleted_at"].as_array().unwrap().len(), 2);
    assert_eq!((state["deleted_for_good"].as_u64(), state["bin_days"].as_u64()), (Some(0), Some(30)));
    assert_eq!(post(&app, "/api/restore", r#"{"names":["fs_ffffffffffffffff.jpg"]}"#).await.json()["restored"], 1);

    assert_eq!(post(&app, "/api/empty-bin", "{}").await.json()["emptied"], 1);
    let state = get(&app, "/api/state").await.json();
    assert_eq!((state["excluded"].clone(), state["deleted_for_good"].as_u64()), (json!([]), Some(1)));
    assert_eq!(
        get(&app, &format!("/thumb?p={SYNCED}")).await.status,
        StatusCode::NOT_FOUND,
        "its thumbnail went with it"
    );
    assert_eq!(
        store::load_excluded(&frame.0),
        [SyncedName::parse("fs_0123456789abcdef.jpg").unwrap()],
        "a sync will still leave it out"
    );
    assert_eq!(
        post(&app, "/api/restore", r#"{"names":"all"}"#).await.json()["restored"],
        0,
        "nothing in the bin to restore"
    );

    assert_eq!(post(&app, "/api/forget-emptied", "{}").await.json()["forgotten"], 1);
    assert!(store::load_excluded(&frame.0).is_empty());
    assert_eq!(get(&app, "/api/state").await.json()["deleted_for_good"], 0);
}

#[tokio::test]
async fn emptying_the_bin_needs_the_pin_when_one_is_set() {
    let (app, _frame) = app("bin-pin");
    post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    for path in ["/api/empty-bin", "/api/forget-emptied"] {
        assert_eq!(post(&app, path, "{}").await.status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn an_added_photo_is_stored_dated_and_fingerprinted() {
    let (app, frame) = app("add");
    let upload = page_post("/api/add").header("X-Filename", "beach%20day.jpg").header("X-Taken", "1700000000");
    let reply = send(&app, upload.body(Body::from(picture(800, 600, image::ImageFormat::Jpeg))).unwrap()).await;
    assert_eq!(reply.status, StatusCode::OK);
    let rel = reply.json()["rel"].as_str().unwrap().to_string();
    assert!(frame.0.cloud_dir.join(&rel).exists());
    let meta = app.index.get(&p(&rel));
    assert_eq!(meta.taken, Some(1_700_000_000));
    assert!(meta.hash.is_some() && meta.quality.is_some(), "ready for the duplicate finder straight away");
    let listed = get(&app, "/api/state?full=1").await.json()["photos"][0].clone();
    assert_eq!(
        (listed[0].as_str(), listed[1].as_str(), listed[2].as_u64()),
        (Some(rel.as_str()), Some("added"), Some(1_700_000_000))
    );
}

#[tokio::test]
async fn an_added_photo_with_no_or_a_silly_date_is_dated_now() {
    let (app, _frame) = app("add-undated");
    for (n, taken) in [None, Some("0"), Some("99999999999"), Some("yesterday")].into_iter().enumerate() {
        let side = 100 + u32::try_from(n).unwrap();
        let mut upload = page_post("/api/add");
        if let Some(taken) = taken {
            upload = upload.header("X-Taken", taken);
        }
        let reply = send(&app, upload.body(Body::from(picture(side, side, image::ImageFormat::Jpeg))).unwrap()).await;
        let stored = app.index.get(&p(reply.json()["rel"].as_str().unwrap())).taken.unwrap();
        assert!(now_secs().abs_diff(stored) < 60, "{taken:?} -> {stored}");
    }
}

#[tokio::test]
async fn adding_rejects_non_images_with_the_file_name() {
    let (app, _frame) = app("add-bad");
    let upload = page_post("/api/add").header("X-Filename", "holiday%20notes.txt");
    let reply = send(&app, upload.body(Body::from("just some text")).unwrap()).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json()["error"], "holiday notes.txt: not an image the frame can read (JPEG, PNG or WebP)");
}

#[tokio::test]
async fn album_links_and_daily_sync_are_saved() {
    let (app, _frame) = app("settings");
    assert_eq!(
        post(&app, "/api/sources", r#"{"sources":[" https://photos.app.goo.gl/abc ",""]}"#).await.status,
        StatusCode::OK
    );
    assert_eq!(post(&app, "/api/auto", r#"{"enabled":true}"#).await.json()["auto"], true);
    let state = get(&app, "/api/state?full=1").await.json();
    assert_eq!(state["sources"], json!(["https://photos.app.goo.gl/abc"]));
    assert_eq!(state["auto"], true);

    assert_eq!(post(&app, "/api/sources", r#"{"sources":["file:///etc"]}"#).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        get(&app, "/api/state?full=1").await.json()["sources"],
        json!(["https://photos.app.goo.gl/abc"]),
        "a bad save changes nothing"
    );
    assert_eq!(post(&app, "/api/auto", "{}").await.json()["auto"], false);
}

#[tokio::test]
async fn sync_needs_album_links_and_refuses_to_run_twice() {
    let (app, _frame) = app("sync");
    for body in ["{}", r#"{"sources":[]}"#, r#"{"sources":["  "]}"#, r#"{"sources":["/home/me/Pictures"]}"#] {
        assert_eq!(post(&app, "/api/sync", body).await.status, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(app.job.lock().unwrap().phase, Phase::Idle, "a rejected request starts nothing");

    assert!(gphotos::try_start(&app.job)); // as if a sync were in progress
    let reply = post(&app, "/api/sync", r#"{"sources":["https://photos.app.goo.gl/abc"]}"#).await;
    assert_eq!(
        (reply.status, reply.json()["error"].as_str()),
        (StatusCode::CONFLICT, Some("a sync is already running"))
    );
}

#[tokio::test]
async fn a_photo_can_be_made_a_favourite_or_hidden_and_a_hidden_one_is_left_out_of_the_slideshow() {
    let (app, frame) = app("marks");
    let other = "frame-sync/fs_fedcba9876543210.jpg";
    frame.put(SYNCED, b"x");
    frame.put(other, b"y");
    let slide = |slides: &Value, photo: &str| {
        slides["photos"].as_array().unwrap().iter().find(|slide| slide[0] == photo).map(|slide| slide[4].clone())
    };
    let mark = |photo: &str, mark: &str| format!(r#"{{"photo":"{photo}","mark":{mark}}}"#);

    let reply = post(&app, "/api/mark", &mark(SYNCED, r#""favourite""#)).await;
    assert_eq!((reply.status, reply.json()), (StatusCode::OK, json!({ "photo": SYNCED, "mark": "favourite" })));
    let slides = get(&app, "/api/slides").await.json();
    assert_eq!((slide(&slides, SYNCED), slide(&slides, other)), (Some(json!(true)), Some(json!(false))));

    // Hidden, it is off the frame's screen but still listed for whoever manages the photos.
    post(&app, "/api/mark", &mark(SYNCED, r#""hidden""#)).await;
    assert_eq!(slide(&get(&app, "/api/slides").await.json(), SYNCED), None);
    let state = get(&app, "/api/state?full=1").await.json();
    let listed = state["photos"].as_array().unwrap();
    assert_eq!(listed.iter().find(|photo| photo[0] == SYNCED).unwrap()[4], "hidden");
    assert_eq!(listed.iter().find(|photo| photo[0] == other).unwrap()[4], Value::Null);

    post(&app, "/api/mark", &mark(SYNCED, "null")).await;
    assert_eq!(slide(&get(&app, "/api/slides").await.json(), SYNCED), Some(json!(false)));

    // A mark needs a photo that is there, and goes when its photo does.
    assert_eq!(
        post(&app, "/api/mark", &mark("frame-sync/fs_0000000000000000.jpg", "null")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(post(&app, "/api/mark", &mark(SYNCED, r#""loved""#)).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(post(&app, "/api/mark", &mark("../settings.json", "null")).await.status, StatusCode::BAD_REQUEST);
    post(&app, "/api/mark", &mark(other, r#""favourite""#)).await;
    post(&app, "/api/delete", &format!(r#"{{"paths":["{other}"]}}"#)).await;
    assert!(app.marks.all().is_empty());
}
