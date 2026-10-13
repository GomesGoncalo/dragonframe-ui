//! Tests of the HTTP routes, driving the real router without a network socket.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HOST, ORIGIN, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

// The server's parts, by the names they have where they live.
use crate::app::*;
use crate::device::Device;
use crate::gphotos::Phase;
use crate::plain::Peer;
use crate::routes::frame::*;
use crate::routes::photos::*;
use crate::routes::*;
use crate::store::testing::{picture, TempFrame};
use crate::store::{Config, LastSync, Settings};
use crate::tasks::*;
use crate::types::*;
use crate::weather::Weather;
use crate::{calendar, device, faces, gphotos, index, night, radio, spotify, store, travel, types, weather};

const SYNCED: &str = "frame-sync/fs_0123456789abcdef.jpg";
const HOST_NAME: &str = "192.168.4.23:8080";

fn p(rel: &str) -> PhotoPath {
    PhotoPath::parse(rel).unwrap()
}

fn app(name: &str) -> (Shared, TempFrame) {
    let frame = TempFrame::new(&format!("route-{name}"));
    let cfg = Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() };
    (App::new(cfg, Device::Simulated, 8080), frame)
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

mod acting;
mod calendars;
mod changing;
mod duplicates;
mod face_checks;
mod forecast;
mod frame;
mod house;
mod protection;
mod reading;
mod syncing;
mod todos;
mod trips;

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
