//! Tests of the HTTP routes, driving the real router without a network socket.

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::*;
use store::testing::{picture, TempFrame};

const SYNCED: &str = "frame-sync/fs_0123456789abcdef.jpg";
const HOST_NAME: &str = "192.168.4.23:8080";

fn p(rel: &str) -> PhotoPath {
    PhotoPath::parse(rel).unwrap()
}

fn app(name: &str) -> (Shared, TempFrame) {
    let frame = TempFrame::new(&format!("route-{name}"));
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    (App::new(cfg), frame)
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {:?}", self.body))
    }

    fn content_type(&self) -> &str {
        self.headers[CONTENT_TYPE].to_str().unwrap()
    }
}

async fn send(app: &Shared, request: Request<Body>) -> Reply {
    let response = router(Arc::clone(app)).oneshot(request).await.unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Reply { status, headers, body }
}

async fn get(app: &Shared, path: &str) -> Reply {
    send(app, Request::get(path).body(Body::empty()).unwrap()).await
}

/// A POST as the page itself would send it.
fn page_post(path: &str) -> axum::http::request::Builder {
    Request::post(path).header(HOST, HOST_NAME).header(ORIGIN, format!("http://{HOST_NAME}")).header("X-Frame-App", "1")
}

async fn post(app: &Shared, path: &str, body: &str) -> Reply {
    send(app, page_post(path).body(Body::from(body.to_string())).unwrap()).await
}

async fn post_as(app: &Shared, token: &str, path: &str, body: &str) -> Reply {
    send(app, page_post(path).header("X-Frame-Token", token).body(Body::from(body.to_string())).unwrap()).await
}

// --- helpers ---------------------------------------------------------------------

#[test]
fn percent_decoding() {
    assert_eq!(percent_decode("holiday%20notes.txt"), "holiday notes.txt");
    assert_eq!(percent_decode("caf%C3%A9"), "café");
    assert_eq!(percent_decode("a+b"), "a+b", "plus is literal outside query strings");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz%4"), "%zz%4");
    assert_eq!(percent_decode(""), "");
}

#[test]
fn content_types_follow_the_extension() {
    assert_eq!(content_type(&p("a/b.webp")), "image/webp");
    assert_eq!(content_type(&p("a/b.PNG")), "image/png");
    assert_eq!(content_type(&p("a/b.jpg")), "image/jpeg");
    assert_eq!(content_type(&p("a/b.jpeg")), "image/jpeg");
}

#[test]
fn request_bodies_parse_into_valid_values_or_not_at_all() {
    let parse = |json: &str| serde_json::from_str::<DeleteRequest>(json).map(|request| request.paths.len());
    assert_eq!(parse(r#"{"paths":["a/b.jpg","c.png"]}"#).unwrap(), 2);
    for bad in ["{}", r#"{"paths":[]}"#, r#"{"paths":["../x.jpg"]}"#, r#"{"paths":"a.jpg"}"#] {
        assert!(parse(bad).is_err(), "{bad}");
    }
    let sync: SyncRequest = serde_json::from_str(r#"{"sources":[" https://a ",""]}"#).unwrap();
    assert_eq!((sync.sources.0.len(), sync.delete, sync.refresh, sync.limit), (1, false, false, None));
    assert!(serde_json::from_str::<SyncRequest>(r#"{"sources":["https://a"],"limit":-1}"#).is_err());
    assert!(
        serde_json::from_str::<WatchingRequest>(r#"{"groups":[["a.jpg"],[]]}"#).is_err(),
        "an empty group is not a group"
    );
    assert!(!serde_json::from_str::<DailySyncRequest>("{}").unwrap().enabled);
}

// --- reading -----------------------------------------------------------------------

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

// --- protection ------------------------------------------------------------------------

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

// --- changing -----------------------------------------------------------------------------

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

// --- syncing, against a stand-in for Google Photos -----------------------------------------------

/// An app whose album listings come from a fake Google serving `items`.
async fn app_with_google(
    name: &str,
    items: Vec<(&'static str, gphotos::testing::Item)>,
) -> (Shared, TempFrame, String) {
    let google = gphotos::testing::google(items, 0).await;
    let frame = TempFrame::new(&format!("route-{name}"));
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    let mut app = Arc::try_unwrap(App::new(cfg)).ok().expect("a new app has one owner");
    app.rpc_url = format!("{google}/rpc");
    (Arc::new(app), frame, google)
}

async fn until_sync_ends(app: &Shared) -> Value {
    for _ in 0..200 {
        let job = get(app, "/api/state").await.json()["job"].clone();
        if job["running"] == false && job["result"] != Value::Null {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the sync did not finish");
}

#[tokio::test]
async fn a_sync_started_from_the_page_runs_to_the_end_and_is_recorded() {
    use gphotos::testing::Item;
    let (app, _frame, google) =
        app_with_google("sync-run", vec![("K1", Item::Photo), ("K2", Item::Video), ("K3", Item::Photo)]).await;
    let album = types::AlbumLink::local(&format!("{google}/a/album"));
    let options = gphotos::Options { albums: vec![album], delete: false, refresh: false, limit: None };
    assert!(start_sync(&app, options));
    let job = until_sync_ends(&app).await;
    assert_eq!(job["result"], json!({ "ok": true, "error": null }));
    assert_eq!(job["progress"]["detail"], "downloading to the frame");
    assert!(job["log"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains("downloaded 2, removed 0, failed 0")));

    let state = get(&app, "/api/state?full=1").await.json();
    let photos = state["photos"].as_array().unwrap();
    assert_eq!(photos.len(), 2);
    assert!(
        photos.iter().all(|photo| photo[1] == "synced" && photo[2].as_u64().unwrap() < photo[3].as_u64().unwrap()),
        "dated from the album"
    );

    // The outcome is saved, which is what the status page and the daily sync go by.
    for _ in 0..200 {
        if store::load_settings(&app.cfg).last_sync.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let sync = get(&app, "/api/status").await.json()["sync"].clone();
    assert_eq!((sync["ok"].as_bool(), sync["running"].as_bool()), (Some(true), Some(false)));
    assert!(sync["summary"].as_str().unwrap().starts_with("done in"));
    assert!(now_secs().abs_diff(sync["last"].as_u64().unwrap()) < 60);
    assert!(
        start_sync(&app, gphotos::Options { albums: vec![], delete: false, refresh: false, limit: None }),
        "a finished job can start again"
    );
}

#[tokio::test]
async fn a_sync_requested_over_http_saves_the_albums_and_starts() {
    let (app, _frame) = app("sync-request");
    // Port 1 refuses connections, so the sync fails at once; what matters here is that it was started.
    let reply = post(&app, "/api/sync", r#"{"sources":["https://127.0.0.1:1/album"],"delete":true,"limit":3}"#).await;
    assert_eq!((reply.status, reply.json()["started"].as_bool()), (StatusCode::OK, Some(true)));
    let job = until_sync_ends(&app).await;
    assert!(job["result"]["error"].as_str().unwrap().starts_with("could not open the album link"));
    assert_eq!(get(&app, "/api/state?full=1").await.json()["sources"], json!(["https://127.0.0.1:1/album"]));
}

#[test]
fn the_daily_sync_runs_only_when_wanted_due_and_possible() {
    let album = || vec![types::AlbumLink::parse("https://photos.app.goo.gl/abc").unwrap()];
    let last = |at| Some(LastSync { at, succeeded: true, summary: None });
    let now = 1_700_000_000;
    let wanted = Settings { albums: album(), daily_sync: true, last_sync: last(now - DAY), ..Settings::default() };

    let options = daily_sync_due(wanted.clone(), now).expect("a day has passed");
    assert_eq!((options.albums, options.delete, options.refresh, options.limit), (album(), false, true, None));
    assert!(
        daily_sync_due(Settings { last_sync: None, ..wanted.clone() }, now).is_some(),
        "never synced counts as due"
    );

    assert!(
        daily_sync_due(Settings { last_sync: last(now - DAY + 1), ..wanted.clone() }, now).is_none(),
        "not yet a day"
    );
    assert!(daily_sync_due(Settings { daily_sync: false, ..wanted.clone() }, now).is_none(), "not asked for");
    assert!(daily_sync_due(Settings { albums: vec![], ..wanted.clone() }, now).is_none(), "nothing to sync");
    assert!(
        daily_sync_due(Settings { last_sync: last(now + 500), ..wanted }, now).is_none(),
        "a clock that went backwards is not due"
    );
}

// --- looking at faces in the background ---------------------------------------------------------------

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

// --- duplicates and status --------------------------------------------------------------------

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
