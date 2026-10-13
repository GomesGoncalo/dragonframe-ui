//! Route tests: looking at faces in the background.

use super::*;

#[tokio::test]
async fn planning_queues_photos_with_look_alikes_and_checking_works_through_them() {
    let models = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    if !faces::MODEL_FILES.iter().all(|name| models.join(name).is_file()) {
        eprintln!("skipping: model files not downloaded");
        return;
    }
    let (app, frame) = app("face-queue");
    std::os::unix::fs::symlink(&models, faces::model_dir(&frame.0)).unwrap();
    for (name, hash) in [("a", 0xff00_u64), ("b", 0xff01), ("alone", 0x1234_5678_0000)] {
        let rel = format!("frame-sync/{name}.jpg");
        frame.put(&rel, &picture(120, 90, image::ImageFormat::Jpeg));
        app.index.set_hash(&p(&rel), hash);
    }
    frame.put("frame-sync/broken.jpg", b"not an image");
    app.index.set_hash(&p("frame-sync/broken.jpg"), 0xff03);

    assert_eq!(check_next_face(&app).await, None, "nothing is queued before planning");
    plan_face_checks(&app).await;
    assert_eq!(
        get(&app, "/api/status").await.json()["prepared"]["with_look_alikes"],
        3,
        "a, b and broken look alike; alone does not"
    );

    // Someone has b on screen: it goes first, with no rest after it.
    app.face_queue.lock().unwrap().watch(vec![p("frame-sync/b.jpg")]);
    assert_eq!(check_next_face(&app).await, Some(Duration::ZERO));
    assert!(app.index.get(&p("frame-sync/b.jpg")).people.is_some());
    assert!(app.index.get(&p("frame-sync/a.jpg")).people.is_none());

    // The rest follow in the background, with a rest after each; an unreadable photo is not retried.
    assert_eq!(check_next_face(&app).await, Some(BACKGROUND_PAUSE));
    assert_eq!(check_next_face(&app).await, Some(BACKGROUND_PAUSE));
    assert_eq!(check_next_face(&app).await, None, "the queue has run dry");
    assert_eq!(app.index.get(&p("frame-sync/broken.jpg")).people, Some(faces::People::default()));
    assert_eq!(app.index.get(&p("frame-sync/alone.jpg")).people, None, "a photo without look-alikes is left alone");
}
