//! Looking at the people in a photo: how many faces, whether their eyes are
//! open, and whether they are smiling. Used to suggest the best of several
//! shots of the same moment.
//!
//! Three small neural networks run on the frame itself through `tract`, a
//! pure-Rust runtime: `YuNet` finds faces and five landmarks, a tiny classifier
//! judges each eye, and FER+ scores the expression. They are loaded from model
//! files next to the server's data; without those files, photos are simply not
//! analysed for people.
//!
//! Looking at a photo takes a few seconds on the frame. Photos on someone's screen are
//! looked at first; the rest of the photos that have look-alikes follow slowly in the background.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::DynamicImage;
use tract_onnx::prelude::*;

use crate::store::Config;
use crate::types::PhotoPath;

/// The detector's fixed input size.
const DETECT_SIDE: u32 = 640;
const MIN_CONFIDENCE: f32 = 0.6;
/// Two detections overlapping more than this are the same face.
const MAX_OVERLAP: f32 = 0.3;
/// Faces narrower than this (in photo pixels) are too small to judge eyes on.
const MIN_FACE_WIDTH: f32 = 40.0;
const MAX_FACES: usize = 8;
const EYE_SIDE: u32 = 32;
const EXPRESSION_SIDE: u32 = 64;
/// The eye crop is this fraction of the distance between the eyes.
const EYE_CROP: f32 = 0.55;
/// Index of "happiness" among FER+'s eight emotions.
const HAPPINESS: usize = 1;
pub const MODEL_FILES: [&str; 3] = ["yunet.onnx", "eye.onnx", "ferplus.onnx"];

type Model = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

/// The people in one photo, summed over its faces so photos can be compared.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct People {
    pub faces: u8,
    /// Sum over faces of how open the eyes are, each 0 to 100.
    pub eyes_open: u16,
    /// Sum over faces of how much each is smiling, each 0 to 100.
    pub smiling: u16,
}

impl People {
    /// How good the expressions are, from 0 to 1, for a photo that should show `expected_faces` people.
    /// A face that turned away and was not found counts as a poor one.
    pub fn score(self, expected_faces: u8) -> f64 {
        if expected_faces == 0 {
            return 0.0;
        }
        let per_face = EYES_WEIGHT * f64::from(self.eyes_open) + (1.0 - EYES_WEIGHT) * f64::from(self.smiling);
        (per_face / 100.0 / f64::from(expected_faces)).min(1.0)
    }
}

/// Open eyes matter more than a smile when choosing between shots.
const EYES_WEIGHT: f64 = 0.6;

pub struct Models {
    detector: Model,
    eye: Model,
    expression: Model,
}

/// The models, loaded when first needed and dropped again after a while unused,
/// because they hold tens of megabytes of the frame's memory.
#[derive(Default)]
pub struct OnDemand {
    loaded: Mutex<Option<(Arc<Models>, Instant)>>,
}

impl OnDemand {
    /// The models, loading them if necessary. Blocking; the first call takes a second or two.
    pub fn get(&self, cfg: &Config) -> TractResult<Arc<Models>> {
        let mut loaded = self.loaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let models = match loaded.take() {
            Some((models, _)) => models,
            None => Arc::new(Models::load(cfg)?),
        };
        *loaded = Some((Arc::clone(&models), Instant::now()));
        Ok(models)
    }

    /// Drop the models if nobody has asked for them for `idle`. Returns whether they were dropped.
    pub fn unload_if_idle(&self, idle: Duration) -> bool {
        let mut loaded = self.loaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let stale = loaded.as_ref().is_some_and(|(_, used)| used.elapsed() >= idle);
        if stale {
            *loaded = None;
        }
        stale
    }
}

/// Which photos to look at next: the ones on someone's screen first, then, slowly,
/// the rest of the photos that have look-alikes.
pub struct Queue<T = PhotoPath> {
    /// Photos the page says are on screen, most important first, and when it said so.
    watched: Option<(Vec<T>, Instant)>,
    /// Everything else worth looking at.
    background: VecDeque<T>,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Queue { watched: None, background: VecDeque::new() }
    }
}

/// What `Queue::next` hands out.
#[derive(Debug, PartialEq)]
pub enum Next<T = PhotoPath> {
    /// Someone is looking at this photo right now.
    Watched(T),
    /// Nobody is waiting for this one.
    Background(T),
}

impl<T: Clone> Queue<T> {
    /// How long a report of what is on screen stays relevant without being repeated.
    pub const WATCH_TIMEOUT: Duration = Duration::from_secs(30);

    /// Record what is on screen now.
    pub fn watch(&mut self, photos: Vec<T>) {
        self.watched = Some((photos, Instant::now()));
    }

    /// Replace the background work.
    pub fn refill(&mut self, photos: impl IntoIterator<Item = T>) {
        self.background = photos.into_iter().collect();
    }

    pub fn background_left(&self) -> usize {
        self.background.len()
    }

    /// The next photo to look at, skipping any that `done` says were looked at already.
    pub fn next(&mut self, done: impl Fn(&T) -> bool) -> Option<Next<T>> {
        if let Some((photos, at)) = &self.watched {
            if at.elapsed() < Self::WATCH_TIMEOUT {
                if let Some(photo) = photos.iter().find(|photo| !done(photo)) {
                    return Some(Next::Watched(photo.clone()));
                }
            } else {
                self.watched = None;
            }
        }
        while let Some(photo) = self.background.pop_front() {
            if !done(&photo) {
                return Some(Next::Background(photo));
            }
        }
        None
    }
}

fn load(path: &Path, shape: &[usize]) -> TractResult<Model> {
    tract_onnx::onnx()
        .model_for_path(path)?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?
        .into_runnable()
}

pub fn model_dir(cfg: &Config) -> std::path::PathBuf {
    cfg.data_dir.join("models")
}

/// Whether the model files are installed.
pub fn available(cfg: &Config) -> bool {
    MODEL_FILES.iter().all(|name| model_dir(cfg).join(name).is_file())
}

impl Models {
    /// Load the three models. Takes a second or two and tens of megabytes; drop it when done.
    pub fn load(cfg: &Config) -> TractResult<Models> {
        let dir = model_dir(cfg);
        let side = |s: u32| s as usize;
        Ok(Models {
            detector: load(&dir.join(MODEL_FILES[0]), &[1, 3, side(DETECT_SIDE), side(DETECT_SIDE)])?,
            eye: load(&dir.join(MODEL_FILES[1]), &[1, 3, side(EYE_SIDE), side(EYE_SIDE)])?,
            expression: load(&dir.join(MODEL_FILES[2]), &[1, 1, side(EXPRESSION_SIDE), side(EXPRESSION_SIDE)])?,
        })
    }

    /// Find the faces in a photo and judge each one.
    pub fn analyse(&self, img: &DynamicImage) -> TractResult<People> {
        let mut people = People::default();
        for face in self.detect(img)? {
            let eyes = f32::midpoint(self.eye_open(img, &face, 0)?, self.eye_open(img, &face, 1)?);
            let smile = self.smile(img, &face)?;
            people.faces += 1;
            people.eyes_open += percent(eyes);
            people.smiling += percent(smile);
        }
        Ok(people)
    }

    fn detect(&self, img: &DynamicImage) -> TractResult<Vec<Detection>> {
        // Fit the photo into the top-left of a square; the rest stays black.
        let small = img.resize(DETECT_SIDE, DETECT_SIDE, FilterType::Triangle).to_rgb8();
        let scale = float(DETECT_SIDE) / float(img.width().max(img.height()));
        let side = DETECT_SIDE as usize;
        let input: Tensor = tract_ndarray::Array4::from_shape_fn((1, 3, side, side), |(_, channel, y, x)| {
            // The detector wants blue, green, red.
            small.get_pixel_checked(index(x), index(y)).map_or(0.0, |pixel| f32::from(pixel[2 - channel]))
        })
        .into();
        let outputs = self.detector.run(tvec!(input.into()))?;
        let mut found = Vec::new();
        for (level, stride) in [8usize, 16, 32].into_iter().enumerate() {
            let flat = |at: usize| -> TractResult<Vec<f32>> {
                Ok(outputs[at].to_array_view::<f32>()?.iter().copied().collect())
            };
            let grid = Grid {
                stride,
                columns: side / stride,
                class: &flat(level)?,
                object: &flat(3 + level)?,
                boxes: &flat(6 + level)?,
                points: &flat(9 + level)?,
            };
            found.extend(grid.detections(scale));
        }
        let mut faces = suppress(found);
        faces.retain(|face| face.width >= MIN_FACE_WIDTH);
        faces.truncate(MAX_FACES);
        Ok(faces)
    }

    /// Probability that one eye (0 = first landmark, 1 = second) is open.
    fn eye_open(&self, img: &DynamicImage, face: &Detection, which: usize) -> TractResult<f32> {
        let between = (face.points[0].0 - face.points[1].0).hypot(face.points[0].1 - face.points[1].1);
        let (x, y) = face.points[which];
        let eye = crop(img, x, y, between * EYE_CROP, EYE_SIDE).to_rgb8();
        let side = EYE_SIDE as usize;
        let input: Tensor = tract_ndarray::Array4::from_shape_fn((1, 3, side, side), |(_, channel, y, x)| {
            (f32::from(eye.get_pixel(index(x), index(y))[channel]) - 127.0) / 255.0
        })
        .into();
        let output = self.eye.run(tvec!(input.into()))?;
        let scores: Vec<f32> = output[0].to_array_view::<f32>()?.iter().copied().collect();
        Ok(probabilities(&scores).get(1).copied().unwrap_or(0.0)) // [closed, open]
    }

    /// Probability that the face is smiling.
    fn smile(&self, img: &DynamicImage, face: &Detection) -> TractResult<f32> {
        let around = face.width.max(face.height) * 1.1;
        let (centre_x, centre_y) = (face.x + face.width / 2.0, face.y + face.height / 2.0);
        let grey = crop(img, centre_x, centre_y, around, EXPRESSION_SIDE).to_luma8();
        let side = EXPRESSION_SIDE as usize;
        let input: Tensor = tract_ndarray::Array4::from_shape_fn((1, 1, side, side), |(_, _, y, x)| {
            f32::from(grey.get_pixel(index(x), index(y))[0])
        })
        .into();
        let output = self.expression.run(tvec!(input.into()))?;
        let scores: Vec<f32> = output[0].to_array_view::<f32>()?.iter().copied().collect();
        Ok(probabilities(&scores).get(HAPPINESS).copied().unwrap_or(0.0))
    }
}

/// A tensor index as an image coordinate.
fn index(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// An image dimension as a float.
fn float(pixels: u32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "image dimensions are far below 2^23")]
    let value = pixels as f32;
    value
}

/// A probability as a whole percentage.
fn percent(probability: f32) -> u16 {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=100 first")]
    let value = (probability * 100.0).round().clamp(0.0, 100.0) as u16;
    value
}

/// Turn raw scores into probabilities, unless they already are.
fn probabilities(scores: &[f32]) -> Vec<f32> {
    let sum: f32 = scores.iter().sum();
    if scores.iter().all(|s| (0.0..=1.0).contains(s)) && (sum - 1.0).abs() < 0.01 {
        return scores.to_vec();
    }
    let max = scores.iter().copied().fold(f32::MIN, f32::max);
    let exps: Vec<f32> = scores.iter().map(|s| (s - max).exp()).collect();
    let total: f32 = exps.iter().sum();
    exps.iter().map(|e| e / total).collect()
}

/// A square region of the photo centred on a point, resized to `out` pixels.
fn crop(img: &DynamicImage, centre_x: f32, centre_y: f32, side: f32, out: u32) -> DynamicImage {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to be non-negative first")]
    let pixels = |value: f32| value.max(0.0) as u32;
    let left = pixels(centre_x - side / 2.0).min(img.width().saturating_sub(1));
    let top = pixels(centre_y - side / 2.0).min(img.height().saturating_sub(1));
    let extent = pixels(side).max(2).min(img.width() - left).min(img.height() - top).max(1);
    img.crop_imm(left, top, extent, extent).resize_exact(out, out, FilterType::Triangle)
}

/// One face the detector found, in photo pixels.
#[derive(Clone, Debug, PartialEq)]
struct Detection {
    confidence: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    /// Right eye, left eye, nose, and the two mouth corners.
    points: [(f32, f32); 5],
}

/// The detector's raw output for one grid of cells.
struct Grid<'a> {
    stride: usize,
    columns: usize,
    class: &'a [f32],
    object: &'a [f32],
    /// Four numbers per cell: centre offset x, y and log width, height, in cells.
    boxes: &'a [f32],
    /// Ten numbers per cell: five landmark offsets, in cells.
    points: &'a [f32],
}

impl Grid<'_> {
    /// The confident detections in this grid, scaled back to photo pixels.
    fn detections(&self, scale: f32) -> Vec<Detection> {
        #[expect(clippy::cast_precision_loss, reason = "grid coordinates are tiny")]
        let (stride, cell) = (self.stride as f32, |n: usize| ((n % self.columns) as f32, (n / self.columns) as f32));
        let mut found = Vec::new();
        for n in 0..self.class.len().min(self.object.len()) {
            let confidence = (self.class[n].clamp(0.0, 1.0) * self.object[n].clamp(0.0, 1.0)).sqrt();
            let (Some(shape), Some(points)) = (self.boxes.get(n * 4..n * 4 + 4), self.points.get(n * 10..n * 10 + 10))
            else {
                break;
            };
            if confidence < MIN_CONFIDENCE {
                continue;
            }
            let (column, row) = cell(n);
            let (centre_x, centre_y) = ((column + shape[0]) * stride, (row + shape[1]) * stride);
            let (width, height) = (shape[2].exp() * stride, shape[3].exp() * stride);
            let mut landmarks = [(0.0, 0.0); 5];
            for (i, landmark) in landmarks.iter_mut().enumerate() {
                *landmark = ((column + points[2 * i]) * stride / scale, (row + points[2 * i + 1]) * stride / scale);
            }
            found.push(Detection {
                confidence,
                x: (centre_x - width / 2.0) / scale,
                y: (centre_y - height / 2.0) / scale,
                width: width / scale,
                height: height / scale,
                points: landmarks,
            });
        }
        found
    }
}

/// Keep the most confident detection of each face, dropping overlapping ones.
fn suppress(mut found: Vec<Detection>) -> Vec<Detection> {
    found.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut kept: Vec<Detection> = Vec::new();
    for face in found {
        if kept.iter().all(|other| overlap(&face, other) <= MAX_OVERLAP) {
            kept.push(face);
        }
    }
    kept
}

/// Shared area of two boxes as a fraction of their combined area.
fn overlap(a: &Detection, b: &Detection) -> f32 {
    let width = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let height = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    if width <= 0.0 || height <= 0.0 {
        return 0.0;
    }
    let shared = width * height;
    shared / (a.width * a.height + b.width * b.height - shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(confidence: f32, x: f32, y: f32, size: f32) -> Detection {
        Detection { confidence, x, y, width: size, height: size, points: [(0.0, 0.0); 5] }
    }

    #[test]
    fn overlap_is_shared_area_over_combined_area() {
        let a = face(0.9, 0.0, 0.0, 10.0);
        assert!((overlap(&a, &a) - 1.0).abs() < 1e-6);
        assert!((overlap(&a, &face(0.9, 5.0, 0.0, 10.0)) - 50.0 / 150.0).abs() < 1e-6);
        assert!(overlap(&a, &face(0.9, 10.0, 0.0, 10.0)).abs() < 1e-6, "touching is not overlapping");
        assert!(overlap(&a, &face(0.9, 50.0, 50.0, 10.0)).abs() < 1e-6);
    }

    #[test]
    fn overlapping_detections_collapse_to_the_most_confident() {
        let kept = suppress(vec![
            face(0.7, 1.0, 1.0, 10.0),
            face(0.95, 0.0, 0.0, 10.0),
            face(0.8, 100.0, 0.0, 10.0),
            face(0.65, 101.0, 1.0, 10.0),
        ]);
        let summary: Vec<(f32, f32)> = kept.iter().map(|f| (f.confidence, f.x)).collect();
        assert_eq!(summary, vec![(0.95, 0.0), (0.8, 100.0)]);
        assert!(suppress(Vec::new()).is_empty());
    }

    #[test]
    fn a_grid_cell_decodes_to_a_box_and_landmarks_in_photo_pixels() {
        // A 2x2 grid at stride 8; only the bottom-right cell (column 1, row 1) is confident.
        let class = [0.1, 0.1, 0.1, 0.81];
        let object = [0.9, 0.9, 0.9, 1.0];
        let mut boxes = [0.0; 16];
        boxes[12..16].copy_from_slice(&[0.5, 0.25, 0.0, 2f32.ln()]); // centre offset, then log size
        let mut points = [0.0; 40];
        points[30..34].copy_from_slice(&[0.0, 0.0, 1.0, -0.5]);
        let grid = Grid { stride: 8, columns: 2, class: &class, object: &object, boxes: &boxes, points: &points };

        let found = grid.detections(0.5); // the photo was halved to fit the detector
        assert_eq!(found.len(), 1);
        let face = &found[0];
        assert!((face.confidence - 0.9).abs() < 1e-6, "geometric mean of the two scores");
        // Centre (1.5, 1.25) cells = (12, 10) px in detector space; 8 x 16 px box; doubled for the photo.
        assert!((face.width - 16.0).abs() < 1e-4 && (face.height - 32.0).abs() < 1e-4, "{face:?}");
        assert!((face.x - 16.0).abs() < 1e-4 && (face.y - 4.0).abs() < 1e-4, "{face:?}");
        assert_eq!(face.points[0], (16.0, 16.0));
        assert_eq!(face.points[1], (32.0, 8.0));
    }

    #[test]
    fn a_grid_with_truncated_output_does_not_panic() {
        let grid =
            Grid { stride: 8, columns: 2, class: &[0.9; 4], object: &[0.9; 4], boxes: &[0.0; 6], points: &[0.0; 40] };
        assert_eq!(grid.detections(1.0).len(), 1, "only the first cell has a complete box");
    }

    #[test]
    fn scores_become_probabilities() {
        assert_eq!(probabilities(&[0.25, 0.75]), vec![0.25, 0.75], "already probabilities");
        let p = probabilities(&[0.0, 0.0]);
        assert!((p[0] - 0.5).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6);
        let p = probabilities(&[10.0, 0.0, -5.0]);
        assert!(p[0] > 0.99 && (p.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(percent(0.994), 99);
        assert_eq!(percent(1.7), 100);
        assert_eq!(percent(-0.2), 0);
    }

    #[test]
    fn crops_stay_inside_the_photo() {
        let img = DynamicImage::new_rgb8(100, 80);
        for (x, y, side) in [
            (50.0, 40.0, 20.0),
            (0.0, 0.0, 30.0),
            (99.0, 79.0, 40.0),
            (500.0, 500.0, 10.0),
            (50.0, 40.0, 0.0),
            (-20.0, -20.0, 10.0),
        ] {
            let out = crop(&img, x, y, side, 32);
            assert_eq!((out.width(), out.height()), (32, 32), "{x},{y} side {side}");
        }
    }

    #[test]
    fn expression_score_rewards_open_eyes_and_smiles_and_penalises_missing_faces() {
        let both_great = People { faces: 2, eyes_open: 200, smiling: 200 };
        let one_blinked = People { faces: 2, eyes_open: 100, smiling: 200 };
        let one_missing = People { faces: 1, eyes_open: 100, smiling: 100 };
        let serious = People { faces: 2, eyes_open: 200, smiling: 0 };
        assert!((both_great.score(2) - 1.0).abs() < 1e-9);
        assert!((one_blinked.score(2) - 0.7).abs() < 1e-9);
        assert!((one_missing.score(2) - 0.5).abs() < 1e-9, "a face that was not found counts against the photo");
        assert!((serious.score(2) - 0.6).abs() < 1e-9);
        assert!(serious.score(2) > one_missing.score(2) && one_blinked.score(2) > serious.score(2));
        assert!(People::default().score(0).abs() < f64::EPSILON, "no people expected, nothing to score");
        assert!((both_great.score(1) - 1.0).abs() < 1e-9, "never above 1");
    }

    #[test]
    fn models_are_reported_missing_until_all_three_files_exist() {
        let frame = crate::store::testing::TempFrame::new("faces-models");
        assert!(!available(&frame.0));
        std::fs::create_dir_all(model_dir(&frame.0)).unwrap();
        for name in &MODEL_FILES[..2] {
            std::fs::write(model_dir(&frame.0).join(name), b"x").unwrap();
        }
        assert!(!available(&frame.0));
        std::fs::write(model_dir(&frame.0).join(MODEL_FILES[2]), b"x").unwrap();
        assert!(available(&frame.0));
        assert!(Models::load(&frame.0).is_err(), "junk files are rejected, not trusted");
    }

    fn rels(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn photos_on_screen_come_before_background_work() {
        let mut queue = Queue::<String>::default();
        queue.refill(rels(&["bg1", "bg2"]));
        queue.watch(rels(&["seen1", "seen2"]));
        let nothing_done = |_: &String| false;
        assert_eq!(queue.next(nothing_done), Some(Next::Watched("seen1".into())));
        assert_eq!(queue.next(nothing_done), Some(Next::Watched("seen1".into())), "still first until it is done");
        assert_eq!(queue.next(|rel| rel == "seen1"), Some(Next::Watched("seen2".into())));
        assert_eq!(queue.background_left(), 2, "background work waits while someone is watching");
    }

    #[test]
    fn background_work_resumes_when_everything_on_screen_is_done() {
        let mut queue = Queue::<String>::default();
        queue.refill(rels(&["bg1", "seen1", "bg2"]));
        queue.watch(rels(&["seen1"]));
        let done = |rel: &String| rel == "seen1";
        assert_eq!(queue.next(done), Some(Next::Background("bg1".into())));
        assert_eq!(queue.next(done), Some(Next::Background("bg2".into())), "photos already looked at are skipped");
        assert_eq!(queue.next(done), None);
        assert_eq!(queue.background_left(), 0);
    }

    #[test]
    fn a_new_report_replaces_what_was_on_screen() {
        let mut queue = Queue::<String>::default();
        queue.watch(rels(&["old"]));
        queue.watch(rels(&["new"]));
        assert_eq!(queue.next(|_| false), Some(Next::Watched("new".into())));
        queue.watch(Vec::new());
        assert_eq!(queue.next(|_| false), None);
    }

    #[test]
    fn a_stale_report_of_what_is_on_screen_is_ignored() {
        let mut queue = Queue::<String>::default();
        queue.refill(rels(&["bg"]));
        let long_ago = Instant::now().checked_sub(Queue::<String>::WATCH_TIMEOUT).unwrap();
        queue.watched = Some((rels(&["seen"]), long_ago));
        assert_eq!(queue.next(|_| false), Some(Next::Background("bg".into())), "the page went away");
    }

    #[test]
    fn models_are_loaded_once_and_dropped_when_idle() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
        if !MODEL_FILES.iter().all(|name| dir.join(name).is_file()) {
            eprintln!("skipping: model files not downloaded");
            return;
        }
        let cfg = Config { cloud_dir: dir, data_dir: Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf() };
        let holder = OnDemand::default();
        assert!(!holder.unload_if_idle(Duration::ZERO), "nothing loaded yet");
        let first = holder.get(&cfg).unwrap();
        assert!(Arc::ptr_eq(&first, &holder.get(&cfg).unwrap()), "the second request reuses the loaded models");
        assert!(!holder.unload_if_idle(Duration::from_secs(3600)), "recently used: kept");
        assert!(holder.unload_if_idle(Duration::ZERO));
        assert!(!Arc::ptr_eq(&first, &holder.get(&cfg).unwrap()), "loaded afresh after being dropped");
    }

    #[test]
    fn loading_fails_cleanly_without_model_files() {
        let frame = crate::store::testing::TempFrame::new("faces-on-demand");
        assert!(OnDemand::default().get(&frame.0).is_err());
    }

    /// A way to look at what the models say about real photos:
    /// `FRAME_FACE_SAMPLES=/some/folder cargo test prints_what -- --nocapture`.
    #[test]
    fn prints_what_the_models_see_in_sample_photos() {
        let (Ok(samples), dir) =
            (std::env::var("FRAME_FACE_SAMPLES"), Path::new(env!("CARGO_MANIFEST_DIR")).join("models"))
        else {
            return;
        };
        let cfg = Config { cloud_dir: dir.clone(), data_dir: Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf() };
        let models = Models::load(&cfg).unwrap();
        let mut paths: Vec<_> = std::fs::read_dir(samples).unwrap().flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths.iter().filter(|p| p.extension().is_some_and(|e| e == "jpg")) {
            let people = models.analyse(&image::open(path).unwrap()).unwrap();
            println!(
                "{}: {people:?} score {:.2}",
                path.file_name().unwrap().to_string_lossy(),
                people.score(people.faces)
            );
        }
    }

    /// Runs the real models when they are present in `models/` beside the crate (see deploy.py).
    #[test]
    fn real_models_find_no_people_in_an_empty_picture() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
        if !MODEL_FILES.iter().all(|name| dir.join(name).is_file()) {
            eprintln!("skipping: model files not downloaded");
            return;
        }
        let cfg = Config { cloud_dir: dir.clone(), data_dir: Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf() };
        let models = Models::load(&cfg).unwrap();
        let blank = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(800, 600, image::Rgb([120, 130, 140])));
        assert_eq!(models.analyse(&blank).unwrap(), People::default());
    }
}
