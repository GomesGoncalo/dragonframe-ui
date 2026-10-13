//! The photos on the frame's storage, and the Android commands that keep the
//! frame's own photo app in step with them.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader};
use serde_json::{json, Value};
use sha1::{Digest as _, Sha1};
use tracing::{debug, info, warn};

use crate::types::{
    hex, AlbumLink, CalendarLink, Chime, Digest, HomeKey, Night, PhotoPath, Place, Reminder, Restore, RoadsKey, Screen,
    Slideshow, Source, Spot, Station, SyncedName, Trip, SYNC_SUBDIR,
};

const MEDIA_URI: &str = "content://media/external/images/media";
const APP_PREFS: &str = "/data/data/com.fujia.calendar/shared_prefs/spUtils.xml";
const MAX_SIDE: u32 = 1920;
const THUMB_SIDE: u32 = 480;

/// Why a photo could not be read, converted or stored.
#[derive(Debug, thiserror::Error)]
pub enum PhotoError {
    #[error("not an image the frame can read (JPEG, PNG or WebP)")]
    NotAnImage,
    #[error("could not read the image ({0})")]
    Decode(#[from] image::ImageError),
    #[error("that photo is no longer on the frame")]
    Missing,
    #[error("could not save the photo ({0})")]
    Io(#[from] io::Error),
    #[error("could not analyse the photo ({0})")]
    Analysis(String),
}

pub struct Config {
    /// Everything the frame's photo app shows lives under here.
    pub cloud_dir: PathBuf,
    /// Our own state: thumbnails, excluded list, settings, log.
    pub data_dir: PathBuf,
}

impl Config {
    pub fn sync_dir(&self) -> PathBuf {
        self.cloud_dir.join(SYNC_SUBDIR)
    }
}

pub fn sha1(data: &[u8]) -> Digest {
    Sha1::digest(data).into()
}

pub fn sha1_hex(data: &[u8]) -> String {
    hex(&sha1(data))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Photo {
    pub path: PhotoPath,
    pub source: Source,
    /// When the file arrived on the frame, seconds since 1970.
    pub mtime: u64,
    pub size: u64,
}

/// Every photo on the frame, most recently added first.
pub fn list_photos(cfg: &Config) -> Vec<Photo> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<Photo>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue; // .downloading and friends
            }
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), &child, out);
            } else if let Some(path) = PhotoPath::parse(&child).filter(|_| meta.len() > 0) {
                let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok());
                out.push(Photo {
                    source: path.source(),
                    path,
                    mtime: mtime.map_or(0, |d| d.as_secs()),
                    size: meta.len(),
                });
            }
        }
    }
    let mut found = Vec::new();
    walk(&cfg.cloud_dir, "", &mut found);
    found.sort_by(|a, b| (b.mtime, &b.path).cmp(&(a.mtime, &a.path)));
    found
}

/// Names of the synced photos currently on the frame.
pub fn synced_names(cfg: &Config) -> Vec<SyncedName> {
    let Ok(entries) = fs::read_dir(cfg.sync_dir()) else {
        return Vec::new();
    };
    entries.flatten().filter_map(|e| SyncedName::parse(&e.file_name().to_string_lossy())).collect()
}

// --- Android side ------------------------------------------------------------

pub fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = match Command::new(program).args(args).output() {
        Ok(out) => out,
        Err(e) => {
            debug!(program, error = %e, "could not run Android command");
            return None;
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        warn!(program, ?args, status = ?out.status.code(), stderr = %stderr.trim(), "Android command failed");
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Ask `MediaStore` to index a file or a whole folder; the frame's photo app reads `MediaStore`.
pub fn scan(path: &Path) {
    let uri = format!("file://{}", path.display());
    run("am", &["broadcast", "-a", "android.intent.action.MEDIA_SCANNER_SCAN_FILE", "-d", &uri]);
}

fn unindex(paths: &[PathBuf]) {
    for chunk in paths.chunks(40) {
        let list: Vec<String> = chunk.iter().map(|p| format!("'{}'", p.display())).collect();
        let filter = format!("_data IN ({})", list.join(","));
        run("content", &["delete", "--uri", MEDIA_URI, "--where", &filter]);
    }
}

fn slideshow_in_front() -> bool {
    run("dumpsys", &["window"])
        .is_some_and(|out| out.lines().any(|l| l.contains("mCurrentFocus") && l.contains("SlideActivity")))
}

/// The photo the frame's slideshow is showing.
pub fn now_showing(cfg: &Config) -> Option<PhotoPath> {
    static CACHE: Mutex<Option<(Instant, Option<PhotoPath>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((at, value)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(3) {
            return value.clone();
        }
    }
    let value = (|| {
        if !slideshow_in_front() {
            return None;
        }
        let prefs = fs::read_to_string(APP_PREFS).ok()?;
        let start = prefs.find("name=\"slide_playing_data\">")? + "name=\"slide_playing_data\">".len();
        let path = &prefs[start..start + prefs[start..].find('<')?];
        PhotoPath::parse(path.strip_prefix(&format!("{}/", cfg.cloud_dir.display()))?)
    })();
    *cache = Some((Instant::now(), value.clone()));
    value
}

/// Close a running slideshow so it reloads its photo list next time it starts.
/// BACK is only ever sent while the slideshow is in front: on the home screen
/// it sends the vendor's launcher into a restart loop.
pub fn refresh_slideshow() -> bool {
    let showing = slideshow_in_front();
    if showing {
        run("input", &["keyevent", "BACK"]);
    }
    info!(closed = showing, "slideshow refresh requested");
    showing
}

/// (total, free) bytes of the frame's storage, if it can be read.
pub fn disk_space() -> Option<(u64, u64)> {
    parse_df(&run("df", &["-k", "/data"])?)
}

/// Total and available bytes from `df -k` output (sizes in 1K blocks on the last line).
fn parse_df(output: &str) -> Option<(u64, u64)> {
    let mut fields = output.lines().last()?.split_whitespace().skip(1);
    let total: u64 = fields.next()?.parse().ok()?;
    let free: u64 = fields.nth(1)?.parse().ok()?;
    Some((total * 1024, free * 1024))
}

/// When each phone-app photo was taken, from the description the vendor's app
/// stores in the frame's media database. Returns (photo, seconds since 1970).
pub fn phone_shooting_times(cfg: &Config) -> Vec<(PhotoPath, u64)> {
    let args = ["query", "--uri", MEDIA_URI, "--projection", "_data:description", "--where", "description IS NOT NULL"];
    let out = run("content", &args).unwrap_or_default();
    parse_shooting_times(&out, &cfg.cloud_dir.display().to_string())
}

fn parse_shooting_times(output: &str, cloud_dir: &str) -> Vec<(PhotoPath, u64)> {
    let prefix = format!("{cloud_dir}/");
    output
        .lines()
        .filter_map(|line| {
            let (path, description) = line.split_once("_data=")?.1.split_once(", description=")?;
            let photo = PhotoPath::parse(path.strip_prefix(&prefix)?)?;
            let key = "\"shootingTime\":\"";
            let at = description.find(key)? + key.len();
            Some((photo, parse_datetime(description.get(at..at + 19)?)?))
        })
        .collect()
}

/// "YYYY-MM-DD HH:MM:SS" as seconds since 1970 (taking the time as UTC).
fn parse_datetime(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() != 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b' '
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| text.get(range)?.parse::<u64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59
    {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's `days_from_civil`), with years counted from March.
    let y = if month <= 2 { year - 1 } else { year };
    let (era, yoe) = (y / 400, y % 400);
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

// --- changing what is on the frame --------------------------------------------

/// Delete photos from the frame. Synced ones go into the bin, so a sync does not bring them back.
pub fn delete(cfg: &Config, photos: &[PhotoPath]) {
    let mut files = Vec::new();
    let mut binned = Vec::new();
    for photo in photos {
        match photo.synced_name() {
            Some(name) => {
                // It can be restored later; keep a thumbnail so the bin can show what it was.
                let _ = thumbnail(cfg, photo);
                binned.push(name);
            }
            None => {
                let _ = fs::remove_file(thumb_path(cfg, photo));
            }
        }
        let file = photo.within(&cfg.cloud_dir);
        let _ = fs::remove_file(&file);
        let _ = fs::remove_file(format!("{}.md5", file.display())); // phone-app photos have one
        files.push(file);
    }
    unindex(&files);
    if !binned.is_empty() {
        let mut excluded = load_excluded(cfg);
        let mut bin = load_bin(cfg);
        let now = now_secs();
        for name in binned {
            bin.insert(name.clone(), now);
            excluded.push(name);
        }
        save_excluded(cfg, &excluded);
        save_bin(cfg, &bin);
    }
}

fn decode(data: &[u8]) -> Result<DynamicImage, PhotoError> {
    let reader = ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let mut decoder = reader.into_decoder().map_err(|_| PhotoError::NotAnImage)?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, PhotoError> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).encode_image(&img.to_rgb8())?;
    Ok(out)
}

pub fn write_atomically(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("part");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

/// Store an uploaded image. The page normally sends a frame-sized JPEG already;
/// anything else is converted here.
pub fn add(cfg: &Config, data: &[u8]) -> Result<PhotoPath, PhotoError> {
    let frame_ready = data.starts_with(&[0xff, 0xd8])
        && image::ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok())
            .is_some_and(|(w, h)| w.max(h) <= MAX_SIDE);
    let converted;
    let jpeg: &[u8] = if frame_ready {
        data
    } else {
        let img = decode(data)?;
        let img = if img.width().max(img.height()) > MAX_SIDE { img.thumbnail(MAX_SIDE, MAX_SIDE) } else { img };
        converted = encode_jpeg(&img, 85)?;
        &converted
    };
    // Named after its content, so adding the same photo twice stores it once.
    let photo = PhotoPath::added(&sha1(jpeg));
    fs::create_dir_all(cfg.sync_dir())?;
    let file = photo.within(&cfg.cloud_dir);
    write_atomically(&file, jpeg)?;
    scan(&file);
    Ok(photo)
}

// --- thumbnails ---------------------------------------------------------------

pub fn thumb_path(cfg: &Config, photo: &PhotoPath) -> PathBuf {
    cfg.data_dir.join("thumbs").join(format!("{}.jpg", sha1_hex(photo.as_str().as_bytes())))
}

/// Decoding full-size photos is the heaviest thing we do; keep it to three at a time
/// so one core is always left for the slideshow.
struct Slots(Mutex<usize>, Condvar);
static DECODERS: Slots = Slots(Mutex::new(3), Condvar::new());

struct Slot;
impl Slot {
    fn take() -> Slot {
        let mut free = DECODERS.0.lock().unwrap();
        while *free == 0 {
            free = DECODERS.1.wait(free).unwrap();
        }
        *free -= 1;
        Slot
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        *DECODERS.0.lock().unwrap() += 1;
        DECODERS.1.notify_one();
    }
}

/// A small JPEG of the photo, made on first use and kept in the data dir.
pub fn thumbnail(cfg: &Config, photo: &PhotoPath) -> Result<Vec<u8>, PhotoError> {
    let cached = thumb_path(cfg, photo);
    if let Ok(data) = fs::read(&cached) {
        return Ok(data);
    }
    let _slot = Slot::take();
    if let Ok(data) = fs::read(&cached) {
        return Ok(data); // made while we waited
    }
    let original = fs::read(photo.within(&cfg.cloud_dir)).map_err(|_| PhotoError::Missing)?;
    let thumb = encode_jpeg(&decode(&original)?.thumbnail(THUMB_SIDE, THUMB_SIDE), 72)?;
    if let Some(dir) = cached.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = write_atomically(&cached, &thumb);
    Ok(thumb)
}

/// Decode a photo in full, for analysis. Takes a decoder slot like thumbnail making does.
pub fn load_photo(cfg: &Config, photo: &PhotoPath) -> Result<DynamicImage, PhotoError> {
    let _slot = Slot::take();
    let data = fs::read(photo.within(&cfg.cloud_dir)).map_err(|_| PhotoError::Missing)?;
    decode(&data)
}

/// A photo's width and height in pixels, read from the start of the file without decoding it.
pub fn dimensions(cfg: &Config, photo: &PhotoPath) -> Option<(u32, u32)> {
    ImageReader::open(photo.within(&cfg.cloud_dir)).ok()?.with_guessed_format().ok()?.into_dimensions().ok()
}

pub fn thumbnail_exists(cfg: &Config, photo: &PhotoPath) -> bool {
    thumb_path(cfg, photo).exists()
}

/// How many thumbnails are stored.
pub fn thumbnail_count(cfg: &Config) -> usize {
    fs::read_dir(cfg.data_dir.join("thumbs")).map_or(0, Iterator::count)
}

// --- small persistent state -----------------------------------------------------

pub fn log_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("logs")
}

/// The last `lines` lines of the newest log file, for reading in a browser.
pub fn recent_log(cfg: &Config, lines: usize) -> String {
    let newest = fs::read_dir(log_dir(cfg)).ok().and_then(|entries| {
        let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        names.sort(); // daily files sort by date
        names.pop()
    });
    let text = newest.and_then(|path| fs::read_to_string(path).ok()).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let mut tail = all[all.len().saturating_sub(lines)..].join("\n");
    tail.push('\n');
    tail
}

/// The synced photos that syncs must leave out: everything deleted, whether still in the bin or not.
pub fn load_excluded(cfg: &Config) -> Vec<SyncedName> {
    let text = fs::read_to_string(cfg.data_dir.join("excluded.txt")).unwrap_or_default();
    text.lines().filter_map(|line| SyncedName::parse(line.trim())).collect()
}

pub fn save_excluded(cfg: &Config, names: &[SyncedName]) {
    let mut names = names.to_vec();
    names.sort();
    names.dedup();
    let text = names.iter().fold(String::new(), |text, name| text + name.as_str() + "\n");
    let _ = write_atomically(&cfg.data_dir.join("excluded.txt"), text.as_bytes());
}

/// The bin: synced photos deleted recently, with when each was deleted. They can still be
/// restored from the page. Photos emptied from the bin stay excluded from syncs but are no
/// longer listed. The first time, everything already excluded goes into the bin.
pub fn load_bin(cfg: &Config) -> BTreeMap<SyncedName, u64> {
    let Ok(text) = fs::read_to_string(cfg.data_dir.join("bin.json")) else {
        let now = now_secs();
        let bin: BTreeMap<SyncedName, u64> = load_excluded(cfg).into_iter().map(|name| (name, now)).collect();
        if !bin.is_empty() {
            save_bin(cfg, &bin);
        }
        return bin;
    };
    let saved: Value = serde_json::from_str(&text).unwrap_or_default();
    saved
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, deleted_at)| Some((SyncedName::parse(name)?, deleted_at.as_u64()?)))
        .collect()
}

pub fn save_bin(cfg: &Config, bin: &BTreeMap<SyncedName, u64>) {
    let _ = write_atomically(&cfg.data_dir.join("bin.json"), json!(bin).to_string().as_bytes());
}

/// Take photos out of the bin and off the excluded list, so the next sync brings them back.
/// Returns how many were restored.
pub fn restore(cfg: &Config, which: &Restore) -> usize {
    let mut bin = load_bin(cfg);
    let restored: Vec<SyncedName> = match which {
        Restore::These(names) => names.iter().filter(|name| bin.contains_key(*name)).cloned().collect(),
        Restore::All => bin.keys().cloned().collect(),
    };
    for name in &restored {
        bin.remove(name);
    }
    let excluded: Vec<SyncedName> = load_excluded(cfg).into_iter().filter(|name| !restored.contains(name)).collect();
    save_excluded(cfg, &excluded);
    save_bin(cfg, &bin);
    restored.len()
}

/// Empty the bin: the photos stay off the frame for good, their thumbnails are removed and
/// they are no longer listed. With `older_than`, only photos deleted that many seconds ago
/// or longer. Returns how many were emptied.
pub fn empty_bin(cfg: &Config, older_than: Option<u64>) -> usize {
    let mut bin = load_bin(cfg);
    let now = now_secs();
    let emptied: Vec<SyncedName> = bin
        .iter()
        .filter(|(_, deleted_at)| older_than.is_none_or(|age| now.saturating_sub(**deleted_at) >= age))
        .map(|(name, _)| name.clone())
        .collect();
    for name in &emptied {
        bin.remove(name);
        let photo = name.path();
        if !photo.within(&cfg.cloud_dir).exists() {
            let _ = fs::remove_file(thumb_path(cfg, &photo));
        }
    }
    if !emptied.is_empty() {
        save_bin(cfg, &bin);
        info!(count = emptied.len(), "deleted photos emptied from the bin");
    }
    emptied.len()
}

/// Forget the photos that were deleted for good, so syncs may download them again.
/// Returns how many were forgotten.
pub fn forget_emptied(cfg: &Config) -> usize {
    let bin = load_bin(cfg);
    let excluded = load_excluded(cfg);
    let keep: Vec<SyncedName> = excluded.iter().filter(|name| bin.contains_key(*name)).cloned().collect();
    save_excluded(cfg, &keep);
    excluded.len() - keep.len()
}

/// What the user has set up, kept in `settings.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// The shared albums to sync.
    pub albums: Vec<AlbumLink>,
    /// Whether to sync them once a day without being asked.
    pub daily_sync: bool,
    pub last_sync: Option<LastSync>,
    /// The PIN, when one is set, together with the sessions it has unlocked.
    pub protection: Option<Protection>,
    /// The calendars the frame subscribes to.
    pub calendars: Vec<CalendarLink>,
    /// Which screen the frame shows.
    pub screen: Screen,
    /// How the slideshow on the frame's own screen runs.
    pub slideshow: Slideshow,
    /// When the screen is switched off.
    pub night: Night,
    /// Whether calendar events are announced with a chime.
    pub chime: Chime,
    /// Where the frame is, for the weather.
    pub place: Option<Place>,
    /// The key the house's relay proves itself with, once the house is connected.
    pub home_key: Option<HomeKey>,
    /// The rules that turn calendar events into reminders.
    pub reminders: Vec<Reminder>,
    /// Where trips start unless they say otherwise.
    pub home: Option<Spot>,
    /// How the events that are travelled to are reached.
    pub trips: Vec<Trip>,
    /// The key for the driving service; without one, trips by car cannot be worked out.
    pub roads_key: Option<RoadsKey>,
    /// The radio stations the frame can play.
    pub stations: Vec<Station>,
    /// Whether Spotify's apps may play on the frame.
    pub spotify: bool,
}

/// How the most recent sync went.
#[derive(Clone, Debug, PartialEq)]
pub struct LastSync {
    /// When it finished, seconds since 1970.
    pub at: u64,
    pub succeeded: bool,
    pub summary: Option<String>,
}

/// A stored PIN (salted and hashed) and the sessions unlocked with it.
/// Sessions cannot exist without a PIN, so they live here.
#[derive(Clone, Debug, PartialEq)]
pub struct Protection {
    pub salt: String,
    pub hash: String,
    /// Hashes of the tokens handed to unlocked browsers.
    pub sessions: Vec<String>,
}

impl Settings {
    /// Read settings leniently: anything missing or malformed takes its default, so one bad
    /// field cannot cost the user their PIN or albums.
    fn from_json(saved: &Value) -> Settings {
        let strings = |value: &Value| -> Vec<String> {
            value.as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect()
        };
        Settings {
            albums: strings(&saved["sources"]).iter().filter_map(|link| AlbumLink::parse(link)).collect(),
            daily_sync: saved["auto"].as_bool().unwrap_or(false),
            last_sync: saved["last_sync"].as_u64().map(|at| LastSync {
                at,
                succeeded: saved["last_sync_ok"].as_bool().unwrap_or(true),
                summary: saved["last_sync_summary"].as_str().map(str::to_string),
            }),
            protection: match (saved["pin"]["salt"].as_str(), saved["pin"]["hash"].as_str()) {
                (Some(salt), Some(hash)) => {
                    Some(Protection { salt: salt.into(), hash: hash.into(), sessions: strings(&saved["sessions"]) })
                }
                _ => None,
            },
            calendars: strings(&saved["calendars"]).iter().filter_map(|link| CalendarLink::parse(link)).collect(),
            screen: serde_json::from_value(saved["screen"].clone()).unwrap_or_default(),
            slideshow: serde_json::from_value(saved["slideshow"].clone()).unwrap_or_default(),
            night: serde_json::from_value(saved["night"].clone()).unwrap_or_default(),
            chime: serde_json::from_value(saved["chime"].clone()).unwrap_or_default(),
            place: serde_json::from_value(saved["place"].clone()).ok(),
            home_key: saved["home_key"].as_str().and_then(HomeKey::parse),
            // One rule that no longer reads does not cost the others.
            reminders: saved["reminders"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|rule| serde_json::from_value(rule.clone()).ok())
                .collect(),
            home: saved["home"].as_str().and_then(Spot::parse),
            trips: saved["trips"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|trip| serde_json::from_value(trip.clone()).ok())
                .collect(),
            roads_key: saved["roads_key"].as_str().and_then(RoadsKey::parse),
            stations: saved["stations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|station| serde_json::from_value(station.clone()).ok())
                .collect(),
            spotify: saved["spotify"].as_bool().unwrap_or(false),
        }
    }

    fn to_json(&self) -> Value {
        let last = self.last_sync.as_ref();
        let protection = self.protection.as_ref();
        json!({
            "sources": self.albums,
            "auto": self.daily_sync,
            "last_sync": last.map(|l| l.at),
            "last_sync_ok": last.map(|l| l.succeeded),
            "last_sync_summary": last.and_then(|l| l.summary.as_deref()),
            "pin": protection.map(|p| json!({ "salt": p.salt, "hash": p.hash })),
            "sessions": protection.map_or(&[][..], |p| &p.sessions),
            "calendars": self.calendars,
            "screen": self.screen,
            "slideshow": self.slideshow,
            "night": self.night,
            "chime": self.chime,
            "place": self.place,
            "home_key": self.home_key,
            "reminders": self.reminders,
            "home": self.home,
            "trips": self.trips,
            "roads_key": self.roads_key,
            "stations": self.stations,
            "spotify": self.spotify,
        })
    }
}

pub fn load_settings(cfg: &Config) -> Settings {
    let saved =
        fs::read_to_string(cfg.data_dir.join("settings.json")).ok().and_then(|text| serde_json::from_str(&text).ok());
    Settings::from_json(&saved.unwrap_or(Value::Null))
}

pub fn save_settings(cfg: &Config, settings: &Settings) {
    let _ = write_atomically(&cfg.data_dir.join("settings.json"), settings.to_json().to_string().as_bytes());
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};

    /// A Config on a throwaway directory, removed when dropped.
    pub struct TempFrame(pub Config);

    impl TempFrame {
        pub fn new(name: &str) -> TempFrame {
            let root = std::env::temp_dir().join(format!("frameserver-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let cfg = Config { cloud_dir: root.join("cloud"), data_dir: root.join("data") };
            fs::create_dir_all(cfg.sync_dir()).unwrap();
            fs::create_dir_all(&cfg.data_dir).unwrap();
            TempFrame(cfg)
        }

        /// Put a file at a path relative to the cloud dir.
        pub fn put(&self, rel: &str, data: &[u8]) {
            let path = self.0.cloud_dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
    }

    impl Drop for TempFrame {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.cloud_dir.parent().unwrap());
        }
    }

    pub fn picture(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let img = RgbImage::from_pixel(width, height, Rgb([200, 40, 120]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use image::ImageFormat;

    const SYNCED: &str = "frame-sync/fs_0123456789abcdef.jpg";
    const PHONE: &str = "123user/photos/9/8/7/2026/06/1782161827915211.webp";

    fn size_of(data: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(data).unwrap();
        (img.width(), img.height())
    }

    fn p(rel: &str) -> PhotoPath {
        PhotoPath::parse(rel).unwrap()
    }

    fn n(name: &str) -> SyncedName {
        SyncedName::parse(name).unwrap()
    }

    fn names(list: &[&str]) -> Vec<SyncedName> {
        list.iter().map(|name| n(name)).collect()
    }

    /// The excluded list as plain text, for comparing.
    fn excluded(cfg: &Config) -> Vec<String> {
        load_excluded(cfg).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn listing_finds_photos_and_skips_everything_else() {
        let frame = TempFrame::new("listing");
        frame.put(SYNCED, b"x");
        frame.put(PHONE, b"x");
        frame.put(&format!("{PHONE}.md5"), b"x");
        frame.put(".downloading/123user/photos/partial.jpg", b"x");
        frame.put("frame-sync/empty.jpg", b"");
        frame.put("frame-sync/notes.txt", b"x");
        frame.put("frame-sync/half.part", b"x");
        let mut found: Vec<(String, Source, u64)> =
            list_photos(&frame.0).into_iter().map(|photo| (photo.path.to_string(), photo.source, photo.size)).collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(found, vec![(PHONE.to_string(), Source::Phone, 1), (SYNCED.to_string(), Source::Synced, 1)]);
        assert_eq!(synced_names(&frame.0), [n("fs_0123456789abcdef.jpg")]);
    }

    #[test]
    fn listing_is_newest_first() {
        let frame = TempFrame::new("order");
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        for (rel, age) in [("frame-sync/old.jpg", Some(old)), ("frame-sync/new.jpg", None)] {
            frame.put(rel, b"x");
            if let Some(time) = age {
                fs::File::options().write(true).open(frame.0.cloud_dir.join(rel)).unwrap().set_modified(time).unwrap();
            }
        }
        let rels: Vec<String> = list_photos(&frame.0).into_iter().map(|photo| photo.path.to_string()).collect();
        assert_eq!(rels, ["frame-sync/new.jpg", "frame-sync/old.jpg"]);
    }

    #[test]
    fn listing_an_empty_or_missing_frame_is_empty() {
        let frame = TempFrame::new("empty");
        assert!(list_photos(&frame.0).is_empty());
        fs::remove_dir_all(&frame.0.cloud_dir).unwrap();
        assert!(list_photos(&frame.0).is_empty());
        assert!(synced_names(&frame.0).is_empty());
    }

    #[test]
    fn adding_a_frame_sized_jpeg_stores_it_untouched() {
        let frame = TempFrame::new("add-small");
        let jpeg = picture(1920, 1080, ImageFormat::Jpeg);
        let rel = add(&frame.0, &jpeg).unwrap();
        assert_eq!(rel.source(), Source::Added);
        assert_eq!(fs::read(rel.within(&frame.0.cloud_dir)).unwrap(), jpeg);
        assert_eq!(add(&frame.0, &jpeg).unwrap(), rel, "same photo, same name: no duplicates");
        assert_eq!(list_photos(&frame.0).len(), 1);
    }

    #[test]
    fn adding_a_large_or_non_jpeg_image_converts_it() {
        let frame = TempFrame::new("add-big");
        let rel = add(&frame.0, &picture(4000, 3000, ImageFormat::Png)).unwrap();
        let stored = fs::read(rel.within(&frame.0.cloud_dir)).unwrap();
        assert!(stored.starts_with(&[0xff, 0xd8]), "stored as JPEG");
        assert_eq!(size_of(&stored), (1920, 1440));

        let rel = add(&frame.0, &picture(3000, 4000, ImageFormat::Jpeg)).unwrap();
        assert_eq!(size_of(&fs::read(rel.within(&frame.0.cloud_dir)).unwrap()), (1440, 1920));

        let rel = add(&frame.0, &picture(64, 48, ImageFormat::Png)).unwrap();
        assert_eq!(
            size_of(&fs::read(rel.within(&frame.0.cloud_dir)).unwrap()),
            (64, 48),
            "small images are not enlarged"
        );
    }

    #[test]
    fn adding_something_that_is_not_an_image_fails_and_stores_nothing() {
        let frame = TempFrame::new("add-bad");
        for data in [&b"not an image"[..], &[0xff, 0xd8, 0xff, 0x00, 0x01][..], &[][..]] {
            assert!(add(&frame.0, data).is_err());
        }
        assert!(list_photos(&frame.0).is_empty());
        assert_eq!(fs::read_dir(frame.0.sync_dir()).unwrap().count(), 0, "no partial files left behind");
    }

    #[test]
    fn deleting_removes_the_photo_its_sidecar_and_its_thumbnail() {
        let frame = TempFrame::new("delete");
        frame.put(PHONE, &picture(800, 600, ImageFormat::Png));
        frame.put(&format!("{PHONE}.md5"), b"abc");
        thumbnail(&frame.0, &p(PHONE)).unwrap();
        assert!(thumb_path(&frame.0, &p(PHONE)).exists());

        delete(&frame.0, &[p(PHONE)]);
        assert!(!frame.0.cloud_dir.join(PHONE).exists());
        assert!(!frame.0.cloud_dir.join(format!("{PHONE}.md5")).exists());
        assert!(!thumb_path(&frame.0, &p(PHONE)).exists());
        assert!(excluded(&frame.0).is_empty(), "only synced photos need excluding");
    }

    #[test]
    fn deleting_a_synced_photo_keeps_the_next_sync_from_restoring_it() {
        let frame = TempFrame::new("delete-synced");
        let added = add(&frame.0, &picture(100, 100, ImageFormat::Jpeg)).unwrap();
        frame.put(SYNCED, b"x");
        frame.put("frame-sync/fs_ffffffffffffffff.jpg", b"x");

        delete(&frame.0, &[p(SYNCED), added]);
        assert_eq!(excluded(&frame.0), vec!["fs_0123456789abcdef.jpg"]);
        assert_eq!(synced_names(&frame.0), [n("fs_ffffffffffffffff.jpg")]);

        delete(&frame.0, &[p(SYNCED)]); // already gone: harmless, and not listed twice
        assert_eq!(excluded(&frame.0).len(), 1);
    }

    #[test]
    fn a_deleted_synced_photo_keeps_a_thumbnail_so_it_can_be_recognised_later() {
        let frame = TempFrame::new("delete-keeps-thumb");
        let added = add(&frame.0, &picture(200, 150, ImageFormat::Jpeg)).unwrap();
        thumbnail(&frame.0, &added).unwrap();
        frame.put(SYNCED, &picture(800, 600, ImageFormat::Jpeg));
        assert!(!thumb_path(&frame.0, &p(SYNCED)).exists(), "not made yet");

        delete(&frame.0, &[p(SYNCED), added.clone()]);
        assert!(!frame.0.cloud_dir.join(SYNCED).exists());
        let kept = thumbnail(&frame.0, &p(SYNCED)).expect("the thumbnail outlives the photo");
        assert_eq!(size_of(&kept), (480, 360));
        assert!(!thumb_path(&frame.0, &added).exists(), "a photo that cannot be restored leaves nothing behind");
        assert!(thumbnail(&frame.0, &added).is_err());
    }

    #[test]
    fn deleted_synced_photos_go_into_the_bin_with_the_time() {
        let frame = TempFrame::new("bin");
        frame.put(SYNCED, &picture(64, 48, ImageFormat::Jpeg));
        let added = add(&frame.0, &picture(40, 40, ImageFormat::Jpeg)).unwrap();
        assert!(load_bin(&frame.0).is_empty());
        delete(&frame.0, &[p(SYNCED), added]);
        let bin = load_bin(&frame.0);
        assert_eq!(
            bin.keys().collect::<Vec<_>>(),
            [&n("fs_0123456789abcdef.jpg")],
            "only photos a sync could bring back"
        );
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(now.abs_diff(bin[&n("fs_0123456789abcdef.jpg")]) < 60);
    }

    #[test]
    fn photos_excluded_before_there_was_a_bin_are_put_in_it_once() {
        let frame = TempFrame::new("bin-migrate");
        save_excluded(&frame.0, &names(&["fs_aaaaaaaaaaaaaaaa.jpg", "fs_bbbbbbbbbbbbbbbb.jpg"]));
        assert_eq!(load_bin(&frame.0).len(), 2);
        assert_eq!(empty_bin(&frame.0, None), 2);
        assert!(load_bin(&frame.0).is_empty(), "emptied photos do not come back into the bin");
        assert_eq!(excluded(&frame.0).len(), 2, "and they stay excluded");
    }

    #[test]
    fn restoring_takes_photos_out_of_the_bin_and_off_the_excluded_list() {
        let frame = TempFrame::new("bin-restore");
        save_excluded(
            &frame.0,
            &names(&["fs_aaaaaaaaaaaaaaaa.jpg", "fs_bbbbbbbbbbbbbbbb.jpg", "fs_cccccccccccccccc.jpg"]),
        );
        load_bin(&frame.0);
        let some = Restore::These(names(&["fs_aaaaaaaaaaaaaaaa.jpg", "fs_dddddddddddddddd.jpg"])); // the second is not in the bin
        assert_eq!(restore(&frame.0, &some), 1);
        assert_eq!(excluded(&frame.0), ["fs_bbbbbbbbbbbbbbbb.jpg", "fs_cccccccccccccccc.jpg"]);
        assert_eq!(load_bin(&frame.0).len(), 2);
        assert_eq!(restore(&frame.0, &Restore::All), 2);
        assert!(excluded(&frame.0).is_empty() && load_bin(&frame.0).is_empty());
    }

    #[test]
    fn restoring_everything_leaves_permanently_deleted_photos_excluded() {
        let frame = TempFrame::new("bin-restore-all");
        save_excluded(&frame.0, &names(&["fs_aaaaaaaaaaaaaaaa.jpg"]));
        empty_bin(&frame.0, None);
        frame.put(SYNCED, b"x");
        delete(&frame.0, &[p(SYNCED)]);
        assert_eq!(restore(&frame.0, &Restore::All), 1);
        assert_eq!(excluded(&frame.0), ["fs_aaaaaaaaaaaaaaaa.jpg"]);
        assert_eq!(forget_emptied(&frame.0), 1);
        assert!(excluded(&frame.0).is_empty());
    }

    #[test]
    fn emptying_the_bin_removes_old_entries_and_their_thumbnails_only() {
        let frame = TempFrame::new("bin-empty");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let (old, recent, back) = ("fs_aaaaaaaaaaaaaaaa.jpg", "fs_bbbbbbbbbbbbbbbb.jpg", "fs_cccccccccccccccc.jpg");
        save_excluded(&frame.0, &names(&[old, recent, back]));
        let bin: BTreeMap<SyncedName, u64> =
            [(old, now - 40 * 86_400), (recent, now - 86_400), (back, now - 40 * 86_400)]
                .map(|(name, at)| (n(name), at))
                .into();
        save_bin(&frame.0, &bin);
        for name in [old, recent, back] {
            let thumb = thumb_path(&frame.0, &n(name).path());
            fs::create_dir_all(thumb.parent().unwrap()).unwrap();
            fs::write(thumb, b"thumb").unwrap();
        }
        frame.put(&format!("{SYNC_SUBDIR}/{back}"), b"x"); // this one is on the frame again

        assert_eq!(empty_bin(&frame.0, Some(30 * 86_400)), 2);
        assert_eq!(load_bin(&frame.0).into_keys().collect::<Vec<_>>(), [n(recent)]);
        assert_eq!(excluded(&frame.0).len(), 3, "emptied photos stay off the frame");
        let has_thumb = |name: &str| thumb_path(&frame.0, &n(name).path()).exists();
        assert!(!has_thumb(old) && has_thumb(recent));
        assert!(has_thumb(back), "a photo that is on the frame keeps its thumbnail");
        assert_eq!(empty_bin(&frame.0, Some(30 * 86_400)), 0, "nothing more is old enough");
        assert_eq!(empty_bin(&frame.0, None), 1);
    }

    #[test]
    fn a_corrupt_bin_file_is_treated_as_empty_and_not_refilled() {
        let frame = TempFrame::new("bin-corrupt");
        save_excluded(&frame.0, &names(&["fs_aaaaaaaaaaaaaaaa.jpg"]));
        fs::write(frame.0.data_dir.join("bin.json"), "{broken").unwrap();
        assert!(load_bin(&frame.0).is_empty());
        fs::write(
            frame.0.data_dir.join("bin.json"),
            r#"{"fs_aaaaaaaaaaaaaaaa.jpg":5,"../x":7,"fs_bbbbbbbbbbbbbbbb.jpg":"soon"}"#,
        )
        .unwrap();
        assert_eq!(load_bin(&frame.0).into_iter().collect::<Vec<_>>(), [(n("fs_aaaaaaaaaaaaaaaa.jpg"), 5)]);
    }

    #[test]
    fn deleting_an_unreadable_synced_photo_still_works() {
        let frame = TempFrame::new("delete-broken");
        frame.put(SYNCED, b"not an image");
        delete(&frame.0, &[p(SYNCED)]);
        assert!(!frame.0.cloud_dir.join(SYNCED).exists());
        assert_eq!(excluded(&frame.0), vec!["fs_0123456789abcdef.jpg"]);
    }

    #[test]
    fn thumbnails_are_small_jpegs_and_cached() {
        let frame = TempFrame::new("thumb");
        frame.put(SYNCED, &picture(1920, 1080, ImageFormat::Jpeg));
        let thumb = thumbnail(&frame.0, &p(SYNCED)).unwrap();
        assert!(thumb.starts_with(&[0xff, 0xd8]));
        assert_eq!(size_of(&thumb), (480, 270));

        // Served from the cache afterwards, even if the original changes.
        frame.put(SYNCED, b"garbage");
        assert_eq!(thumbnail(&frame.0, &p(SYNCED)).unwrap(), thumb);
    }

    #[test]
    fn thumbnail_of_a_missing_or_broken_photo_is_an_error() {
        let frame = TempFrame::new("thumb-bad");
        assert!(thumbnail(&frame.0, &p(SYNCED)).is_err());
        frame.put(SYNCED, b"garbage");
        assert!(thumbnail(&frame.0, &p(SYNCED)).is_err());
        assert!(!thumb_path(&frame.0, &p(SYNCED)).exists());
    }

    #[test]
    fn excluded_list_round_trips_sorted_without_duplicates_or_junk() {
        let frame = TempFrame::new("excluded");
        assert!(excluded(&frame.0).is_empty());
        save_excluded(
            &frame.0,
            &names(&["fs_bbbbbbbbbbbbbbbb.jpg", "fs_aaaaaaaaaaaaaaaa.jpg", "fs_bbbbbbbbbbbbbbbb.jpg"]),
        );
        assert_eq!(excluded(&frame.0), ["fs_aaaaaaaaaaaaaaaa.jpg", "fs_bbbbbbbbbbbbbbbb.jpg"]);

        fs::write(
            frame.0.data_dir.join("excluded.txt"),
            "fs_aaaaaaaaaaaaaaaa.jpg\n\n../../etc/passwd\n  fs_cccccccccccccccc.jpg  \n",
        )
        .unwrap();
        assert_eq!(excluded(&frame.0), ["fs_aaaaaaaaaaaaaaaa.jpg", "fs_cccccccccccccccc.jpg"]);
    }

    #[test]
    fn settings_round_trip() {
        let frame = TempFrame::new("settings");
        assert_eq!(load_settings(&frame.0), Settings::default());
        let settings = Settings {
            albums: vec![AlbumLink::parse("https://photos.app.goo.gl/abc").unwrap()],
            daily_sync: true,
            last_sync: Some(LastSync { at: 1_700_000_000, succeeded: false, summary: Some("boom".into()) }),
            protection: Some(Protection {
                salt: "s".into(),
                hash: "h".into(),
                sessions: vec!["t1".into(), "t2".into()],
            }),
            calendars: vec![CalendarLink::parse("webcal://calendar.example/family.ics").unwrap()],
            screen: Screen::New,
            slideshow: serde_json::from_value(json!({ "every": 30, "order": "newest", "show": { "year": 2023 } }))
                .unwrap(),
            night: serde_json::from_value(json!({ "on": true, "from": "22:00", "until": "06:30", "when_dark": true }))
                .unwrap(),
            chime: serde_json::from_value(json!(10)).unwrap(),
            place: Place::new("Lisbon", 38.72, -9.14),
            home_key: HomeKey::parse("0123456789abcdef0123456789abcdef"),
            reminders: serde_json::from_value(json!([
                { "words": "bin, recycling", "when": { "days": 1, "at": "19:00" }, "say": "Bins out", "chime": true },
                { "words": "swimming", "when": { "minutes": 45 } },
                { "words": "wake up", "when": { "minutes": 0 }, "then": { "do": "play", "station": "Radio Four" } },
            ]))
            .unwrap(),
            home: Spot::parse("BR6 0AA"),
            trips: serde_json::from_value(json!([
                { "title": "Dentist", "place": "12 High St", "to": "BR6 0AA", "how": "transit" },
                { "title": "Match", "to": "Wembley", "how": "park", "via": "Orpington station", "margin": 20 },
            ]))
            .unwrap(),
            roads_key: RoadsKey::parse("0123456789abcdefABCDEF"),
            stations: serde_json::from_value(json!([
                { "name": "Radio Four", "stream": "https://stream.example/four.mp3" },
                { "name": "Jazz", "stream": "http://stream.example:8000/jazz" },
            ]))
            .unwrap(),
            spotify: true,
        };
        save_settings(&frame.0, &settings);
        assert_eq!(load_settings(&frame.0), settings);
        save_settings(&frame.0, &Settings::default());
        assert_eq!(load_settings(&frame.0), Settings::default());
    }

    #[test]
    fn settings_survive_a_corrupt_or_partly_wrong_file() {
        let frame = TempFrame::new("settings-corrupt");
        let file = frame.0.data_dir.join("settings.json");
        for junk in ["{not json", "[1,2]", "", "null"] {
            fs::write(&file, junk).unwrap();
            assert_eq!(load_settings(&frame.0), Settings::default(), "{junk:?}");
        }
        // One bad field does not cost the rest, and half a PIN is no PIN.
        fs::write(
            &file,
            r#"{"sources":["https://a","/etc",5],"auto":"yes","pin":{"salt":"s","hash":"h"},"sessions":["t"]}"#,
        )
        .unwrap();
        let settings = load_settings(&frame.0);
        assert_eq!(settings.albums.iter().map(AlbumLink::as_str).collect::<Vec<_>>(), ["https://a"]);
        assert!(!settings.daily_sync);
        assert_eq!(settings.protection.unwrap().sessions, ["t"]);
        fs::write(&file, r#"{"pin":{"salt":"s"},"sessions":["t"]}"#).unwrap();
        assert_eq!(load_settings(&frame.0).protection, None);
    }

    #[test]
    fn settings_written_by_earlier_versions_are_understood() {
        let frame = TempFrame::new("settings-old");
        let old = r#"{"auto":true,"last_sync":1791837161,"sources":["https://photos.app.goo.gl/abc"]}"#;
        fs::write(frame.0.data_dir.join("settings.json"), old).unwrap();
        let settings = load_settings(&frame.0);
        assert!(settings.daily_sync && settings.albums.len() == 1 && settings.protection.is_none());
        assert_eq!(settings.last_sync, Some(LastSync { at: 1_791_837_161, succeeded: true, summary: None }));
    }

    #[test]
    fn errors_say_what_went_wrong() {
        let frame = TempFrame::new("errors");
        assert!(matches!(add(&frame.0, b"not an image"), Err(PhotoError::NotAnImage)));
        assert!(matches!(thumbnail(&frame.0, &p(SYNCED)), Err(PhotoError::Missing)));
        assert_eq!(PhotoError::NotAnImage.to_string(), "not an image the frame can read (JPEG, PNG or WebP)");
        assert_eq!(PhotoError::Missing.to_string(), "that photo is no longer on the frame");
    }

    #[test]
    fn disk_space_is_read_from_df() {
        let output = "Filesystem            1K-blocks    Used Available Use% Mounted on\n/dev/block/mmcblk0p17  27298916 7540876  19758040  28% /data\n";
        assert_eq!(parse_df(output), Some((27_298_916 * 1024, 19_758_040 * 1024)));
        assert_eq!(parse_df(""), None);
        assert_eq!(parse_df("df: /data: No such file or directory"), None);
    }

    #[test]
    fn dates_are_parsed_as_seconds_since_1970() {
        assert_eq!(parse_datetime("1970-01-01 00:00:00"), Some(0));
        assert_eq!(parse_datetime("2026-02-11 10:24:25"), Some(1_770_805_465));
        assert_eq!(parse_datetime("2024-02-29 23:59:59"), Some(1_709_251_199));
        assert_eq!(parse_datetime("2000-03-01 00:00:00"), Some(951_868_800));
        for bad in [
            "",
            "2026-02-11",
            "2026-13-01 00:00:00",
            "2026-02-11T10:24:25",
            "1969-12-31 23:59:59",
            "2026-02-11 24:00:00",
            "abcd-ef-gh ij:kl:mn",
        ] {
            assert_eq!(parse_datetime(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn phone_photo_dates_come_from_the_media_database_rows() {
        let cloud = "/storage/emulated/0/CloudAlbum/cloud";
        let output = format!(
            concat!(
                "Row: 0 _data={cloud}/123user/photos/9/a.webp, description={{\"createTime\":\"2026-06-22 20:57:10\",\"shootingTime\":\"2026-02-11 10:24:25\",\"text\":\"\"}}\n",
                "Row: 1 _data={cloud}/123user/photos/9/b.webp, description={{\"shootingTime\":\"\"}}\n",
                "Row: 2 _data=/somewhere/else/c.webp, description={{\"shootingTime\":\"2026-02-11 10:24:25\"}}\n",
                "Row: 3 _data={cloud}/123user/photos/9/d.webp, description=NULL\n",
                "No result found.\n",
            ),
            cloud = cloud
        );
        assert_eq!(parse_shooting_times(&output, cloud), vec![(p("123user/photos/9/a.webp"), 1_770_805_465)]);
    }

    #[test]
    fn sha1_matches_known_value() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
