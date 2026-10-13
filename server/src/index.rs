//! What we know about each photo beyond the file itself: when it was taken,
//! and a visual fingerprint for finding look-alikes. Held in memory and saved
//! to the data dir; a background task fills in whatever is missing.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::DynamicImage;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, info};

use crate::faces::{Models, People};
use crate::store::{self, Config, Photo, PhotoError};
use crate::types::{PhotoPath, Source};

/// Photos are measured at this size (longest side) so sharpness is comparable between them.
const MEASURE_SIDE: u32 = 1024;
const CLIP_BLACK: u8 = 4;
const CLIP_WHITE: u8 = 251;
/// When expressions are compared, how much being the sharpest of the group adds (expressions range 0 to 1).
const SHARPNESS_WEIGHT: f64 = 0.3;
/// Look-alikes at this distance are analysed first, since that is where a suggestion is needed.
const PRIORITY_DISTANCE: u32 = 8;
/// However much of a photo is clipped, it never loses more than this share of its score.
const MAX_CLIP_PENALTY: f64 = 0.6;

/// Photos decoded at once by the background task: enough to use spare cores, few enough to leave the slideshow smooth.
const PREPARE_IN_PARALLEL: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Meta {
    /// When the photo was taken, in seconds since 1970.
    pub taken: Option<u64>,
    /// Fingerprint from `dhash`.
    pub hash: Option<u64>,
    /// How sharp and well exposed it is, from `measure`.
    pub quality: Option<Quality>,
    /// The faces in it and their expressions, if the face models are installed.
    pub people: Option<People>,
    /// Width and height in pixels, which the frame's screen needs to lay photos out side by side.
    pub size: Option<(u32, u32)>,
}

/// Progress of the background analysis.
#[derive(Debug, PartialEq)]
pub struct Analysed {
    pub fingerprints: usize,
    pub quality: usize,
    pub people: usize,
}

/// How the photos of one group of look-alikes compare.
#[derive(Debug, PartialEq)]
pub struct Ranking {
    /// A score per photo, higher is better; None where a photo has not been measured yet.
    pub scores: Vec<Option<f64>>,
    /// Whether expressions were taken into account, or only sharpness and exposure.
    pub by_expressions: bool,
}

/// Score shots of the same scene against each other. With people in them, expressions
/// decide and sharpness breaks ties; without, sharpness and exposure decide.
pub fn rank(photos: &[Meta]) -> Ranking {
    let best_quality = photos.iter().filter_map(|m| m.quality).map(Quality::score).fold(0.0, f64::max);
    let most_faces = photos.iter().filter_map(|m| m.people).map(|p| p.faces).max().unwrap_or(0);
    let by_expressions = most_faces > 0 && photos.iter().all(|m| m.people.is_some());
    let scores = photos
        .iter()
        .map(|meta| {
            let sharpness = meta.quality?.score();
            if !by_expressions {
                return Some(sharpness);
            }
            let relative = if best_quality > 0.0 { sharpness / best_quality } else { 0.0 };
            Some(meta.people?.score(most_faces) + SHARPNESS_WEIGHT * relative)
        })
        .collect();
    Ranking { scores, by_expressions }
}

/// Technical quality of a photo. Only meaningful for comparing shots of the same scene.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    /// Amount of fine edge detail (variance of the Laplacian); higher is sharper.
    pub sharpness: u32,
    /// Share of pixels that are pure black or pure white, in thousandths.
    pub clipped: u16,
}

impl Quality {
    /// One number to rank by: sharpness, marked down for blown-out or crushed areas.
    pub fn score(self) -> f64 {
        let clipped = (f64::from(self.clipped) / 1000.0).min(MAX_CLIP_PENALTY);
        f64::from(self.sharpness) * (1.0 - clipped)
    }
}

/// Measure sharpness and exposure. The photo is first reduced to a common size
/// so that results are comparable between photos.
pub fn measure(img: &DynamicImage) -> Quality {
    let grey = if img.width().max(img.height()) > MEASURE_SIDE {
        img.thumbnail(MEASURE_SIDE, MEASURE_SIDE).to_luma8()
    } else {
        img.to_luma8()
    };
    let (width, height) = (grey.width() as usize, grey.height() as usize);
    let pixels = grey.as_raw();
    let clipped = pixels.iter().filter(|&&p| p <= CLIP_BLACK || p >= CLIP_WHITE).count();
    // Laplacian: how much each pixel differs from its four neighbours. Blur flattens it.
    let (mut sum, mut sum_of_squares, mut count) = (0f64, 0f64, 0f64);
    for y in 1..height.saturating_sub(1) {
        for x in 1..width.saturating_sub(1) {
            let at = |dx: usize, dy: usize| i32::from(pixels[(y + dy - 1) * width + x + dx - 1]);
            let laplacian = f64::from(4 * at(1, 1) - at(0, 1) - at(2, 1) - at(1, 0) - at(1, 2));
            sum += laplacian;
            sum_of_squares += laplacian * laplacian;
            count += 1.0;
        }
    }
    let variance = if count > 0.0 { sum_of_squares / count - (sum / count).powi(2) } else { 0.0 };
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the target range first")]
    Quality {
        sharpness: variance.round().clamp(0.0, f64::from(u32::MAX)) as u32,
        clipped: (clipped * 1000 / pixels.len().max(1)).min(1000) as u16,
    }
}

pub struct Index {
    path: PathBuf,
    entries: Mutex<HashMap<PhotoPath, Meta>>,
    dirty: AtomicBool,
}

impl Index {
    pub fn load(cfg: &Config) -> Index {
        let path = cfg.data_dir.join("index.json");
        let saved: Value =
            fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or(Value::Null);
        let mut entries = HashMap::new();
        for (rel, value) in saved.as_object().into_iter().flatten() {
            let Some(photo) = PhotoPath::parse(rel) else { continue };
            let quality = match (value[2].as_u64(), value[3].as_u64()) {
                (Some(sharpness), Some(clipped)) => Some(Quality {
                    sharpness: u32::try_from(sharpness).unwrap_or(u32::MAX),
                    clipped: u16::try_from(clipped).unwrap_or(1000),
                }),
                _ => None,
            };
            let small = |v: &Value| v.as_u64().and_then(|n| u16::try_from(n).ok());
            let pixels = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
            let people = match (small(&value[4][0]), small(&value[4][1]), small(&value[4][2])) {
                (Some(faces), Some(eyes_open), Some(smiling)) => {
                    Some(People { faces: u8::try_from(faces).unwrap_or(u8::MAX), eyes_open, smiling })
                }
                _ => None,
            };
            let meta = Meta {
                taken: value[0].as_u64(),
                hash: value[1].as_str().and_then(|hex| u64::from_str_radix(hex, 16).ok()),
                quality,
                people,
                size: match (pixels(&value[5][0]), pixels(&value[5][1])) {
                    (Some(width), Some(height)) => Some((width, height)),
                    _ => None,
                },
            };
            entries.insert(photo, meta);
        }
        Index { path, entries: Mutex::new(entries), dirty: AtomicBool::new(false) }
    }

    /// Write the index to disk if anything changed since the last save.
    pub fn save(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let saved: serde_json::Map<String, Value> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|(photo, meta)| {
                let hash = meta.hash.map(|h| format!("{h:016x}"));
                let (sharpness, clipped) = meta.quality.map_or((None, None), |q| (Some(q.sharpness), Some(q.clipped)));
                let people = meta.people.map(|p| [u16::from(p.faces), p.eyes_open, p.smiling]);
                let size = meta.size.map(|(width, height)| [width, height]);
                (photo.to_string(), json!([meta.taken, hash, sharpness, clipped, people, size]))
            })
            .collect();
        if let Err(e) = store::write_atomically(&self.path, Value::Object(saved).to_string().as_bytes()) {
            debug!(error = %e, "could not save the photo index");
        }
    }

    pub fn get(&self, photo: &PhotoPath) -> Meta {
        self.entries.lock().unwrap().get(photo).copied().unwrap_or_default()
    }

    fn update(&self, photo: &PhotoPath, change: impl FnOnce(&mut Meta)) {
        let mut entries = self.entries.lock().unwrap();
        let meta = entries.entry(photo.clone()).or_default();
        let before = *meta;
        change(meta);
        if *meta != before {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn set_taken(&self, photo: &PhotoPath, taken: u64) {
        self.update(photo, |meta| meta.taken = Some(taken));
    }

    pub fn set_hash(&self, photo: &PhotoPath, hash: u64) {
        self.update(photo, |meta| meta.hash = Some(hash));
    }

    pub fn set_quality(&self, photo: &PhotoPath, quality: Quality) {
        self.update(photo, |meta| meta.quality = Some(quality));
    }

    pub fn set_size(&self, photo: &PhotoPath, size: (u32, u32)) {
        self.update(photo, |meta| meta.size = Some(size));
    }

    pub fn set_people(&self, photo: &PhotoPath, people: People) {
        self.update(photo, |meta| meta.people = Some(people));
    }

    pub fn forget(&self, photos: &[PhotoPath]) {
        let mut entries = self.entries.lock().unwrap();
        for photo in photos {
            if entries.remove(photo).is_some() {
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    /// How many photos have a fingerprint, a quality measurement, and a look at their faces.
    pub fn analysed(&self) -> Analysed {
        let entries = self.entries.lock().unwrap();
        let count = |has: fn(&Meta) -> bool| entries.values().filter(|meta| has(meta)).count();
        Analysed {
            fingerprints: count(|meta| meta.hash.is_some()),
            quality: count(|meta| meta.quality.is_some()),
            people: count(|meta| meta.people.is_some()),
        }
    }
}

/// A 64-bit "difference hash": shrink to 9x8 grey pixels and record, for each
/// pixel, whether it is brighter than its right-hand neighbour. Copies of a
/// photo at different sizes or qualities get the same or nearly the same bits.
pub fn dhash(img: &DynamicImage) -> u64 {
    let small = img.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            hash = (hash << 1) | u64::from(small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0]);
        }
    }
    hash
}

/// Group photos whose fingerprints differ in at most `max_distance` bits.
/// A photo joins the first earlier photo it matches; groups come largest first.
/// Featureless images (all bits equal) say nothing about likeness and are left out.
pub fn similar_groups<T: Clone>(photos: &[(T, u64)], max_distance: u32) -> Vec<Vec<T>> {
    let usable: Vec<&(T, u64)> = photos.iter().filter(|(_, hash)| *hash != 0 && *hash != u64::MAX).collect();
    let mut grouped = vec![false; usable.len()];
    let mut groups = Vec::new();
    for (i, (photo, hash)) in usable.iter().enumerate() {
        if grouped[i] {
            continue;
        }
        let mut group = vec![photo.clone()];
        for (j, (other, other_hash)) in usable.iter().enumerate().skip(i + 1) {
            if !grouped[j] && (hash ^ other_hash).count_ones() <= max_distance {
                grouped[j] = true;
                group.push(other.clone());
            }
        }
        if group.len() > 1 {
            groups.push(group);
        }
    }
    groups.sort_by_key(|group| std::cmp::Reverse(group.len()));
    groups
}

/// Bring dates up to date and forget photos that are gone. Returns the photos
/// that still need a thumbnail or a fingerprint. Blocking.
pub fn refresh(cfg: &Config, index: &Index) -> Vec<Photo> {
    let photos = store::list_photos(cfg);
    // Phone-app photos carry their date in the frame's media database.
    if photos.iter().any(|p| p.source == Source::Phone && index.get(&p.path).taken.is_none()) {
        for (photo, taken) in store::phone_shooting_times(cfg) {
            index.set_taken(&photo, taken);
        }
    }
    for photo in &photos {
        // Synced photos get their date from the album listing; for the rest, arrival time is the best we have.
        if photo.source != Source::Synced && index.get(&photo.path).taken.is_none() {
            index.set_taken(&photo.path, photo.mtime);
        }
    }
    // Reading a photo's size takes only the start of the file, so it is done here for all of them.
    for photo in photos.iter().filter(|p| index.get(&p.path).size.is_none()) {
        if let Some(size) = store::dimensions(cfg, &photo.path) {
            index.set_size(&photo.path, size);
        }
    }
    // Album dates are recorded for every item, downloaded or not, so keep entries for synced names.
    let present: HashSet<&PhotoPath> = photos.iter().map(|p| &p.path).collect();
    let stale: Vec<PhotoPath> = index
        .entries
        .lock()
        .unwrap()
        .keys()
        .filter(|photo| !present.contains(photo) && photo.source() != Source::Synced)
        .cloned()
        .collect();
    index.forget(&stale);
    index.save();
    let needs_work = |meta: Meta| meta.hash.is_none() || meta.quality.is_none();
    let mut pending: Vec<Photo> = photos
        .into_iter()
        .filter(|p| needs_work(index.get(&p.path)) || !store::thumbnail_exists(cfg, &p.path))
        .collect();
    // Photos that have look-alikes come first: those are the ones a suggestion is waiting on.
    let fingerprints: Vec<(PhotoPath, u64)> =
        index.entries.lock().unwrap().iter().filter_map(|(photo, meta)| Some((photo.clone(), meta.hash?))).collect();
    let in_groups: HashSet<PhotoPath> =
        similar_groups(&fingerprints, PRIORITY_DISTANCE).into_iter().flatten().collect();
    pending.sort_by_key(|photo| !in_groups.contains(&photo.path));
    pending
}

/// The photos that have look-alikes and whose faces have not been looked at yet: the only ones
/// where knowing about eyes and smiles changes a suggestion. Returns them with the number of
/// photos that have look-alikes at all. Blocking.
pub fn people_to_look_at(cfg: &Config, index: &Index) -> (Vec<PhotoPath>, usize) {
    let fingerprints: Vec<(PhotoPath, u64)> = store::list_photos(cfg)
        .into_iter()
        .filter_map(|p| Some((index.get(&p.path).hash?, p.path)))
        .map(|(h, p)| (p, h))
        .collect();
    let with_look_alikes: Vec<PhotoPath> =
        similar_groups(&fingerprints, PRIORITY_DISTANCE).into_iter().flatten().collect();
    let total = with_look_alikes.len();
    (with_look_alikes.into_iter().filter(|photo| index.get(photo).people.is_none()).collect(), total)
}

/// Make one photo's thumbnail, fingerprint and quality measurement, whichever are missing. Blocking.
pub fn prepare(cfg: &Config, index: &Index, photo: &PhotoPath) -> Result<(), PhotoError> {
    let meta = index.get(photo);
    if meta.hash.is_none() || !store::thumbnail_exists(cfg, photo) {
        let thumb = store::thumbnail(cfg, photo)?;
        index.set_hash(photo, dhash(&image::load_from_memory(&thumb)?));
    }
    if meta.quality.is_none() {
        index.set_quality(photo, measure(&store::load_photo(cfg, photo)?));
    }
    Ok(())
}

/// Look at the faces in one photo, unless that was done before, and remember the result.
/// Takes a few seconds on the frame. Blocking.
pub fn look_at_people(cfg: &Config, index: &Index, photo: &PhotoPath, models: &Models) -> Result<People, PhotoError> {
    let meta = index.get(photo);
    if let Some(people) = meta.people {
        return Ok(people);
    }
    let image = store::load_photo(cfg, photo)?;
    if meta.quality.is_none() {
        index.set_quality(photo, measure(&image));
    }
    let people = models.analyse(&image).map_err(|e| PhotoError::Analysis(e.to_string()))?;
    index.set_people(photo, people);
    Ok(people)
}

/// Keep thumbnails, fingerprints and dates complete as photos come and go,
/// working on a few photos at a time in parallel.
pub async fn maintain(cfg: Arc<Config>, index: Arc<Index>) {
    loop {
        let (c, i) = (Arc::clone(&cfg), Arc::clone(&index));
        let pending = tokio::task::spawn_blocking(move || refresh(&c, &i)).await.unwrap_or_default();
        let started = Instant::now();
        let (mut made, mut failed) = (0usize, 0usize);
        let limit = Arc::new(Semaphore::new(PREPARE_IN_PARALLEL));
        let mut tasks = JoinSet::new();
        let mut tally = |result: Result<Result<(), PhotoError>, tokio::task::JoinError>| match result {
            Ok(Ok(())) => made += 1,
            _ => failed += 1,
        };
        for (n, photo) in pending.into_iter().enumerate() {
            let permit = Arc::clone(&limit).acquire_owned().await.expect("semaphore is never closed");
            let (c, i) = (Arc::clone(&cfg), Arc::clone(&index));
            tasks.spawn_blocking(move || {
                let _permit = permit;
                let result = prepare(&c, &i, &photo.path);
                if let Err(e) = &result {
                    debug!(photo = %photo.path, error = %e, "could not analyse a photo");
                }
                result
            });
            while let Some(result) = tasks.try_join_next() {
                tally(result);
            }
            if n % 200 == 199 {
                let i = Arc::clone(&index);
                let _ = tokio::task::spawn_blocking(move || i.save()).await;
            }
        }
        while let Some(result) = tasks.join_next().await {
            tally(result);
        }
        if made + failed > 0 {
            let i = Arc::clone(&index);
            let _ = tokio::task::spawn_blocking(move || i.save()).await;
            info!(made, failed, secs = started.elapsed().as_secs(), "photos analysed");
        }
        tokio::time::sleep(Duration::from_secs(120)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(rel: &str) -> PhotoPath {
        PhotoPath::parse(rel).unwrap()
    }
    use crate::store::testing::{picture, TempFrame};
    use image::{ImageFormat, Rgb, RgbImage};

    /// A picture with real structure: a diagonal gradient plus a bright block at (bx, by).
    fn scene(width: u32, height: u32, bx: u32, by: u32) -> DynamicImage {
        let img = RgbImage::from_fn(width, height, |x, y| {
            let (fx, fy) = (x * 8 / width, y * 8 / height);
            let base = ((fx * 37 + fy * 91 + fx * fy * 13) % 200) as u8;
            if fx == bx && fy == by {
                Rgb([255, 255, 255])
            } else {
                Rgb([base, base / 2, 255 - base])
            }
        });
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn the_same_picture_at_another_size_has_the_same_fingerprint() {
        let large = dhash(&scene(1920, 1080, 2, 3));
        let small = dhash(&scene(480, 270, 2, 3));
        assert!((large ^ small).count_ones() <= 2, "{large:016x} vs {small:016x}");
    }

    #[test]
    fn different_pictures_have_distant_fingerprints() {
        let a = dhash(&scene(640, 480, 2, 3));
        let flipped = dhash(&scene(640, 480, 2, 3).fliph());
        assert!((a ^ flipped).count_ones() > 12, "{a:016x} vs {flipped:016x}");
    }

    #[test]
    fn a_flat_picture_has_no_fingerprint_bits() {
        let flat = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 100, Rgb([90, 90, 90])));
        assert_eq!(dhash(&flat), 0);
    }

    fn photos(hashes: &[(&str, u64)]) -> Vec<(String, u64)> {
        hashes.iter().map(|(rel, hash)| (rel.to_string(), *hash)).collect()
    }

    #[test]
    fn exact_copies_are_grouped() {
        let groups = similar_groups(&photos(&[("a", 0xf0f0), ("b", 0x1234), ("c", 0xf0f0), ("d", 0xf0f1)]), 0);
        assert_eq!(groups, vec![vec!["a".to_string(), "c".to_string()]]);
    }

    #[test]
    fn near_copies_are_grouped_within_the_distance_and_largest_group_first() {
        let list = photos(&[
            ("a", 0b0000_1111),
            ("b", 0xffff_0000),
            ("c", 0b0000_1110),
            ("d", 0xffff_0001),
            ("e", 0xffff_0003),
            ("f", 0xdead_beef_0000),
        ]);
        let groups = similar_groups(&list, 2);
        assert_eq!(groups, vec![vec!["b".to_string(), "d".into(), "e".into()], vec!["a".to_string(), "c".into()]]);
        assert_eq!(similar_groups(&list, 0), Vec::<Vec<String>>::new());
    }

    #[test]
    fn featureless_pictures_are_never_grouped() {
        let list = photos(&[("black1", 0), ("black2", 0), ("white1", u64::MAX), ("white2", u64::MAX), ("x", 1)]);
        assert!(similar_groups(&list, 0).is_empty());
        assert!(similar_groups(&list, 3).is_empty());
    }

    #[test]
    fn refresh_dates_unsynced_photos_and_lists_what_needs_preparing() {
        let frame = TempFrame::new("index-refresh");
        frame.put("frame-sync/up_0123456789abcdef.jpg", &picture(64, 48, ImageFormat::Jpeg));
        frame.put("frame-sync/fs_0123456789abcdef.jpg", &picture(64, 48, ImageFormat::Jpeg));
        let index = Index::load(&frame.0);
        index.set_hash(&p("frame-sync/long-gone.jpg"), 9);
        index.set_taken(&p("frame-sync/fs_ffffffffffffffff.jpg"), 123); // album date for a photo not downloaded yet

        let pending = refresh(&frame.0, &index);
        assert_eq!(pending.len(), 2, "neither has a thumbnail or fingerprint yet");
        assert!(
            index.get(&p("frame-sync/up_0123456789abcdef.jpg")).taken.is_some(),
            "added photos fall back to arrival time"
        );
        assert_eq!(
            index.get(&p("frame-sync/fs_0123456789abcdef.jpg")).taken,
            None,
            "synced photos wait for the album date"
        );
        assert_eq!(
            index.get(&p("frame-sync/long-gone.jpg")),
            Meta::default(),
            "entries for removed photos are dropped"
        );
        assert_eq!(index.get(&p("frame-sync/fs_ffffffffffffffff.jpg")).taken, Some(123), "album dates are kept");

        for photo in &pending {
            prepare(&frame.0, &index, &photo.path).unwrap();
        }
        assert!(refresh(&frame.0, &index).is_empty(), "nothing left to do; faces are only looked at on request");
        assert!(store::thumbnail_exists(&frame.0, &p("frame-sync/up_0123456789abcdef.jpg")));
        assert!(index.get(&p("frame-sync/up_0123456789abcdef.jpg")).quality.is_some());
    }

    #[test]
    fn refresh_learns_the_size_of_every_photo_it_can_read() {
        let frame = TempFrame::new("index-sizes");
        frame.put("frame-sync/up_0123456789abcdef.jpg", &picture(64, 48, ImageFormat::Jpeg));
        frame.put("9user/photos/tall.webp", &picture(30, 50, ImageFormat::WebP));
        frame.put("frame-sync/up_00000000000000ff.jpg", b"not a picture");
        let index = Index::load(&frame.0);
        refresh(&frame.0, &index);
        assert_eq!(index.get(&p("frame-sync/up_0123456789abcdef.jpg")).size, Some((64, 48)));
        assert_eq!(index.get(&p("9user/photos/tall.webp")).size, Some((30, 50)));
        assert_eq!(index.get(&p("frame-sync/up_00000000000000ff.jpg")).size, None);
        assert_eq!(Index::load(&frame.0).get(&p("9user/photos/tall.webp")).size, Some((30, 50)), "and remembers it");
    }

    #[test]
    fn only_unchecked_photos_with_look_alikes_are_queued_for_a_look_at_their_faces() {
        let frame = TempFrame::new("index-people-queue");
        let index = Index::load(&frame.0);
        for (name, hash) in
            [("a", 0xffff_0000_u64), ("b", 0xffff_0001), ("c", 0xffff_0003), ("alone", 0x0f0f_0f0f_0000_0000)]
        {
            let rel = format!("frame-sync/{name}.jpg");
            frame.put(&rel, b"x");
            index.set_hash(&p(&rel), hash);
        }
        frame.put("frame-sync/unfingerprinted.jpg", b"x");
        index.set_people(&p("frame-sync/b.jpg"), People::default());
        let (mut todo, total) = people_to_look_at(&frame.0, &index);
        todo.sort();
        assert_eq!(todo, [p("frame-sync/a.jpg"), p("frame-sync/c.jpg")]);
        assert_eq!(total, 3, "three photos have look-alikes; one of them is already done");
    }

    #[test]
    fn photos_with_look_alikes_are_prepared_first() {
        let frame = TempFrame::new("index-priority");
        let index = Index::load(&frame.0);
        for (name, hash) in
            [("a", 0x0f0f_0f0f_u64), ("b", 0xffff_0000), ("c", 0xffff_0001), ("d", 0xdead_beef_0000_0000)]
        {
            let rel = format!("frame-sync/{name}.jpg");
            frame.put(&rel, &picture(32, 32, ImageFormat::Jpeg));
            index.set_hash(&p(&rel), hash);
        }
        let order: Vec<String> = refresh(&frame.0, &index).into_iter().map(|photo| photo.path.to_string()).collect();
        assert_eq!(order.len(), 4);
        let position = |name: &str| order.iter().position(|rel| rel == &format!("frame-sync/{name}.jpg")).unwrap();
        assert!(position("b").max(position("c")) < position("a").min(position("d")), "{order:?}");
    }

    fn shot(sharpness: u32, people: Option<People>) -> Meta {
        Meta { quality: Some(Quality { sharpness, clipped: 0 }), people, ..Meta::default() }
    }

    #[test]
    fn without_people_the_sharpest_shot_ranks_first() {
        let nobody = Some(People::default());
        let ranking = rank(&[shot(100, nobody), shot(900, nobody), shot(400, None)]);
        assert!(!ranking.by_expressions);
        assert_eq!(ranking.scores, vec![Some(100.0), Some(900.0), Some(400.0)]);
    }

    #[test]
    fn with_people_expressions_outrank_sharpness() {
        let great = People { faces: 2, eyes_open: 200, smiling: 180 };
        let blink = People { faces: 2, eyes_open: 100, smiling: 180 };
        let ranking = rank(&[shot(1000, Some(blink)), shot(500, Some(great))]);
        assert!(ranking.by_expressions);
        let (sharp_blink, soft_great) = (ranking.scores[0].unwrap(), ranking.scores[1].unwrap());
        assert!(soft_great > sharp_blink, "{soft_great} vs {sharp_blink}");
    }

    #[test]
    fn with_equal_expressions_the_sharper_shot_wins() {
        let same = Some(People { faces: 1, eyes_open: 100, smiling: 50 });
        let ranking = rank(&[shot(300, same), shot(800, same)]);
        assert!(ranking.scores[1] > ranking.scores[0]);
    }

    #[test]
    fn a_shot_missing_a_face_ranks_below_one_with_everyone() {
        let everyone = People { faces: 3, eyes_open: 270, smiling: 150 };
        let one_turned_away = People { faces: 2, eyes_open: 200, smiling: 200 };
        let ranking = rank(&[shot(500, Some(one_turned_away)), shot(500, Some(everyone))]);
        assert!(ranking.scores[1] > ranking.scores[0]);
    }

    #[test]
    fn expressions_are_only_used_once_every_shot_has_been_looked_at() {
        let great = Some(People { faces: 1, eyes_open: 100, smiling: 100 });
        let ranking = rank(&[shot(100, great), shot(900, None)]);
        assert!(!ranking.by_expressions);
        assert_eq!(ranking.scores, vec![Some(100.0), Some(900.0)]);
        let unmeasured = Meta { people: great, ..Meta::default() };
        assert_eq!(rank(&[unmeasured, shot(5, great)]).scores[0], None, "no score without a quality measurement");
        assert_eq!(rank(&[]).scores, Vec::<Option<f64>>::new());
    }

    #[test]
    fn a_blurred_copy_measures_as_less_sharp() {
        let crisp = scene(1280, 960, 2, 3);
        let blurred = crisp.blur(3.0);
        let (crisp, blurred) = (measure(&crisp), measure(&blurred));
        assert!(crisp.sharpness > blurred.sharpness * 2, "{crisp:?} vs {blurred:?}");
        assert!(crisp.score() > blurred.score());
    }

    #[test]
    fn sharpness_is_comparable_across_sizes() {
        // The same picture at two sizes is reduced to the same working size before measuring.
        let (large, small) = (measure(&scene(4000, 3000, 2, 3)), measure(&scene(2000, 1500, 2, 3)));
        let ratio = f64::from(large.sharpness) / f64::from(small.sharpness.max(1));
        assert!((0.5..2.0).contains(&ratio), "{large:?} vs {small:?}");
    }

    #[test]
    fn blown_out_and_crushed_areas_count_as_clipped() {
        let flat = |level: u8| DynamicImage::ImageRgb8(RgbImage::from_pixel(64, 64, Rgb([level, level, level])));
        assert_eq!(measure(&flat(255)), Quality { sharpness: 0, clipped: 1000 });
        assert_eq!(measure(&flat(0)), Quality { sharpness: 0, clipped: 1000 });
        assert_eq!(measure(&flat(128)), Quality { sharpness: 0, clipped: 0 });
        let half =
            DynamicImage::ImageRgb8(RgbImage::from_fn(
                64,
                64,
                |x, _| if x < 32 { Rgb([255; 3]) } else { Rgb([128; 3]) },
            ));
        assert_eq!(measure(&half).clipped, 500);
    }

    #[test]
    fn clipping_lowers_the_score_but_only_so_far() {
        let clean = Quality { sharpness: 1000, clipped: 0 };
        assert!((clean.score() - 1000.0).abs() < f64::EPSILON);
        assert!((Quality { sharpness: 1000, clipped: 250 }.score() - 750.0).abs() < 1e-9);
        assert!((Quality { sharpness: 1000, clipped: 1000 }.score() - 400.0).abs() < 1e-9, "the penalty is capped");
        assert!(Quality { sharpness: 2000, clipped: 300 }.score() > clean.score(), "a much sharper photo still wins");
    }

    #[test]
    fn tiny_pictures_do_not_break_measuring() {
        for (w, h) in [(1, 1), (2, 2), (3, 1)] {
            let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([10, 200, 30])));
            assert_eq!(measure(&img).sharpness, 0, "{w}x{h}");
        }
    }

    #[test]
    fn preparing_a_broken_photo_fails_without_recording_a_fingerprint() {
        let frame = TempFrame::new("index-broken");
        frame.put("frame-sync/broken.jpg", b"junk");
        let index = Index::load(&frame.0);
        assert!(prepare(&frame.0, &index, &p("frame-sync/broken.jpg")).is_err());
        assert_eq!(index.get(&p("frame-sync/broken.jpg")), Meta::default());
    }

    #[test]
    fn index_round_trips_through_its_file() {
        let frame = TempFrame::new("index");
        let index = Index::load(&frame.0);
        assert_eq!(index.get(&p("a.jpg")), Meta::default());
        index.set_taken(&p("a.jpg"), 1_700_000_000);
        index.set_hash(&p("a.jpg"), 0xdead_beef_0000_0001);
        index.set_quality(&p("a.jpg"), Quality { sharpness: 321, clipped: 12 });
        index.set_people(&p("a.jpg"), People { faces: 2, eyes_open: 150, smiling: 90 });
        index.set_hash(&p("b.jpg"), 7);
        index.set_size(&p("a.jpg"), (1920, 1080));
        index.save();

        let reloaded = Index::load(&frame.0);
        assert_eq!(
            reloaded.get(&p("a.jpg")),
            Meta {
                taken: Some(1_700_000_000),
                hash: Some(0xdead_beef_0000_0001),
                quality: Some(Quality { sharpness: 321, clipped: 12 }),
                people: Some(People { faces: 2, eyes_open: 150, smiling: 90 }),
                size: Some((1920, 1080)),
            }
        );
        assert_eq!(reloaded.get(&p("b.jpg")), Meta { hash: Some(7), ..Meta::default() });
        assert_eq!(reloaded.analysed(), Analysed { fingerprints: 2, quality: 1, people: 1 });

        reloaded.forget(&[p("a.jpg"), p("never-there.jpg")]);
        reloaded.save();
        assert_eq!(Index::load(&frame.0).analysed(), Analysed { fingerprints: 1, quality: 0, people: 0 });
    }

    #[test]
    fn index_only_writes_when_something_changed() {
        let frame = TempFrame::new("index-dirty");
        let index = Index::load(&frame.0);
        index.save();
        assert!(!frame.0.data_dir.join("index.json").exists(), "nothing to save yet");
        index.set_taken(&p("a.jpg"), 5);
        index.save();
        let written = fs::metadata(frame.0.data_dir.join("index.json")).unwrap().modified().unwrap();
        index.set_taken(&p("a.jpg"), 5); // same value
        index.save();
        assert_eq!(fs::metadata(frame.0.data_dir.join("index.json")).unwrap().modified().unwrap(), written);
    }

    #[test]
    fn a_corrupt_index_file_starts_empty() {
        let frame = TempFrame::new("index-corrupt");
        fs::write(frame.0.data_dir.join("index.json"), "{broken").unwrap();
        assert_eq!(Index::load(&frame.0).analysed().fingerprints, 0);
        fs::write(
            frame.0.data_dir.join("index.json"),
            r#"{"a.jpg":[null,"zz"],"b.jpg":"x","c.jpg":[12,"00000000000000ff"]}"#,
        )
        .unwrap();
        let index = Index::load(&frame.0);
        assert_eq!(index.get(&p("a.jpg")), Meta::default());
        assert_eq!(index.get(&p("c.jpg")), Meta { taken: Some(12), hash: Some(255), ..Meta::default() });
    }
}
