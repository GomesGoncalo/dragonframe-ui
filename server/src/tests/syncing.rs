//! Route tests: syncing, against a stand-in for Google Photos.

use super::*;

/// An app whose album listings come from a fake Google serving `items`.
async fn app_with_google(
    name: &str,
    items: Vec<(&'static str, gphotos::testing::Item)>,
) -> (Shared, TempFrame, String) {
    let google = gphotos::testing::google(items, 0).await;
    let frame = TempFrame::new(&format!("route-{name}"));
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    let mut app = Arc::try_unwrap(App::new(cfg, Device::simulated(), 8080)).ok().expect("a new app has one owner");
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
