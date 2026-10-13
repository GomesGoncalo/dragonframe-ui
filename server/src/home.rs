//! What the rest of the house is doing: the doorbell, cameras, alarm and door sensors.
//!
//! The frame does not talk to those devices and holds no password or token for them:
//! anything kept on the frame can be read by whoever is on the same network. A relay
//! on another machine (see `homeassistant/` in this project) watches Home Assistant
//! and tells the frame what to show, proving itself with a key that opens nothing
//! but this. What it says is kept in memory only.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use axum::body::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::types::{AlarmState, Bounded, CameraId, Label, SensorKind};

/// The relay reports every half minute; after this long without a word the house counts as out of touch.
const OUT_OF_TOUCH_AFTER: Duration = Duration::from_secs(90);
const MAX_PICTURE_BYTES: usize = 3 * 1024 * 1024;

/// A moment on the clock that waiting is measured by.
type Moment = tokio::time::Instant;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub name: Label,
    pub state: AlarmState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sensor {
    pub name: Label,
    pub kind: SensorKind,
    /// Open, for a door or window; something moving, for a motion sensor.
    pub on: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub id: CameraId,
    pub name: Label,
    /// It sees a person right now.
    #[serde(default)]
    pub person: bool,
    /// It is the doorbell's camera, whose picture goes with a ring.
    #[serde(default)]
    pub bell: bool,
}

/// What the relay says the house is doing. The lists have limits, so the frame cannot be
/// made to hold more than a house's worth.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub alarms: Bounded<Alarm, 0, 4>,
    #[serde(default)]
    pub sensors: Bounded<Sensor, 0, 40>,
    #[serde(default)]
    pub cameras: Bounded<Camera, 0, 8>,
    /// Someone is ringing the doorbell.
    #[serde(default)]
    pub ringing: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PictureError {
    #[error("the picture is larger than 3 MB")]
    TooLarge,
    #[error("that is not a JPEG or PNG picture")]
    NotAPicture,
    #[error("the house has no camera by that name")]
    UnknownCamera,
}

struct Picture {
    bytes: Bytes,
    /// When it arrived, seconds since 1970.
    at: u64,
}

#[derive(Default)]
struct Told {
    report: Report,
    heard: Option<Moment>,
    /// When the doorbell last started ringing, seconds since 1970.
    rang: Option<u64>,
    pictures: HashMap<CameraId, Picture>,
}

#[derive(Default)]
pub struct Home {
    told: Mutex<Told>,
}

impl Home {
    fn told(&self) -> std::sync::MutexGuard<'_, Told> {
        self.told.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take in what the relay says. Returns whether the doorbell has just started ringing.
    pub fn hear(&self, report: Report, now_secs: u64) -> bool {
        let mut told = self.told();
        let rings = report.ringing && !told.report.ringing;
        if rings {
            told.rang = Some(now_secs);
        }
        // A camera that is gone takes its last picture with it.
        told.pictures.retain(|id, _| report.cameras.iter().any(|camera| camera.id == *id));
        told.report = report;
        told.heard = Some(Moment::now());
        rings
    }

    /// Keep a camera's latest picture.
    pub fn keep_picture(&self, camera: CameraId, bytes: Bytes, now_secs: u64) -> Result<(), PictureError> {
        if bytes.len() > MAX_PICTURE_BYTES {
            return Err(PictureError::TooLarge);
        }
        if content_type(&bytes).is_none() {
            return Err(PictureError::NotAPicture);
        }
        let mut told = self.told();
        if !told.report.cameras.iter().any(|known| known.id == camera) {
            return Err(PictureError::UnknownCamera);
        }
        told.pictures.insert(camera, Picture { bytes, at: now_secs });
        Ok(())
    }

    /// The cameras the frame has no picture of (after a restart, all of them), so that
    /// whoever reports can send them again.
    pub fn without_a_picture(&self) -> Vec<CameraId> {
        let told = self.told();
        told.report
            .cameras
            .iter()
            .map(|camera| &camera.id)
            .filter(|id| !told.pictures.contains_key(*id))
            .cloned()
            .collect()
    }

    /// A camera's latest picture and what kind of picture it is.
    pub fn picture(&self, camera: &CameraId) -> Option<(Bytes, &'static str)> {
        let bytes = self.told().pictures.get(camera)?.bytes.clone();
        content_type(&bytes).map(|kind| (bytes, kind))
    }

    /// Forget everything that was said, as when the house is disconnected.
    pub fn forget(&self) {
        *self.told() = Told::default();
    }

    /// The house for the page. `connected` is false once the relay has gone quiet, and what
    /// was last said is then old news, to be shown as such.
    pub fn view(&self) -> Value {
        let told = self.told();
        let cameras: Vec<Value> = told
            .report
            .cameras
            .iter()
            .map(|camera| {
                json!({
                    "id": camera.id,
                    "name": camera.name,
                    "person": camera.person,
                    "bell": camera.bell,
                    // When its picture arrived, which also tells the page when to fetch it again.
                    "picture": told.pictures.get(&camera.id).map(|picture| picture.at),
                })
            })
            .collect();
        json!({
            "connected": told.heard.is_some_and(|heard| heard.elapsed() < OUT_OF_TOUCH_AFTER),
            "alarms": &*told.report.alarms,
            "sensors": &*told.report.sensors,
            "cameras": cameras,
            "ringing": told.report.ringing,
            "rang": told.rang,
        })
    }
}

/// What kind of picture these bytes are, going by how they start.
fn content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(value: Value) -> Report {
        serde_json::from_value(value).unwrap()
    }

    fn camera(id: &str) -> CameraId {
        CameraId::parse(id).unwrap()
    }

    const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 1, 2, 3];

    fn house() -> Value {
        json!({
            "alarms": [{ "name": "Homebase", "state": "armed_away" }],
            "sensors": [
                { "name": "Front Door", "kind": "door", "on": false },
                { "name": "Kitchen", "kind": "motion", "on": true },
            ],
            "cameras": [
                { "id": "doorbell", "name": "Doorbell", "bell": true },
                { "id": "side_passage", "name": "Side Passage", "person": true },
            ],
        })
    }

    #[tokio::test(start_paused = true)]
    async fn the_house_is_shown_as_last_told_and_as_out_of_touch_once_the_relay_goes_quiet() {
        let home = Home::default();
        assert_eq!(home.view()["connected"], false);
        assert!(!home.hear(report(house()), 1_800_000_000));
        let view = home.view();
        assert_eq!(view["connected"], true);
        assert_eq!(view["alarms"], json!([{ "name": "Homebase", "state": "armed_away" }]));
        assert_eq!(view["sensors"][1], json!({ "name": "Kitchen", "kind": "motion", "on": true }));
        assert_eq!(
            view["cameras"][1],
            json!({ "id": "side_passage", "name": "Side Passage", "person": true, "bell": false, "picture": null })
        );
        assert_eq!((&view["ringing"], &view["rang"]), (&json!(false), &Value::Null));

        tokio::time::advance(OUT_OF_TOUCH_AFTER).await;
        let view = home.view();
        assert_eq!(view["connected"], false);
        assert_eq!(view["alarms"][0]["state"], "armed_away", "what was last said is still there");
        home.forget();
        assert_eq!(home.view()["alarms"], json!([]));
    }

    #[tokio::test]
    async fn a_ring_is_noticed_once_when_it_starts() {
        let home = Home::default();
        let ringing = |on: bool| {
            let mut house = house();
            house["ringing"] = json!(on);
            report(house)
        };
        assert!(!home.hear(ringing(false), 100));
        assert!(home.hear(ringing(true), 200), "it has just started");
        assert!(!home.hear(ringing(true), 205), "still the same ring");
        assert_eq!(home.view()["rang"], 200);
        assert!(!home.hear(ringing(false), 230));
        assert_eq!(home.view()["rang"], 200, "when it last rang is remembered");
        assert!(home.hear(ringing(true), 900));
        assert_eq!(home.view()["rang"], 900);
    }

    #[tokio::test]
    async fn a_cameras_picture_is_kept_and_goes_when_the_camera_does() {
        let home = Home::default();
        assert_eq!(
            home.keep_picture(camera("doorbell"), Bytes::from_static(JPEG), 1),
            Err(PictureError::UnknownCamera)
        );
        home.hear(report(house()), 100);
        assert_eq!(home.without_a_picture(), [camera("doorbell"), camera("side_passage")]);
        assert_eq!(home.keep_picture(camera("doorbell"), Bytes::from_static(JPEG), 150), Ok(()));
        assert_eq!(home.without_a_picture(), [camera("side_passage")]);
        assert_eq!(home.view()["cameras"][0]["picture"], 150);
        assert_eq!(home.picture(&camera("doorbell")), Some((Bytes::from_static(JPEG), "image/jpeg")));
        assert_eq!(home.picture(&camera("side_passage")), None);

        let png = Bytes::from_static(b"\x89PNG\r\n\x1a\nrest");
        assert_eq!(home.keep_picture(camera("side_passage"), png, 160), Ok(()));
        assert_eq!(home.picture(&camera("side_passage")).unwrap().1, "image/png");
        let refused = home.keep_picture(camera("doorbell"), Bytes::from_static(b"<html>"), 170);
        assert_eq!(refused, Err(PictureError::NotAPicture));
        let huge = Bytes::from(vec![0xff; MAX_PICTURE_BYTES + 1]);
        assert_eq!(home.keep_picture(camera("doorbell"), huge, 170), Err(PictureError::TooLarge));
        assert_eq!(home.view()["cameras"][0]["picture"], 150, "a refused picture replaces nothing");

        home.hear(report(json!({ "cameras": [{ "id": "side_passage", "name": "Side Passage" }] })), 200);
        assert_eq!(home.picture(&camera("doorbell")), None);
        assert!(home.picture(&camera("side_passage")).is_some());
    }

    #[test]
    fn a_report_with_more_than_a_house_holds_is_refused() {
        let many: Vec<Value> = (0..9).map(|n| json!({ "id": format!("c{n}"), "name": "Cam" })).collect();
        assert!(serde_json::from_value::<Report>(json!({ "cameras": many })).is_err());
        assert!(serde_json::from_value::<Report>(json!({ "sensors": [{ "name": "", "kind": "door", "on": true }] }))
            .is_err());
        assert_eq!(report(json!({})), Report::default());
    }
}
