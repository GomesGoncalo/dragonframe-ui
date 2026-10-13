//! Route tests: protection.

use super::*;

#[tokio::test]
async fn other_websites_cannot_make_changes() {
    let (app, frame) = app("csrf");
    frame.put(SYNCED, b"x");
    let body = || Body::from(r#"{"paths":["frame-sync/fs_0123456789abcdef.jpg"]}"#);
    let base = || Request::post("/api/delete").header(HOST, HOST_NAME);

    // A plain cross-site form post: no marker header.
    assert_eq!(send(&app, base().body(body()).unwrap()).await.status, StatusCode::FORBIDDEN);
    // A script on another site: wrong Origin, even with the header.
    let foreign = base().header("X-Frame-App", "1").header(ORIGIN, "http://evil.example");
    assert_eq!(send(&app, foreign.body(body()).unwrap()).await.status, StatusCode::FORBIDDEN);
    assert!(frame.0.cloud_dir.join(SYNCED).exists(), "nothing was deleted");

    // The page itself, and tools that send no Origin, are allowed.
    assert_eq!(send(&app, base().header("X-Frame-App", "1").body(body()).unwrap()).await.status, StatusCode::OK);
    assert!(!frame.0.cloud_dir.join(SYNCED).exists());
}

#[tokio::test]
async fn with_a_pin_changes_need_an_unlocked_session_but_viewing_does_not() {
    let (app, frame) = app("pin");
    frame.put(SYNCED, b"x");
    let delete = r#"{"paths":["frame-sync/fs_0123456789abcdef.jpg"]}"#;

    let reply = post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    assert_eq!(reply.status, StatusCode::OK);
    let owner = reply.json()["token"].as_str().unwrap().to_string();

    // Locked out without a session, for every kind of change.
    for (path, body) in [
        ("/api/delete", delete),
        ("/api/add", "x"),
        ("/api/sync", "{}"),
        ("/api/pin", r#"{"pin":""}"#),
        ("/api/auto", "{}"),
    ] {
        let reply = post(&app, path, body).await;
        assert_eq!(
            (reply.status, reply.json()["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("PIN required")),
            "{path}"
        );
    }
    assert_eq!(post_as(&app, "made-up-token", "/api/delete", delete).await.status, StatusCode::UNAUTHORIZED);
    assert!(frame.0.cloud_dir.join(SYNCED).exists());

    // Viewing stays open, and says whether this browser is unlocked.
    let state = get(&app, "/api/state?full=1").await.json();
    assert_eq!(
        (state["pin"].as_bool(), state["unlocked"].as_bool(), state["photos"].as_array().map(Vec::len)),
        (Some(true), Some(false), Some(1))
    );
    assert_eq!(get(&app, &format!("/file?p={SYNCED}")).await.status, StatusCode::OK);
    let mine =
        send(&app, Request::get("/api/state").header("X-Frame-Token", &owner).body(Body::empty()).unwrap()).await;
    assert_eq!(mine.json()["unlocked"], true);

    // Unlocking with the PIN gives a session that works.
    let wrong = post(&app, "/api/unlock", r#"{"pin":"0000"}"#).await;
    assert_eq!(
        (wrong.status, wrong.json()["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("Wrong PIN. 4 tries left."))
    );
    let token = post(&app, "/api/unlock", r#"{"pin":"2468"}"#).await.json()["token"].as_str().unwrap().to_string();
    assert_eq!(post_as(&app, &token, "/api/delete", delete).await.status, StatusCode::OK);
    assert!(!frame.0.cloud_dir.join(SYNCED).exists());
}

#[tokio::test]
async fn the_pin_can_be_changed_and_removed_only_when_unlocked() {
    let (app, _frame) = app("pin-change");
    for bad in [r#"{"pin":"12"}"#, r#"{"pin":"abcd"}"#, r#"{"pin":"123456789"}"#] {
        assert_eq!(post(&app, "/api/pin", bad).await.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let first = post(&app, "/api/pin", r#"{"pin":"2468"}"#).await.json()["token"].as_str().unwrap().to_string();
    let second =
        post_as(&app, &first, "/api/pin", r#"{"pin":"1357"}"#).await.json()["token"].as_str().unwrap().to_string();
    assert_eq!(
        post_as(&app, &first, "/api/auto", "{}").await.status,
        StatusCode::UNAUTHORIZED,
        "the old session ended"
    );
    assert_eq!(
        post(&app, "/api/unlock", r#"{"pin":"2468"}"#).await.status,
        StatusCode::FORBIDDEN,
        "the old PIN is gone"
    );

    assert_eq!(post_as(&app, &second, "/api/pin", r#"{"pin":""}"#).await.json()["pin"], false);
    assert_eq!(post(&app, "/api/auto", "{}").await.status, StatusCode::OK, "no PIN, no lock");
    assert_eq!(get(&app, "/api/state").await.json()["pin"], false);
}

#[tokio::test]
async fn too_many_wrong_pins_lock_out_guessing() {
    let (app, _frame) = app("pin-lockout");
    post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;
    for _ in 0..4 {
        assert_eq!(post(&app, "/api/unlock", r#"{"pin":"1111"}"#).await.status, StatusCode::FORBIDDEN);
    }
    let locked = post(&app, "/api/unlock", r#"{"pin":"1111"}"#).await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.json()["error"].as_str().unwrap().starts_with("Too many wrong PINs"));
    assert_eq!(
        post(&app, "/api/unlock", r#"{"pin":"2468"}"#).await.status,
        StatusCode::TOO_MANY_REQUESTS,
        "even the right PIN waits"
    );
}
