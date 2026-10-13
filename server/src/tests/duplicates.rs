//! Route tests: duplicates and status.

use super::*;

#[tokio::test]
async fn duplicates_groups_look_alikes_with_the_best_photo_first() {
    let (app, frame) = app("duplicates");
    let quality = |sharpness| index::Quality { sharpness, clipped: 0 };
    for (name, taken, hash, sharpness) in [
        ("a", 300, 0xff00_u64, 50),
        ("b", 100, 0xff00, 900),
        ("c", 200, 0xff01, 400),
        ("d", 50, 0x1234_5678, 10),
        ("e", 10, 0, 10),
    ] {
        let rel = format!("frame-sync/{name}.jpg");
        frame.put(&rel, b"x");
        app.index.set_taken(&p(&rel), taken);
        app.index.set_hash(&p(&rel), hash);
        app.index.set_quality(&p(&rel), quality(sharpness));
    }
    frame.put("frame-sync/unanalysed.jpg", b"x");

    let exact = get(&app, "/api/duplicates").await.json();
    assert_eq!(
        exact["groups"],
        json!([{
            "photos": ["frame-sync/b.jpg", "frame-sync/a.jpg"],
            "ranked": true,
            "by": "sharpness",
            "quality": [[900, 0], [50, 0]],
            "people": [null, null],
        }])
    );
    assert_eq!((exact["found"].as_u64(), exact["pending"].as_u64()), (Some(1), Some(1)));

    let similar = get(&app, "/api/duplicates?d=2").await.json();
    assert_eq!(
        similar["groups"][0]["photos"],
        json!(["frame-sync/b.jpg", "frame-sync/c.jpg", "frame-sync/a.jpg"]),
        "sharpest first"
    );
    assert_eq!(
        get(&app, "/api/duplicates?d=junk").await.json()["groups"],
        exact["groups"],
        "a bad distance means exact"
    );

    // Heavy clipping can cost the sharpest photo its lead.
    app.index.set_quality(&p("frame-sync/b.jpg"), index::Quality { sharpness: 900, clipped: 900 });
    assert_eq!(get(&app, "/api/duplicates?d=2").await.json()["groups"][0]["photos"][0], "frame-sync/c.jpg");
}

#[tokio::test]
async fn with_faces_analysed_the_best_expressions_come_first() {
    let (app, frame) = app("duplicates-people");
    let shots = [
        ("sharp-blink", 900, faces::People { faces: 2, eyes_open: 100, smiling: 160 }),
        ("soft-great", 300, faces::People { faces: 2, eyes_open: 196, smiling: 180 }),
        ("one-turned-away", 800, faces::People { faces: 1, eyes_open: 100, smiling: 100 }),
    ];
    for (taken, (name, sharpness, people)) in (0u64..).zip(shots) {
        let rel = format!("frame-sync/{name}.jpg");
        frame.put(&rel, b"x");
        app.index.set_taken(&p(&rel), taken);
        app.index.set_hash(&p(&rel), 0xbeef);
        app.index.set_quality(&p(&rel), index::Quality { sharpness, clipped: 0 });
        app.index.set_people(&p(&rel), people);
    }
    let group = get(&app, "/api/duplicates").await.json()["groups"][0].clone();
    assert_eq!(group["by"], "expressions");
    assert_eq!(
        group["photos"],
        json!(["frame-sync/soft-great.jpg", "frame-sync/sharp-blink.jpg", "frame-sync/one-turned-away.jpg"])
    );
    assert_eq!(
        group["people"],
        json!([[2, 98, 90], [2, 50, 80], [1, 100, 100]]),
        "faces, then average eyes open and smiling"
    );
}

#[tokio::test]
async fn reporting_what_is_on_screen_queues_it_and_returns_the_current_ranking() {
    let (app, frame) = app("watching");
    for (name, sharpness) in [("a", 100), ("b", 900), ("c", 400)] {
        let rel = format!("frame-sync/{name}.jpg");
        frame.put(&rel, b"x");
        app.index.set_quality(&p(&rel), index::Quality { sharpness, clipped: 0 });
    }
    app.index.set_people(&p("frame-sync/a.jpg"), faces::People::default());
    let body = r#"{"groups":[["frame-sync/a.jpg","frame-sync/b.jpg"],["frame-sync/c.jpg"]]}"#;
    let reply = post(&app, "/api/watching", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let groups = reply.json()["groups"].clone();
    assert_eq!(
        groups[0]["photos"],
        json!(["frame-sync/b.jpg", "frame-sync/a.jpg"]),
        "ranked with what is known so far"
    );
    assert_eq!(groups[0]["people"], json!([null, [0, 0, 0]]));
    assert_eq!(groups[1]["photos"], json!(["frame-sync/c.jpg"]));

    // The photos on screen that still need a look are now first in the queue.
    let mut queue = app.face_queue.lock().unwrap();
    let done = |photo: &PhotoPath| app.index.get(photo).people.is_some();
    assert_eq!(queue.next(done), Some(faces::Next::Watched(p("frame-sync/b.jpg"))));
}

#[tokio::test]
async fn reporting_what_is_on_screen_validates_its_input_and_needs_no_pin() {
    let (app, _frame) = app("watching-input");
    let too_many = json!({ "groups": vec![vec![SYNCED]; MAX_WATCHED_GROUPS + 1] }).to_string();
    for body in
        ["{}", r#"{"groups":[[]]}"#, r#"{"groups":[["../x.jpg"]]}"#, r#"{"groups":["x.jpg"]}"#, too_many.as_str()]
    {
        assert_eq!(
            post(&app, "/api/watching", body).await.status,
            StatusCode::BAD_REQUEST,
            "{}",
            &body[..body.len().min(40)]
        );
    }
    assert_eq!(post(&app, "/api/watching", r#"{"groups":[]}"#).await.json()["groups"], json!([]));
    post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    assert_eq!(
        post(&app, "/api/watching", r#"{"groups":[]}"#).await.status,
        StatusCode::OK,
        "watching changes no photos"
    );
}

#[tokio::test]
async fn analysing_validates_its_input_and_needs_the_models() {
    let (app, frame) = app("analyse-input");
    frame.put(SYNCED, b"x");
    let too_many = json!({ "paths": vec![SYNCED; MAX_ANALYSE + 1] }).to_string();
    for body in ["{}", r#"{"paths":[]}"#, r#"{"paths":["../x.jpg"]}"#, too_many.as_str()] {
        assert_eq!(
            post(&app, "/api/analyse", body).await.status,
            StatusCode::BAD_REQUEST,
            "{}",
            &body[..body.len().min(40)]
        );
    }
    let reply = post(&app, "/api/analyse", r#"{"paths":["frame-sync/fs_0123456789abcdef.jpg"]}"#).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.json()["error"], "the face models are not installed on the frame");
}

#[tokio::test]
async fn analysing_is_allowed_without_the_pin_because_it_changes_no_photos() {
    let (app, frame) = app("analyse-pin");
    frame.put(SYNCED, b"x");
    post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    let reply = post(&app, "/api/analyse", r#"{"paths":["frame-sync/fs_0123456789abcdef.jpg"]}"#).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "refused for lack of models, not for lack of a PIN");
    let foreign = Request::post("/api/analyse").header(HOST, HOST_NAME).body(Body::from("{}")).unwrap();
    assert_eq!(send(&app, foreign).await.status, StatusCode::FORBIDDEN, "other websites still cannot trigger it");
}

/// Uses the real models when `deploy.py` has downloaded them beside the crate.
#[tokio::test]
async fn analysing_a_group_looks_at_each_photo_once_and_returns_the_ranking() {
    let models = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    if !faces::MODEL_FILES.iter().all(|name| models.join(name).is_file()) {
        eprintln!("skipping: model files not downloaded");
        return;
    }
    let (app, frame) = app("analyse-real");
    std::os::unix::fs::symlink(&models, faces::model_dir(&frame.0)).unwrap();
    for (name, side) in [("a", 300), ("b", 320)] {
        frame.put(&format!("frame-sync/{name}.jpg"), &picture(side, side, image::ImageFormat::Jpeg));
    }
    let body = r#"{"paths":["frame-sync/a.jpg","frame-sync/b.jpg","frame-sync/missing.jpg"]}"#;
    let reply = post(&app, "/api/analyse", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let result = reply.json();
    assert_eq!(result["failed"], 1, "the missing photo is reported, the rest still analysed");
    assert_eq!(result["group"]["by"], "sharpness", "no faces in plain pictures, so sharpness decides");
    let nobody = Some(faces::People::default());
    assert_eq!(
        (app.index.get(&p("frame-sync/a.jpg")).people, app.index.get(&p("frame-sync/b.jpg")).people),
        (nobody, nobody)
    );
    assert!(
        app.index.get(&p("frame-sync/a.jpg")).quality.is_some(),
        "quality is measured on the way if it was missing"
    );

    // A second request has nothing left to look at and answers from what is remembered.
    app.index.set_people(&p("frame-sync/a.jpg"), faces::People { faces: 1, eyes_open: 100, smiling: 100 });
    app.index.set_people(&p("frame-sync/b.jpg"), faces::People { faces: 1, eyes_open: 10, smiling: 0 });
    let again = post(&app, "/api/analyse", r#"{"paths":["frame-sync/b.jpg","frame-sync/a.jpg"]}"#).await.json();
    assert_eq!(again["group"]["by"], "expressions");
    assert_eq!(again["group"]["photos"], json!(["frame-sync/a.jpg", "frame-sync/b.jpg"]));
    assert!(app.models.unload_if_idle(Duration::ZERO), "the models were loaded for this and can be dropped again");
}

#[tokio::test]
async fn a_group_with_an_unmeasured_photo_is_not_ranked() {
    let (app, frame) = app("duplicates-unranked");
    for (name, taken) in [("a", 1), ("b", 2), ("c", 3)] {
        let rel = format!("frame-sync/{name}.jpg");
        frame.put(&rel, b"x");
        app.index.set_taken(&p(&rel), taken);
        app.index.set_hash(&p(&rel), 0xabcd);
    }
    app.index.set_quality(&p("frame-sync/c.jpg"), index::Quality { sharpness: 5, clipped: 0 });
    let found = get(&app, "/api/duplicates").await.json();
    assert_eq!(found["groups"][0]["ranked"], false);
    assert_eq!(found["groups"][0]["photos"][0], "frame-sync/c.jpg", "measured photos still come first");
    assert_eq!(found["groups"][0]["quality"], json!([[5, 0], null, null]), "quality lines up with the photos");
    assert_eq!(found["pending"], 2);
}

#[tokio::test]
async fn status_reports_storage_by_source_sync_health_and_pin() {
    let (app, frame) = app("status");
    frame.put(SYNCED, b"12345");
    frame.put("frame-sync/up_0123456789abcdef.jpg", b"123");
    frame.put("9user/photos/a.webp", b"1");
    app.index.set_hash(&p(SYNCED), 1);
    post(&app, "/api/sources", r#"{"sources":["https://photos.app.goo.gl/abc"]}"#).await;

    let status = get(&app, "/api/status").await.json();
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["photos"]["count"], 3);
    assert_eq!(status["photos"]["bytes"], 9);
    assert_eq!(
        status["photos"]["by_source"],
        json!({
            "synced": {"count": 1, "bytes": 5}, "added": {"count": 1, "bytes": 3}, "phone": {"count": 1, "bytes": 1},
        })
    );
    assert_eq!(status["prepared"]["fingerprints"], 1);
    assert_eq!((status["prepared"]["quality"].as_u64(), status["prepared"]["people"].as_u64()), (Some(0), Some(0)));
    assert_eq!(status["prepared"]["face_models"], false);
    assert_eq!(
        (status["prepared"]["with_look_alikes"].as_u64(), status["prepared"]["face_queue"].as_u64()),
        (Some(0), Some(0))
    );
    assert_eq!(status["sync"]["albums"], 1);
    assert_eq!(
        (status["sync"]["last"].clone(), status["sync"]["next"].clone(), status["sync"]["running"].as_bool()),
        (Value::Null, Value::Null, Some(false))
    );
    assert_eq!(status["pin"], false);

    // After a sync finishes, the outcome and the next daily run show up.
    app.update_settings(|settings| {
        settings.daily_sync = true;
        settings.last_sync = Some(LastSync {
            at: 1_700_000_000,
            succeeded: false,
            summary: Some("could not open the album link".into()),
        });
    });
    let sync = get(&app, "/api/status").await.json()["sync"].clone();
    assert_eq!((sync["last"].as_u64(), sync["next"].as_u64()), (Some(1_700_000_000), Some(1_700_000_000 + DAY)));
    assert_eq!((sync["ok"].as_bool(), sync["summary"].as_str()), (Some(false), Some("could not open the album link")));
}
