//! Values that are checked once, where they enter the program, and carry that
//! proof in their type.
//!
//! A `PhotoPath` can only be made from text that is safe to join onto the
//! photo folder and pass to commands; a `Pin` is always 4 to 8 digits; a
//! `Bounded` list always has an allowed length. Code that receives one of
//! these never has to check again, and cannot be handed something unchecked.
//! Each type parses itself out of JSON, so request bodies are valid by the
//! time a handler sees them.

use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate, NaiveTime};
use serde::de::{self, Deserialize, Deserializer};
use serde::Serialize;

/// Folder (inside the photo folder) that holds everything this app and `frame_sync.py` put on the frame.
pub const SYNC_SUBDIR: &str = "frame-sync";
const IMAGE_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
/// Characters of a digest used in a file name.
const ID_LENGTH: usize = 16;

/// A SHA-1 digest.
pub type Digest = [u8; 20];

/// Lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}"); // writing to a String cannot fail
    }
    out
}

/// The part of a digest that goes into a file name.
fn id(digest: &Digest) -> String {
    hex(digest)[..ID_LENGTH].to_string()
}

fn is_id(text: &str) -> bool {
    text.len() == ID_LENGTH && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Text with its `%xx` escapes undone. An escape that is not one is left as it is.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex =
            (bytes[i] == b'%').then(|| text.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(byte) = hex {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Text with everything but letters, digits and `-._~` written as `%xx`, for use in an address.
fn percent_encode(text: &str) -> String {
    use fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}"); // writing to a String cannot fail
        }
    }
    out
}

/// Decode base64 text, in either of its alphabets and with or without padding.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut held) = (0u32, 0u8);
    for byte in text.trim_end_matches('=').bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push(u8::try_from((bits >> held) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// Deserialize a string and parse it, turning a failed parse into a readable error.
fn parsed<'de, D: Deserializer<'de>, T>(
    deserializer: D,
    parse: impl Fn(&str) -> Option<T>,
    what: &str,
) -> Result<T, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse(&text).ok_or_else(|| de::Error::custom(format!("{what}: {text:?}")))
}

// --- photos -----------------------------------------------------------------------

/// Where a photo on the frame came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Put there by an album sync.
    Synced,
    /// Added by hand through this app.
    Added,
    /// Sent with the vendor's phone app.
    Phone,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Synced, Source::Added, Source::Phone];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Synced => "synced",
            Source::Added => "added",
            Source::Phone => "phone",
        }
    }
}

/// The file name of a synced photo: `fs_<16 hex digits>.jpg`. The digits identify the
/// album item, so the same item always gets the same name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SyncedName(String);

impl SyncedName {
    pub fn parse(name: &str) -> Option<SyncedName> {
        let id = name.strip_prefix("fs_")?.strip_suffix(".jpg")?;
        is_id(id).then(|| SyncedName(name.to_string()))
    }

    /// The name for the album item whose identity hashes to `digest`.
    pub fn from_digest(digest: &Digest) -> SyncedName {
        SyncedName(format!("fs_{}.jpg", id(digest)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Where the photo lives, relative to the photo folder.
    pub fn path(&self) -> PhotoPath {
        PhotoPath(format!("{SYNC_SUBDIR}/{}", self.0))
    }
}

impl fmt::Display for SyncedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SyncedName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(deserializer, SyncedName::parse, "not the name of a synced photo")
    }
}

/// The path of a photo relative to the frame's photo folder.
///
/// Having one proves the text is safe: it stays inside the folder, contains nothing a shell
/// or SQL filter could misread, is not hidden, and names an image file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PhotoPath(String);

impl PhotoPath {
    pub fn parse(rel: &str) -> Option<PhotoPath> {
        let safe = !rel.is_empty()
            && !rel.starts_with(['/', '.', '-'])
            && !rel.contains("..")
            && rel.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
            && Path::new(rel)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)));
        safe.then(|| PhotoPath(rel.to_string()))
    }

    /// The path for a photo added by hand whose content hashes to `digest`.
    pub fn added(digest: &Digest) -> PhotoPath {
        PhotoPath(format!("{SYNC_SUBDIR}/up_{}.jpg", id(digest)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The synced-photo name, if this is a synced photo.
    pub fn synced_name(&self) -> Option<SyncedName> {
        let name = self.0.strip_prefix(SYNC_SUBDIR)?.strip_prefix('/')?;
        SyncedName::parse(name)
    }

    pub fn source(&self) -> Source {
        match self.0.rsplit_once('/') {
            Some((SYNC_SUBDIR, name)) if SyncedName::parse(name).is_some() => Source::Synced,
            Some((SYNC_SUBDIR, _)) => Source::Added,
            _ => Source::Phone,
        }
    }

    /// The photo's location under `dir`.
    pub fn within(&self, dir: &Path) -> PathBuf {
        dir.join(&self.0)
    }
}

impl fmt::Display for PhotoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PhotoPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(deserializer, PhotoPath::parse, "not a photo path")
    }
}

// --- lists with an allowed length -----------------------------------------------------

/// A list with between `MIN` and `MAX` items.
#[derive(Clone, Debug, PartialEq)]
pub struct Bounded<T, const MIN: usize, const MAX: usize>(Vec<T>);

/// A list with at least one item.
pub type NonEmpty<T> = Bounded<T, 1, { usize::MAX }>;

impl<T, const MIN: usize, const MAX: usize> Bounded<T, MIN, MAX> {
    pub fn new(items: Vec<T>) -> Option<Self> {
        (MIN..=MAX).contains(&items.len()).then_some(Bounded(items))
    }

    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

/// A list that may be empty starts empty.
impl<T, const MAX: usize> Default for Bounded<T, 0, MAX> {
    fn default() -> Self {
        Bounded(Vec::new())
    }
}

impl<T, const MIN: usize, const MAX: usize> Deref for Bounded<T, MIN, MAX> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<'de, T: Deserialize<'de>, const MIN: usize, const MAX: usize> Deserialize<'de> for Bounded<T, MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = Vec::<T>::deserialize(deserializer)?;
        let count = items.len();
        Bounded::new(items).ok_or_else(|| {
            let allowed = if MAX == usize::MAX { format!("at least {MIN}") } else { format!("{MIN} to {MAX}") };
            de::Error::custom(format!("expected {allowed} items, got {count}"))
        })
    }
}

// --- the PIN --------------------------------------------------------------------------------

/// A PIN: 4 to 8 digits.
#[derive(Clone, PartialEq, Eq)]
pub struct Pin(String);

impl Pin {
    pub fn parse(text: &str) -> Option<Pin> {
        ((4..=8).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit())).then(|| Pin(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A PIN never appears in logs or debug output.
impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pin(****)")
    }
}

/// What a request to change the PIN asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum PinChange {
    Set(Pin),
    /// An empty PIN means "remove it".
    Remove,
}

impl<'de> Deserialize<'de> for PinChange {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.is_empty() {
            return Ok(PinChange::Remove);
        }
        Pin::parse(&text).map(PinChange::Set).ok_or_else(|| de::Error::custom("A PIN is 4 to 8 digits."))
    }
}

// --- albums ------------------------------------------------------------------------------------

/// A link to a shared album: an https address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AlbumLink(String);

impl AlbumLink {
    pub fn parse(text: &str) -> Option<AlbumLink> {
        let text = text.trim();
        let rest = text.strip_prefix("https://")?;
        (!rest.is_empty() && !text.contains(char::is_whitespace)).then(|| AlbumLink(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl AlbumLink {
    /// A link to a server inside a test, which cannot offer https.
    pub fn local(url: &str) -> AlbumLink {
        AlbumLink(url.to_string())
    }
}

impl fmt::Display for AlbumLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The album links typed into the page: one per line, blank lines ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumLinks(pub Vec<AlbumLink>);

impl<'de> Deserialize<'de> for AlbumLinks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| AlbumLink::parse(line).ok_or_else(|| de::Error::custom("album links must start with https://")))
            .collect::<Result<_, _>>()
            .map(AlbumLinks)
    }
}

/// The album links to sync now: at least one, typed one per line.
#[derive(Clone, Debug, PartialEq)]
pub struct AlbumsToSync(pub NonEmpty<AlbumLink>);

impl<'de> Deserialize<'de> for AlbumsToSync {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let AlbumLinks(links) = AlbumLinks::deserialize(deserializer)?;
        NonEmpty::new(links)
            .map(AlbumsToSync)
            .ok_or_else(|| de::Error::custom("add at least one Google Photos shared-album link (https://...)"))
    }
}

/// Which photos in the bin to restore.
#[derive(Debug, PartialEq, Eq)]
pub enum Restore {
    /// Everything in the bin.
    All,
    These(Vec<SyncedName>),
}

impl<'de> Deserialize<'de> for Restore {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor(PhantomData<()>);
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Restore;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("\"all\" or a list of synced photo names")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Restore, E> {
                if text == "all" {
                    Ok(Restore::All)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(text), &self))
                }
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Restore, A::Error> {
                Vec::<SyncedName>::deserialize(de::value::SeqAccessDeserializer::new(seq)).map(Restore::These)
            }
        }
        deserializer.deserialize_any(Visitor(PhantomData))
    }
}

/// How alike two photos must look to be grouped: the number of fingerprint bits (of 64)
/// that may differ, from 0 (identical-looking) to 10.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Likeness(u32);

impl Likeness {
    pub const IDENTICAL: Likeness = Likeness(0);
    const LOOSEST: u32 = 10;

    /// From a query parameter; anything unreadable means identical, anything larger is capped.
    pub fn from_query(text: Option<&str>) -> Likeness {
        text.and_then(|t| t.parse::<u32>().ok()).map_or(Likeness::IDENTICAL, |bits| Likeness(bits.min(Self::LOOSEST)))
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

// --- the calendar ----------------------------------------------------------------------------

/// Most calendars the frame subscribes to.
pub const MAX_CALENDARS: usize = 10;
const MAX_LINK_LENGTH: usize = 2000;
const MAX_TITLE_LENGTH: usize = 120;
const DAY_SECS: i64 = 24 * 60 * 60;

/// The address of a calendar to subscribe to: an https link to an iCalendar (.ics) feed.
/// A `webcal://` link, which is how calendars are often shared, means the same thing.
///
/// Such a link is usually a secret that gives access to the whole calendar, so it is never
/// printed in full: logs and debug output show only the host.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CalendarLink(String);

impl CalendarLink {
    pub fn parse(text: &str) -> Option<CalendarLink> {
        let text = text.trim();
        let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("webcal://"))?;
        let usable = !rest.is_empty() && text.len() <= MAX_LINK_LENGTH && !text.contains(char::is_whitespace);
        let link = format!("https://{rest}");
        usable.then(|| CalendarLink(google_feed(&link).unwrap_or(link)))
    }

    /// Whether this is one of Google Calendar's public feeds, which only public calendars have.
    pub fn is_google_public_feed(&self) -> bool {
        self.host() == GOOGLE_CALENDAR && self.0.ends_with("/public/basic.ics")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The server the calendar comes from, which is safe to show.
    pub fn host(&self) -> &str {
        let rest = self.0.split_once("://").map_or(self.0.as_str(), |(_, rest)| rest);
        rest.split(['/', '?', '#']).next().unwrap_or(rest)
    }
}

const GOOGLE_CALENDAR: &str = "calendar.google.com";

/// The feed behind one of Google Calendar's ordinary links: the one for sharing a calendar
/// (`...?cid=<the calendar's id, encoded>`) or for embedding it in a page (`...embed?src=<id>`).
/// None for any other address, including Google's own feed addresses, which are used as they are.
/// The feed this gives only works for a calendar that is public; a private one has a secret
/// feed address instead, which only its owner can look up.
fn google_feed(link: &str) -> Option<String> {
    let (host, path) = link.strip_prefix("https://")?.split_once('/')?;
    let calendar_page = match host {
        GOOGLE_CALENDAR => true,
        "www.google.com" => path.starts_with("calendar/"),
        _ => false,
    };
    if !calendar_page || path.contains("ical/") {
        return None;
    }
    let query = path.split_once('?')?.1.split('#').next()?;
    let named = |name: &str| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| percent_decode(value))
    };
    let plausible = |id: &String| id.contains('@') && !id.contains(|c: char| c.is_whitespace() || c.is_control());
    // The sharing link carries the id in base64; a few carry it as it is.
    let shared = named("cid")
        .map(|cid| base64_decode(&cid).and_then(|id| String::from_utf8(id).ok()).filter(plausible).unwrap_or(cid));
    let id = named("src").or(shared).filter(plausible)?;
    Some(format!("https://{GOOGLE_CALENDAR}/calendar/ical/{}/public/basic.ics", percent_encode(&id)))
}

#[cfg(test)]
impl CalendarLink {
    /// A link to a server inside a test, which cannot offer https.
    pub fn local(url: &str) -> CalendarLink {
        CalendarLink(url.to_string())
    }
}

impl fmt::Debug for CalendarLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CalendarLink({}/…)", self.host())
    }
}

impl<'de> Deserialize<'de> for CalendarLink {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        CalendarLink::parse(&text)
            .ok_or_else(|| de::Error::custom("calendar links must start with https:// or webcal://"))
    }
}

/// The calendar links typed into the page: one per line, blank lines ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalendarLinks(pub Vec<CalendarLink>);

impl<'de> Deserialize<'de> for CalendarLinks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut links: Vec<CalendarLink> = Vec::new();
        for line in Vec::<String>::deserialize(deserializer)?.iter().filter(|line| !line.trim().is_empty()) {
            let link = CalendarLink::parse(line)
                .ok_or_else(|| de::Error::custom("calendar links must start with https:// or webcal://"))?;
            if !links.contains(&link) {
                links.push(link);
            }
        }
        if links.len() > MAX_CALENDARS {
            return Err(de::Error::custom(format!("at most {MAX_CALENDARS} calendars")));
        }
        Ok(CalendarLinks(links))
    }
}

/// What an event is called: one line of 1 to 120 characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Title(String);

impl Title {
    pub fn parse(text: &str) -> Option<Title> {
        let text = text.trim();
        let length = text.chars().count();
        ((1..=MAX_TITLE_LENGTH).contains(&length) && !text.contains(char::is_control)).then(|| Title(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Title {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Title::parse(&text)
            .ok_or_else(|| de::Error::custom(format!("an event needs a title of 1 to {MAX_TITLE_LENGTH} characters")))
    }
}

/// Identifies an event that was added through this app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EventId(String);

impl EventId {
    pub fn parse(text: &str) -> Option<EventId> {
        is_id(text).then(|| EventId(text.to_string()))
    }

    pub fn from_digest(digest: &Digest) -> EventId {
        EventId(id(digest))
    }
}

impl<'de> Deserialize<'de> for EventId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(deserializer, EventId::parse, "not an event id")
    }
}

/// A date on the calendar, written `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day(pub NaiveDate);

impl Day {
    pub fn parse(text: &str) -> Option<Day> {
        let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
        // Four-digit years only, so the text form always reads back.
        (text.len() == 10 && (1900..=2200).contains(&date.year())).then_some(Day(date))
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.format("%Y-%m-%d"))
    }
}

impl Serialize for Day {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Day {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(deserializer, Day::parse, "not a date (YYYY-MM-DD)")
    }
}

/// A day about now, written `YYYY-MM-DD`: the server's own, or the one before or after.
/// The server does not know the time zone of whoever sends it, so either neighbour is as
/// good as its own day; anything further off is a mistake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Today(pub Day);

impl Today {
    /// `day`, if it is about `today`.
    pub fn parse(day: Day, today: NaiveDate) -> Option<Today> {
        ((day.0 - today).num_days().abs() <= 1).then_some(Today(day))
    }
}

impl<'de> Deserialize<'de> for Today {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let day = Day::deserialize(deserializer)?;
        Today::parse(day, chrono::Utc::now().date_naive()).ok_or_else(|| de::Error::custom("that is not today"))
    }
}

/// A time of day to the minute, written `HH:MM`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockTime(pub NaiveTime);

impl ClockTime {
    pub fn parse(text: &str) -> Option<ClockTime> {
        (text.len() == 5).then(|| NaiveTime::parse_from_str(text, "%H:%M").ok()).flatten().map(ClockTime)
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.format("%H:%M"))
    }
}

impl Serialize for ClockTime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(deserializer, ClockTime::parse, "not a time (HH:MM)")
    }
}

/// How often an event that was added here comes round again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Never,
    Weekly,
    Monthly,
    Yearly,
}

// --- the frame's own screen ---------------------------------------------------------------------

/// What the frame shows: the screen it came with, or the one this app serves at `/frame`.
/// It stays the one it came with until someone chooses otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Screen {
    #[default]
    Legacy,
    New,
}

/// The order photos are shown in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    #[default]
    Shuffled,
    /// Shuffled, with photos taken in the last month or year coming round more often.
    Recent,
    Newest,
}

/// Which photos the slideshow shows: all of them, the favourites, or those taken in one year.
/// Written `"all"`, `"favourites"` or `{"year": 2023}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Show {
    #[default]
    All,
    Favourites,
    Year(Year),
}

/// A year photos could have been taken in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Year(u16);

impl<'de> Deserialize<'de> for Year {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let year = u64::deserialize(deserializer)?;
        u16::try_from(year)
            .ok()
            .filter(|year| (1900..=2200).contains(year))
            .map(Year)
            .ok_or_else(|| de::Error::custom("not a year photos are from"))
    }
}

/// What someone has said about a photo: that it is a favourite, or that the frame should
/// not show it. A hidden photo stays on the frame and can be shown again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mark {
    Favourite,
    Hidden,
}

/// How long each screen of photos stays up: 5 seconds to an hour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Pace(u16);

impl Pace {
    pub fn new(seconds: u64) -> Option<Pace> {
        u16::try_from(seconds).ok().filter(|secs| (5..=3600).contains(secs)).map(Pace)
    }
}

impl Default for Pace {
    fn default() -> Self {
        Pace(15)
    }
}

impl<'de> Deserialize<'de> for Pace {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let seconds = u64::deserialize(deserializer)?;
        Pace::new(seconds).ok_or_else(|| de::Error::custom("photos change every 5 to 3600 seconds"))
    }
}

/// How the Pictures tab of the frame's own screen behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Slideshow {
    #[serde(default)]
    pub every: Pace,
    #[serde(default)]
    pub order: Order,
    /// Photos taken on today's date in earlier years are shown first.
    #[serde(default = "yes")]
    pub memories: bool,
    /// In the morning, today's events are listed under the clock.
    #[serde(default = "yes")]
    pub morning: bool,
    /// The weather is shown beside the clock.
    #[serde(default = "yes")]
    pub weather: bool,
    /// When the Calendar or Settings tab is left open, the screen goes back to Pictures.
    #[serde(default)]
    pub home_after: Rest,
    /// Which of the photos are shown.
    #[serde(default)]
    pub show: Show,
}

/// How many minutes a tab other than Pictures may be left alone before the screen goes back
/// to Pictures: 1 minute to a day, or null to stay where it was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Rest(Option<u16>);

impl Default for Rest {
    fn default() -> Self {
        Rest(Some(5))
    }
}

impl<'de> Deserialize<'de> for Rest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Option::<u64>::deserialize(deserializer)? {
            None => Ok(Rest(None)),
            Some(minutes) if (1..=1440).contains(&minutes) => Ok(Rest(u16::try_from(minutes).ok())),
            Some(_) => {
                Err(de::Error::custom("the screen goes back to Pictures after 1 to 1440 minutes, or never (null)"))
            }
        }
    }
}

fn yes() -> bool {
    true
}

impl Default for Slideshow {
    fn default() -> Self {
        Slideshow {
            every: Pace::default(),
            order: Order::default(),
            memories: true,
            morning: true,
            weather: true,
            home_after: Rest::default(),
            show: Show::default(),
        }
    }
}

/// When the frame's screen is switched off: between two times of day, and, if wanted,
/// whenever the room is dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Night {
    #[serde(default)]
    pub on: bool,
    #[serde(default = "Night::usual_start")]
    pub from: ClockTime,
    #[serde(default = "Night::usual_end")]
    pub until: ClockTime,
    /// Also off whenever the room has been dark for a while, whatever the time.
    #[serde(default)]
    pub when_dark: bool,
}

impl Night {
    fn usual_start() -> ClockTime {
        ClockTime(NaiveTime::from_hms_opt(23, 0, 0).unwrap_or(NaiveTime::MIN))
    }

    fn usual_end() -> ClockTime {
        ClockTime(NaiveTime::from_hms_opt(7, 0, 0).unwrap_or(NaiveTime::MIN))
    }

    /// Whether `time` falls in the night, which may run past midnight. A night that
    /// starts and ends at the same time is no night at all.
    pub fn covers(&self, time: NaiveTime) -> bool {
        let (from, until) = (self.from.0, self.until.0);
        if from <= until {
            from <= time && time < until
        } else {
            time >= from || time < until
        }
    }
}

impl Default for Night {
    fn default() -> Self {
        Night { on: false, from: Night::usual_start(), until: Night::usual_end(), when_dark: false }
    }
}

/// Whether the frame sounds a chime for calendar events, and how many minutes before
/// they start (at most two hours). Written as a number of minutes, or null for no chime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Chime(Option<u8>);

impl Chime {
    const MOST_MINUTES: u64 = 120;
}

impl<'de> Deserialize<'de> for Chime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let minutes = Option::<u64>::deserialize(deserializer)?;
        match minutes {
            None => Ok(Chime(None)),
            Some(minutes) if minutes <= Self::MOST_MINUTES => Ok(Chime(u8::try_from(minutes).ok())),
            Some(_) => Err(de::Error::custom("a chime comes at most 120 minutes before an event")),
        }
    }
}

/// A rule that turns calendar events into reminders on the frame: every event whose name has
/// one of the rule's words gets a note on the screen some time before it, which stays until
/// it is touched away or the event is over.
///
/// "Bin day" is one use (words `bin, recycling`, the evening before at 19:00); anything else
/// that comes round on a calendar is another rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Reminder {
    pub words: Words,
    pub when: Ahead,
    /// What the note says; without it, the event's own name.
    #[serde(default)]
    pub say: Option<Title>,
    /// Whether the note comes with a chime.
    #[serde(default)]
    pub chime: bool,
    /// Something for the frame to do when the note comes up, such as playing a station.
    #[serde(default, rename = "then")]
    pub act: Option<Action>,
}

/// The words a reminder looks for in the names of events: one to eight words or phrases,
/// written with commas between them. Capitals do not matter, and only whole words count,
/// give or take a plural: `bin` finds "Black bin" and "Bins out" but not "Robin".
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Words(String);

impl Words {
    const MOST: usize = 8;

    pub fn parse(text: &str) -> Option<Words> {
        let words: Vec<&str> = text.split(',').map(str::trim).filter(|word| !word.is_empty()).collect();
        let usable = (1..=Self::MOST).contains(&words.len())
            && words.iter().all(|word| word.chars().count() <= 40 && !word.contains(char::is_control));
        usable.then(|| Words(words.join(", ")))
    }

    /// Whether an event with this name is one the rule is about.
    pub fn found_in(&self, title: &str) -> bool {
        let title = title.to_lowercase();
        let letter = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        self.0.split(", ").map(str::to_lowercase).any(|word| {
            title.match_indices(&word).any(|(at, _)| {
                let after = &title[at + word.len()..];
                let after = after.strip_prefix("es").filter(|rest| !letter(rest.chars().next())).unwrap_or(after);
                let after = after.strip_prefix('s').filter(|rest| !letter(rest.chars().next())).unwrap_or(after);
                !letter(title[..at].chars().next_back()) && !letter(after.chars().next())
            })
        })
    }
}

impl<'de> Deserialize<'de> for Words {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Words::parse(&text).ok_or_else(|| {
            de::Error::custom("a reminder needs one to eight words to look for, with commas between them")
        })
    }
}

/// How long before an event its reminder comes: so many minutes before it starts (at most a
/// day), or at a time of day so many days before (0 for the day itself, at most 14).
/// Written `{"minutes": 30}` or `{"days": 1, "at": "19:00"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ahead {
    Minutes(u16),
    Days { days: u8, at: ClockTime },
}

impl Ahead {
    const MOST_MINUTES: u64 = 24 * 60;
    const MOST_DAYS: u64 = 14;
}

impl Serialize for Ahead {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Ahead::Minutes(minutes) => serde_json::json!({ "minutes": minutes }).serialize(serializer),
            Ahead::Days { days, at } => serde_json::json!({ "days": days, "at": at }).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Ahead {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            minutes: Option<u64>,
            days: Option<u64>,
            at: Option<ClockTime>,
        }
        match Raw::deserialize(deserializer)? {
            Raw { minutes: Some(minutes), days: None, at: None } if minutes <= Ahead::MOST_MINUTES => {
                Ok(Ahead::Minutes(u16::try_from(minutes).unwrap_or(0)))
            }
            Raw { minutes: None, days: Some(days), at: Some(at) } if days <= Ahead::MOST_DAYS => {
                Ok(Ahead::Days { days: u8::try_from(days).unwrap_or(0), at })
            }
            _ => Err(de::Error::custom(
                "a reminder comes up to 1440 minutes before an event, or at a time of day up to 14 days before",
            )),
        }
    }
}

/// The reminder rules: at most twenty.
pub type Reminders = Bounded<Reminder, 0, 20>;

// --- things the frame can be told to do ------------------------------------------------------------

/// One of the tabs of the frame's screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tab {
    Pictures,
    Calendar,
    Todo,
    Utilities,
    Settings,
}

impl Tab {
    pub fn parse(text: &str) -> Option<Tab> {
        serde_json::from_value(serde_json::Value::String(text.to_string())).ok()
    }
}

/// How long a timer runs: 1 to 600 minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Minutes(u16);

impl<'de> Deserialize<'de> for Minutes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let minutes = u64::deserialize(deserializer)?;
        u16::try_from(minutes)
            .ok()
            .filter(|minutes| (1..=600).contains(minutes))
            .map(Minutes)
            .ok_or_else(|| de::Error::custom("a timer runs for 1 to 600 minutes"))
    }
}

/// How loud the music is, from 0 (silent) to 100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Loudness(u8);

impl Loudness {
    pub fn new(percent: u64) -> Option<Loudness> {
        u8::try_from(percent).ok().filter(|percent| *percent <= 100).map(Loudness)
    }

    pub fn percent(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Loudness {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let percent = u64::deserialize(deserializer)?;
        Loudness::new(percent).ok_or_else(|| de::Error::custom("the volume goes from 0 to 100"))
    }
}

/// Something the frame's screen can be told to do, whoever does the telling: a button on
/// the phone page, a key on a remote control, a reminder rule, and in time a spoken word.
/// Written `{"do": "next"}`, `{"do": "show", "tab": "calendar"}` and so on.
///
/// Everything here is done by the screen page itself, so whatever can be done by touching
/// the frame can be done from elsewhere in the same way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// The next screen of photos.
    Next,
    /// The screen of photos before this one.
    Previous,
    /// Hold the slideshow where it is.
    Pause,
    /// Let the slideshow run again.
    Resume,
    /// Open a tab.
    Show {
        tab: Tab,
    },
    /// Start a timer.
    Timer {
        minutes: Minutes,
    },
    /// Play a radio station, by its name; without a name, the one that played last.
    Play {
        #[serde(default)]
        station: Option<Label>,
    },
    /// Stop the music.
    Stop,
    /// Play the station after the one that is playing.
    Skip,
    Louder,
    Quieter,
    Volume {
        to: Loudness,
    },
    /// Put a note on the screen for a minute, with a chime.
    Note {
        say: Title,
    },
    /// Celebrate points someone was given. Only the server tells the screen this, when the
    /// points are credited: it cannot be asked for.
    #[serde(skip_deserializing)]
    Cheer(Cheer),
}

/// Someone was given points: what the screen shows to celebrate it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cheer {
    pub name: String,
    /// What they were given the points for.
    pub what: String,
    pub points: i64,
    /// What they have now.
    pub has: i64,
    /// The reward they are saving for; none when there are no rewards.
    pub goal: Option<Goal>,
}

/// A reward someone is saving for, as the screen shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Goal {
    pub title: String,
    pub cost: u32,
}

impl Action {
    /// The station it asks for by name, if it does.
    pub fn station(&self) -> Option<&Label> {
        match self {
            Action::Play { station } => station.as_ref(),
            _ => None,
        }
    }
}

/// Where a radio station is listened to: an http or https address of at most 500 characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StreamLink(String);

impl StreamLink {
    pub fn parse(text: &str) -> Option<StreamLink> {
        let text = text.trim();
        let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"))?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let usable = !host.is_empty()
            && text.len() <= 500
            && !text.contains(|c: char| c.is_control() || c.is_whitespace() || matches!(c, '"' | '<' | '>'));
        usable.then(|| StreamLink(text.to_string()))
    }
}

impl<'de> Deserialize<'de> for StreamLink {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(
            deserializer,
            StreamLink::parse,
            "not the address of a radio stream (it starts with http:// or https://)",
        )
    }
}

/// Where a picture of a track's cover is: an https address of at most 300 characters, with
/// nothing in it that could break out of the page's markup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CoverLink(String);

impl CoverLink {
    pub fn parse(text: &str) -> Option<CoverLink> {
        let rest = text.strip_prefix("https://")?;
        let usable = !rest.is_empty()
            && text.len() <= 300
            && !text.contains(|c: char| c.is_control() || c.is_whitespace() || matches!(c, '"' | '<' | '>'));
        usable.then(|| CoverLink(text.to_string()))
    }
}

/// A radio station the frame can play.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Station {
    pub name: Label,
    pub stream: StreamLink,
}

/// The radio stations: at most twenty-four, no two with the same name (capitals aside),
/// since a station is asked for by its name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stations(Vec<Station>);

impl Stations {
    const MOST: usize = 24;

    pub fn new(stations: Vec<Station>) -> Option<Stations> {
        let names: std::collections::HashSet<String> =
            stations.iter().map(|station| station.name.as_str().to_lowercase()).collect();
        (stations.len() <= Self::MOST && names.len() == stations.len()).then_some(Stations(stations))
    }

    pub fn into_inner(self) -> Vec<Station> {
        self.0
    }
}

impl<'de> Deserialize<'de> for Stations {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Stations::new(Vec::<Station>::deserialize(deserializer)?)
            .ok_or_else(|| de::Error::custom("at most 24 stations, each with a name of its own"))
    }
}

/// What the screen page says about itself each time it asks what to do: which tab is open,
/// whether the slideshow is paused, and what is playing how loudly. Kept for the phone page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Said {
    pub tab: Option<Tab>,
    pub paused: bool,
    /// The station that is playing.
    pub playing: Option<Label>,
    pub volume: Option<Loudness>,
}

impl Said {
    /// From query parameters; anything unreadable is left unsaid.
    pub fn from_query(asked: impl Fn(&str) -> Option<String>) -> Said {
        Said {
            tab: asked("tab").and_then(|tab| Tab::parse(&tab)),
            paused: asked("paused").as_deref() == Some("1"),
            playing: asked("playing").and_then(|name| Label::parse(&name)),
            volume: asked("volume").and_then(|volume| volume.parse().ok()).and_then(Loudness::new),
        }
    }
}

// --- things to do --------------------------------------------------------------------------------

/// What someone who does tasks is called: one line of 1 to 30 characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PersonName(String);

impl PersonName {
    pub fn parse(text: &str) -> Option<PersonName> {
        let text = text.trim();
        let usable = (1..=30).contains(&text.chars().count()) && !text.contains(char::is_control);
        usable.then(|| PersonName(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PersonName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        PersonName::parse(&text).ok_or_else(|| de::Error::custom("a person needs a name of 1 to 30 characters"))
    }
}

/// What a task earns or a reward costs: nothing to ten thousand points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Points(u32);

impl Points {
    const MOST: u64 = 10_000;

    pub fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Points {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let points = u64::deserialize(deserializer)?;
        u32::try_from(points)
            .ok()
            .filter(|_| points <= Points::MOST)
            .map(Points)
            .ok_or_else(|| de::Error::custom("points go from 0 to 10000"))
    }
}

/// Points given to someone by hand, or taken away when below nothing: 1 to 1000 either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointChange(i32);

impl PointChange {
    pub fn new(change: i64) -> Option<PointChange> {
        i32::try_from(change).ok().filter(|change| *change != 0 && change.abs() <= 1000).map(PointChange)
    }

    pub fn get(self) -> i32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for PointChange {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let change = i64::deserialize(deserializer)?;
        PointChange::new(change).ok_or_else(|| de::Error::custom("give or take 1 to 1000 points at a time"))
    }
}

/// Days of the week, at least one: written as a list of numbers, 1 for Monday to 7 for Sunday.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Weekdays(u8);

impl Weekdays {
    pub fn has(self, day: chrono::Weekday) -> bool {
        self.0 & (1 << day.num_days_from_monday()) != 0
    }
}

impl Serialize for Weekdays {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq((1..=7u8).filter(|day| self.0 & (1 << (day - 1)) != 0))
    }
}

impl<'de> Deserialize<'de> for Weekdays {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let days = Vec::<u8>::deserialize(deserializer)?;
        if days.is_empty() || days.iter().any(|day| !(1..=7).contains(day)) {
            return Err(de::Error::custom("days of the week are 1 (Monday) to 7 (Sunday), and at least one"));
        }
        Ok(Weekdays(days.iter().fold(0, |set, day| set | (1 << (day - 1)))))
    }
}

// --- getting to events ------------------------------------------------------------------------

/// Somewhere a trip starts, ends or changes: a postcode, an address or the name of a station,
/// as someone typed it. One line of 1 to 120 characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Spot(String);

impl Spot {
    pub fn parse(text: &str) -> Option<Spot> {
        let text = text.trim();
        let usable = (1..=120).contains(&text.chars().count()) && !text.contains(char::is_control);
        usable.then(|| Spot(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// This as a UK postcode without its space ("BR60AA"), if that is what it is.
    pub fn postcode(&self) -> Option<String> {
        let code: String = self.0.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
        let (outward, inward) = code.split_at_checked(code.len().checked_sub(3)?)?;
        let inward_fits = inward.starts_with(|c: char| c.is_ascii_digit())
            && inward.chars().skip(1).all(|c| c.is_ascii_uppercase())
            && inward.len() == 3;
        let outward_fits = (2..=4).contains(&outward.len())
            && outward.starts_with(|c: char| c.is_ascii_uppercase())
            && outward.chars().all(|c| c.is_ascii_alphanumeric())
            && outward.contains(|c: char| c.is_ascii_digit());
        (inward_fits && outward_fits).then_some(code)
    }
}

impl<'de> Deserialize<'de> for Spot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Spot::parse(&text)
            .ok_or_else(|| de::Error::custom("a place is a postcode, an address or a station, up to 120 characters"))
    }
}

/// How a trip is made. `Park` is by car to a station and on from there by public transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum How {
    Transit,
    Car,
    Walk,
    Cycle,
    Park,
}

impl How {
    /// Whether working this out takes the driving service, which needs a key.
    pub fn drives(self) -> bool {
        matches!(self, How::Car | How::Park)
    }

    /// The ways there are of making a trip: the ones public transport's planner knows, and
    /// with a key for the driving service, the ones with a car in them.
    pub fn available(driving: bool) -> Vec<How> {
        let all = [How::Transit, How::Walk, How::Cycle, How::Car, How::Park];
        all.into_iter().filter(|how| driving || !how.drives()).collect()
    }
}

/// When a journey is to arrive, in seconds since 1970: from two days ago to 120 days ahead.
/// Events are looked at a few months around today, and refusing other moments keeps anyone
/// on the network from having the frame ask the planners (one of them metered) about any time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival(i64);

impl Arrival {
    const DAY: i64 = 24 * 60 * 60;

    /// `at`, if it is near enough to `now` (both in seconds since 1970).
    pub fn parse(at: i64, now: i64) -> Option<Arrival> {
        (now.saturating_sub(2 * Arrival::DAY)..=now.saturating_add(120 * Arrival::DAY))
            .contains(&at)
            .then_some(Arrival(at))
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Arrival {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // In a query string a number arrives as text.
        let text = String::deserialize(deserializer)?;
        let now = chrono::Utc::now().timestamp();
        let at = text.parse().map_err(|_| de::Error::custom(format!("not a moment (seconds since 1970): {text:?}")))?;
        Arrival::parse(at, now).ok_or_else(|| de::Error::custom("that is too far from today to plan for"))
    }
}

/// The key for the driving service (`TomTom`), as its owner typed it: 16 to 128 letters,
/// digits, dashes and underscores. It is kept on the frame and never handed back out.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RoadsKey(String);

impl RoadsKey {
    pub fn parse(text: &str) -> Option<RoadsKey> {
        let text = text.trim();
        let usable = (16..=128).contains(&text.len())
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        usable.then(|| RoadsKey(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A key is never printed in logs or debug output.
impl fmt::Debug for RoadsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoadsKey(****)")
    }
}

impl<'de> Deserialize<'de> for RoadsKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        RoadsKey::parse(&text).ok_or_else(|| de::Error::custom("that does not look like a key for the driving service"))
    }
}

/// A key for Yoto Limiter's API, as made in its app (Settings, API keys): `yl_` and then 8 to
/// 200 letters, digits, dashes and underscores. The frame only ever reads with it, so a key
/// with the `read` scope alone is enough. It is kept on the frame and never handed back out.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct YotoKey(String);

impl YotoKey {
    pub fn parse(text: &str) -> Option<YotoKey> {
        let text = text.trim();
        let rest = text.strip_prefix("yl_")?;
        let usable = (8..=200).contains(&rest.len())
            && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        usable.then(|| YotoKey(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A key is never printed in logs or debug output.
impl fmt::Debug for YotoKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("YotoKey(****)")
    }
}

impl<'de> Deserialize<'de> for YotoKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        YotoKey::parse(&text)
            .ok_or_else(|| de::Error::custom("that does not look like a Yoto Limiter key, which starts with yl_"))
    }
}

/// How someone gets to an event: every event with this name and this location is reached
/// this way. The frame works out when to leave and says so ahead of time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Trip {
    /// The event's name and, if it has one, its location, exactly as the calendar has them.
    pub title: Title,
    pub place: Option<String>,
    /// Where the trip starts; home when not given.
    pub from: Option<Spot>,
    pub to: Spot,
    pub how: How,
    /// The station driven to, for `How::Park` and only for it.
    pub via: Option<Spot>,
    /// Minutes to spare on top of the journey, at most two hours.
    pub margin: u8,
}

impl Trip {
    const USUAL_MARGIN: u64 = 10;
    const MOST_MARGIN: u64 = 120;

    /// Whether an event with this name and location is one this trip is for.
    pub fn is_for(&self, title: &str, place: Option<&str>) -> bool {
        self.title.as_str() == title.trim() && self.place.as_deref() == place
    }

    /// The same trip made another way, for comparing. Driving to a station needs the
    /// station, which only a trip set up that way has.
    pub fn by(&self, how: How) -> Option<Trip> {
        let via = if how == How::Park { Some(self.via.clone()?) } else { None };
        Some(Trip { how, via, ..self.clone() })
    }

    /// The ways this trip could be made, with or without the driving service.
    pub fn ways(&self, driving: bool) -> Vec<How> {
        How::available(driving).into_iter().filter(|how| self.by(*how).is_some()).collect()
    }
}

impl<'de> Deserialize<'de> for Trip {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            title: Title,
            #[serde(default)]
            place: Option<String>,
            #[serde(default)]
            from: Option<Spot>,
            to: Spot,
            how: How,
            #[serde(default)]
            via: Option<Spot>,
            #[serde(default)]
            margin: Option<u64>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let margin = raw.margin.unwrap_or(Trip::USUAL_MARGIN);
        if margin > Trip::MOST_MARGIN {
            return Err(de::Error::custom("the time to spare is at most 120 minutes"));
        }
        if raw.place.as_ref().is_some_and(|place| place.is_empty() || place.chars().count() > 300) {
            return Err(de::Error::custom("an event's location is 1 to 300 characters"));
        }
        if (raw.how == How::Park) != raw.via.is_some() {
            return Err(de::Error::custom("a station to drive to goes with driving to a station, and only with it"));
        }
        Ok(Trip {
            title: raw.title,
            place: raw.place,
            from: raw.from,
            to: raw.to,
            how: raw.how,
            via: raw.via,
            margin: u8::try_from(margin).unwrap_or(0),
        })
    }
}

/// The trips that are set up: at most forty.
pub type Trips = Bounded<Trip, 0, 40>;

/// A place on the map, for the weather.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl Place {
    /// A place with a name of 1 to 100 characters, somewhere on the globe.
    pub fn new(name: &str, latitude: f64, longitude: f64) -> Option<Place> {
        let name = name.trim();
        let named = (1..=100).contains(&name.chars().count()) && !name.contains(char::is_control);
        let on_the_globe = (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude);
        (named && on_the_globe).then(|| Place { name: name.to_string(), latitude, longitude })
    }
}

impl<'de> Deserialize<'de> for Place {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            name: String,
            latitude: f64,
            longitude: f64,
        }
        let raw = Raw::deserialize(deserializer)?;
        Place::new(&raw.name, raw.latitude, raw.longitude).ok_or_else(|| de::Error::custom("not a place on the map"))
    }
}

/// What someone typed to be looked up, such as the town the frame is in or the name of a
/// radio station: 1 to 100 characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceQuery(String);

impl PlaceQuery {
    pub fn parse(text: &str) -> Option<PlaceQuery> {
        let text = text.trim();
        let usable = (1..=100).contains(&text.chars().count()) && !text.contains(char::is_control);
        usable.then(|| PlaceQuery(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PlaceQuery {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        PlaceQuery::parse(&text).ok_or_else(|| de::Error::custom("type a name to look for, up to 100 characters"))
    }
}

// --- the house -----------------------------------------------------------------------------------

/// A short name to show on the screen: one line of 1 to 60 characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Label(String);

impl Label {
    pub fn parse(text: &str) -> Option<Label> {
        let text = text.trim();
        ((1..=60).contains(&text.chars().count()) && !text.contains(char::is_control)).then(|| Label(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(deserializer, Label::parse, "not a name of 1 to 60 characters")
    }
}

/// What a camera is called in addresses: 1 to 64 lower-case letters, digits and underscores.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CameraId(String);

impl CameraId {
    pub fn parse(text: &str) -> Option<CameraId> {
        let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_';
        ((1..=64).contains(&text.len()) && text.bytes().all(allowed)).then(|| CameraId(text.to_string()))
    }
}

impl<'de> Deserialize<'de> for CameraId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        parsed(deserializer, CameraId::parse, "not the name of a camera")
    }
}

/// What a house alarm is doing. Anything not known here is `Unknown`, so that a new state
/// some day is shown as such rather than refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlarmState {
    Disarmed,
    ArmedHome,
    ArmedAway,
    ArmedNight,
    ArmedVacation,
    ArmedCustomBypass,
    Arming,
    Pending,
    Triggered,
    #[serde(other)]
    Unknown,
}

/// What a sensor in the house watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorKind {
    Door,
    Window,
    Motion,
}

/// The key the house's relay proves itself with: 32 hex digits, made here.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct HomeKey(String);

impl HomeKey {
    pub fn parse(text: &str) -> Option<HomeKey> {
        (text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| HomeKey(text.to_string()))
    }

    /// Whether `offered` is this key. Every character is looked at, however early they differ.
    pub fn matches(&self, offered: &str) -> bool {
        let (ours, theirs) = (self.0.as_bytes(), offered.as_bytes());
        ours.len() == theirs.len() && ours.iter().zip(theirs).fold(0u8, |differing, (a, b)| differing | (a ^ b)) == 0
    }
}

/// A key is never printed in logs or debug output.
impl fmt::Debug for HomeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HomeKey(****)")
    }
}

/// What the frame's screen page asks of the display when it says it is running: to stay on
/// for a while (a timer is counting down), and to be on at a moment to come (an alarm).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vigil {
    /// Keep the display on for this long from now, at most four hours.
    pub stay_on: Option<std::time::Duration>,
    /// Have the display on at this moment, in seconds since 1970.
    pub wake_at: Option<u64>,
}

impl Vigil {
    const LONGEST_SECS: u64 = 4 * 60 * 60;

    /// From query parameters; anything unreadable asks for nothing.
    pub fn from_query(busy: Option<&str>, wake: Option<&str>) -> Vigil {
        let number = |text: Option<&str>| text.and_then(|t| t.parse::<u64>().ok()).filter(|n| *n > 0);
        Vigil {
            stay_on: number(busy).map(|secs| std::time::Duration::from_secs(secs.min(Self::LONGEST_SECS))),
            wake_at: number(wake),
        }
    }
}

/// The stretch of time a calendar request asks about, in seconds since 1970: never
/// backwards, and never longer than the calendar is prepared to work out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub from: i64,
    pub to: i64,
}

impl Window {
    const DEFAULT_DAYS: i64 = 35;
    const MAX_DAYS: i64 = 400;

    /// From query parameters. Without a readable start it begins a day ago; without a
    /// readable end it runs for five weeks; an end too far off is brought closer.
    pub fn from_query(from: Option<&str>, to: Option<&str>, now: i64) -> Window {
        let number = |text: Option<&str>| text.and_then(|t| t.parse::<i64>().ok());
        let earliest = now - 50 * 365 * DAY_SECS;
        let from = number(from).unwrap_or(now - DAY_SECS).clamp(earliest, now + 50 * 365 * DAY_SECS);
        let to =
            number(to).unwrap_or(from + Self::DEFAULT_DAYS * DAY_SECS).clamp(from, from + Self::MAX_DAYS * DAY_SECS);
        Window { from, to }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json};

    #[test]
    fn calendar_links_are_https_and_keep_their_secret() {
        let link = CalendarLink::parse(" webcal://p12-caldav.icloud.com/published/2/SECRET ").unwrap();
        assert_eq!(link.as_str(), "https://p12-caldav.icloud.com/published/2/SECRET");
        assert_eq!(link.host(), "p12-caldav.icloud.com");
        assert_eq!(format!("{link:?}"), "CalendarLink(p12-caldav.icloud.com/…)", "the secret part is never printed");
        assert_eq!(CalendarLink::parse("https://calendar.google.com?x=1").unwrap().host(), "calendar.google.com");
        for bad in ["", "https://", "webcal://", "http://plain", "ftp://x/y.ics", "https://a b", "/etc/x.ics"] {
            assert_eq!(CalendarLink::parse(bad), None, "{bad:?}");
        }
        assert_eq!(CalendarLink::parse(&format!("https://a/{}", "x".repeat(2000))), None, "absurdly long");
    }

    #[test]
    fn google_calendars_ordinary_links_become_the_calendars_public_feed() {
        let feed = |id: &str| format!("https://calendar.google.com/calendar/ical/{id}/public/basic.ics");
        let parsed = |link: &str| CalendarLink::parse(link).unwrap().as_str().to_string();
        // The link from "Get shareable link": the calendar's id in base64, with or without padding.
        assert_eq!(
            parsed("https://calendar.google.com/calendar/u/0?cid=ZmFtaWx5QGdyb3VwLmNhbGVuZGFyLmdvb2dsZS5jb20"),
            feed("family%40group.calendar.google.com")
        );
        assert_eq!(
            parsed("https://calendar.google.com/calendar?cid=c29tZW9uZUBnbWFpbC5jb20="),
            feed("someone%40gmail.com")
        );
        assert_eq!(
            parsed("https://calendar.google.com/calendar/r?cid=someone@gmail.com"),
            feed("someone%40gmail.com"),
            "an id given as it is"
        );
        // The link for embedding in a page.
        assert_eq!(
            parsed("https://calendar.google.com/calendar/embed?src=en.portuguese%23holiday%40group.v.calendar.google.com&ctz=Europe%2FLondon"),
            feed("en.portuguese%23holiday%40group.v.calendar.google.com")
        );
        assert_eq!(
            parsed("https://www.google.com/calendar/embed?height=600&src=someone%40gmail.com#top"),
            feed("someone%40gmail.com")
        );
        assert!(CalendarLink::parse(&feed("x%40y")).unwrap().is_google_public_feed());

        // Feeds, and anything that is not one of those links, are used as they are.
        for unchanged in [
            "https://calendar.google.com/calendar/ical/someone%40gmail.com/private-0123abcd/basic.ics",
            "https://calendar.google.com/calendar/ical/someone%40gmail.com/public/basic.ics",
            "https://calendar.google.com/calendar/u/0/r/week",
            "https://calendar.google.com/calendar/embed?ctz=Europe%2FLondon",
            "https://calendar.google.com/calendar?cid=!!!not-an-id!!!",
            "https://www.google.com/search?src=someone@gmail.com",
            "https://example.org/embed?src=someone%40gmail.com",
        ] {
            assert_eq!(parsed(unchanged), unchanged);
        }
        assert!(!CalendarLink::parse("https://calendar.google.com/calendar/ical/a%40b/private-0123/basic.ics")
            .unwrap()
            .is_google_public_feed());
        assert_eq!(base64_decode("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(base64_decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_decode("-_-_"), Some(vec![0xfb, 0xff, 0xbf]), "the address-safe alphabet too");
        assert_eq!(base64_decode("not base64!"), None);
        assert_eq!(percent_encode("a b@c#d-e_f.g~h"), "a%20b%40c%23d-e_f.g~h");
    }

    #[test]
    fn calendar_link_lists_drop_blanks_and_repeats_and_have_a_limit() {
        let links =
            from_value::<CalendarLinks>(json!([" https://a/1.ics ", "", "webcal://a/1.ics", "https://b"])).unwrap();
        assert_eq!(links.0.iter().map(CalendarLink::as_str).collect::<Vec<_>>(), ["https://a/1.ics", "https://b"]);
        assert_eq!(from_value::<CalendarLinks>(json!([])).unwrap(), CalendarLinks::default());
        let error = from_value::<CalendarLinks>(json!(["http://a"])).unwrap_err().to_string();
        assert!(error.contains("https:// or webcal://"), "{error}");
        let many: Vec<String> = (0..=MAX_CALENDARS).map(|n| format!("https://a/{n}")).collect();
        assert!(from_value::<CalendarLinks>(json!(many)).unwrap_err().to_string().contains("at most 10"));
        assert!(from_value::<CalendarLink>(json!("https://a")).is_ok());
        assert!(from_value::<CalendarLink>(json!("nope")).is_err());
    }

    #[test]
    fn titles_are_one_trimmed_line() {
        assert_eq!(Title::parse("  Dentist  ").unwrap().as_str(), "Dentist");
        assert!(Title::parse(&"é".repeat(120)).is_some(), "length is counted in characters");
        for bad in ["", "   ", "two\nlines", &"x".repeat(121)] {
            assert_eq!(Title::parse(bad), None, "{bad:?}");
        }
        assert!(from_value::<Title>(json!("")).unwrap_err().to_string().contains("1 to 120 characters"));
        assert_eq!(json!(Title::parse("Swim").unwrap()), json!("Swim"));
    }

    #[test]
    fn event_ids_are_sixteen_hex_digits() {
        let id = EventId::from_digest(&[0xcd; 20]);
        assert_eq!(json!(id), json!("cdcdcdcdcdcdcdcd"));
        assert_eq!(from_value::<EventId>(json!("cdcdcdcdcdcdcdcd")).unwrap(), id);
        for bad in ["", "cdcd", "CDCDCDCDCDCDCDCD", "../../etc/passwd"] {
            assert_eq!(EventId::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn days_and_times_read_and_write_one_form() {
        let day = Day::parse("2026-10-13").unwrap();
        assert_eq!((day.to_string(), json!(day)), ("2026-10-13".to_string(), json!("2026-10-13")));
        assert_eq!(from_value::<Day>(json!("2028-02-29")).unwrap().0, NaiveDate::from_ymd_opt(2028, 2, 29).unwrap());
        for bad in ["", "2026-02-30", "2026-1-5", "13/10/2026", "0026-10-13", "2026-10-13T09:00"] {
            assert_eq!(Day::parse(bad), None, "{bad:?}");
        }
        let time = ClockTime::parse("07:05").unwrap();
        assert_eq!((time.to_string(), json!(time)), ("07:05".to_string(), json!("07:05")));
        assert!(from_value::<ClockTime>(json!("23:59")).is_ok());
        for bad in ["", "7:05", "24:00", "07:60", "07:05:00", "noon!"] {
            assert_eq!(ClockTime::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn repeats_are_named_in_lower_case() {
        assert_eq!(from_value::<Repeat>(json!("yearly")).unwrap(), Repeat::Yearly);
        assert_eq!(json!(Repeat::Never), json!("never"));
        assert_eq!(Repeat::default(), Repeat::Never);
        assert!(from_value::<Repeat>(json!("hourly")).is_err());
    }

    #[test]
    fn the_screen_and_the_slideshow_have_safe_defaults() {
        assert_eq!(Screen::default(), Screen::Legacy, "the frame keeps its own screen until told otherwise");
        assert_eq!(from_value::<Screen>(json!("new")).unwrap(), Screen::New);
        assert_eq!(json!(Screen::Legacy), json!("legacy"));
        assert!(from_value::<Screen>(json!("shiny")).is_err());
        let usual = json!({ "every": 15, "order": "shuffled", "memories": true, "morning": true, "weather": true, "home_after": 5, "show": "all" });
        assert_eq!(json!(Slideshow::default()), usual);
        let chosen = from_value::<Slideshow>(json!({ "every": 60, "order": "newest", "memories": false })).unwrap();
        let expected =
            Slideshow { every: Pace::new(60).unwrap(), order: Order::Newest, memories: false, ..Slideshow::default() };
        assert_eq!(chosen, expected);
        assert_eq!(from_value::<Slideshow>(json!({})).unwrap(), Slideshow::default());
        assert_eq!(from_value::<Order>(json!("recent")).unwrap(), Order::Recent);
        assert_eq!(
            json!(from_value::<Slideshow>(json!({ "home_after": null })).unwrap())["home_after"],
            json!(null),
            "never"
        );
        assert_eq!(json!(from_value::<Rest>(json!(30)).unwrap()), json!(30));
        for bad in [json!(0), json!(1441), json!(-1), json!("soon")] {
            assert!(from_value::<Rest>(bad.clone()).is_err(), "{bad}");
        }
        for bad in [json!(4), json!(3601), json!(-5), json!("15"), json!(99_999_999)] {
            let error = from_value::<Pace>(bad.clone()).unwrap_err().to_string();
            assert!(error.contains("5 to 3600 seconds") || error.contains("invalid"), "{bad}: {error}");
        }
        assert_eq!((Pace::new(5), Pace::new(4), Pace::new(3600).is_some()), (Some(Pace(5)), None, true));
    }

    #[test]
    fn a_night_may_run_past_midnight() {
        let at = |text: &str| ClockTime::parse(text).unwrap();
        let night = Night::default();
        assert_eq!((night.on, night.from, night.until, night.when_dark), (false, at("23:00"), at("07:00"), false));
        for (time, dark) in
            [("22:59", false), ("23:00", true), ("03:00", true), ("06:59", true), ("07:00", false), ("12:00", false)]
        {
            assert_eq!(night.covers(at(time).0), dark, "{time}");
        }
        let nap = Night { from: at("13:00"), until: at("14:30"), ..night };
        assert!(nap.covers(at("13:30").0) && !nap.covers(at("14:30").0) && !nap.covers(at("23:30").0));
        let none = Night { from: at("09:00"), until: at("09:00"), ..night };
        assert!(!none.covers(at("09:00").0) && !none.covers(at("21:00").0));
        assert_eq!(from_value::<Night>(json!({})).unwrap(), night, "anything left out takes its usual value");
        let set =
            from_value::<Night>(json!({ "on": true, "from": "22:30", "until": "06:00", "when_dark": true })).unwrap();
        assert_eq!(json!(set), json!({ "on": true, "from": "22:30", "until": "06:00", "when_dark": true }));
        assert!(from_value::<Night>(json!({ "from": "late" })).is_err());
    }

    #[test]
    fn a_chime_is_off_or_some_minutes_ahead() {
        assert_eq!(from_value::<Chime>(json!(null)).unwrap(), Chime::default());
        assert_eq!(
            json!(from_value::<Chime>(json!(0)).unwrap()),
            json!(0),
            "at the time itself is not the same as off"
        );
        assert_eq!(json!(from_value::<Chime>(json!(120)).unwrap()), json!(120));
        assert_eq!(json!(from_value::<Chime>(json!(10)).unwrap()), json!(10));
        assert_eq!(json!(Chime::default()), json!(null));
        assert!(from_value::<Chime>(json!(121)).unwrap_err().to_string().contains("at most 120 minutes"));
        assert!(from_value::<Chime>(json!(-5)).is_err() && from_value::<Chime>(json!("soon")).is_err());
    }

    #[test]
    fn a_place_has_a_name_and_is_on_the_globe() {
        let lisbon = Place::new(" Lisbon ", 38.72, -9.14).unwrap();
        assert_eq!(json!(lisbon), json!({ "name": "Lisbon", "latitude": 38.72, "longitude": -9.14 }));
        assert_eq!(from_value::<Place>(json!(lisbon)).unwrap(), lisbon);
        for (name, latitude, longitude) in
            [("", 0.0, 0.0), ("Nowhere", 91.0, 0.0), ("Nowhere", 0.0, -181.0), ("Nowhere", f64::NAN, 0.0)]
        {
            assert_eq!(Place::new(name, latitude, longitude), None, "{name} {latitude} {longitude}");
        }
        assert!(from_value::<Place>(json!({ "name": "x", "latitude": 99, "longitude": 0 })).is_err());
        assert_eq!(from_value::<PlaceQuery>(json!("  Porto ")).unwrap().as_str(), "Porto");
        for bad in [json!(""), json!("   "), json!("x".repeat(101)), json!(5)] {
            assert!(from_value::<PlaceQuery>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_names_and_states_of_the_house_are_checked_where_they_arrive() {
        assert_eq!(json!(Label::parse("  Front Door ").unwrap()), json!("Front Door"));
        for bad in ["", "   ", "two\nlines", &"x".repeat(61)] {
            assert_eq!(Label::parse(bad), None, "{bad:?}");
        }
        assert!(from_value::<Label>(json!("")).is_err());
        assert_eq!(json!(CameraId::parse("side_passage").unwrap()), json!("side_passage"));
        for bad in ["", "Side Passage", "../etc", "a-b", &"x".repeat(65)] {
            assert_eq!(CameraId::parse(bad), None, "{bad:?}");
        }
        assert_eq!(from_value::<AlarmState>(json!("armed_away")).unwrap(), AlarmState::ArmedAway);
        assert_eq!(from_value::<AlarmState>(json!("something_new")).unwrap(), AlarmState::Unknown);
        assert_eq!(json!(AlarmState::Disarmed), json!("disarmed"));
        assert_eq!(from_value::<SensorKind>(json!("door")).unwrap(), SensorKind::Door);
        assert!(from_value::<SensorKind>(json!("smoke")).is_err());
        assert!(from_value::<Bounded<u8, 0, 2>>(json!([1, 2, 3])).is_err());
        assert!(Bounded::<u8, 0, 2>::default().is_empty());
    }

    #[test]
    fn a_home_key_is_32_hex_digits_and_never_printed() {
        let key = HomeKey::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert!(key.matches("0123456789abcdef0123456789abcdef"));
        assert!(!key.matches("0123456789abcdef0123456789abcdee"));
        assert!(!key.matches("") && !key.matches("0123"));
        assert_eq!(format!("{key:?}"), "HomeKey(****)");
        for bad in ["", "0123", "0123456789ABCDEF0123456789ABCDEF", "0123456789abcdef0123456789abcdeg"] {
            assert!(HomeKey::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_vigil_asks_for_the_display_within_reason() {
        use std::time::Duration;
        assert_eq!(Vigil::from_query(None, None), Vigil::default());
        let asked = Vigil::from_query(Some("600"), Some("1800000000"));
        assert_eq!(asked, Vigil { stay_on: Some(Duration::from_mins(10)), wake_at: Some(1_800_000_000) });
        assert_eq!(Vigil::from_query(Some("999999"), None).stay_on, Some(Duration::from_hours(4)), "capped");
        for junk in ["", "0", "-5", "soon", "1.5"] {
            assert_eq!(Vigil::from_query(Some(junk), Some(junk)), Vigil::default(), "{junk:?}");
        }
    }

    #[test]
    fn a_calendar_window_is_never_backwards_or_endless() {
        let now = 1_800_000_000;
        assert_eq!(Window::from_query(None, None, now), Window { from: now - 86_400, to: now + 34 * 86_400 });
        assert_eq!(
            Window::from_query(Some("100"), Some("50"), now).to,
            Window::from_query(Some("100"), None, now).from
        );
        let wide = Window::from_query(Some("1800000000"), Some("9999999999999"), now);
        assert_eq!(wide.to - wide.from, 400 * 86_400);
        assert_eq!(Window::from_query(Some("junk"), Some("junk"), now), Window::from_query(None, None, now));
        assert!(Window::from_query(Some("-99999999999999"), None, now).from > 0, "a silly start is brought into range");
    }

    #[test]
    fn photo_paths_accept_real_photo_locations() {
        for rel in [
            "frame-sync/fs_0123456789abcdef.jpg",
            "123user/photos/9/8/7/2026/06/1782161827915211.webp",
            "frame-sync/up_00aa00aa00aa00aa.jpg",
            "a/b/c.PNG",
            "x.jpeg",
        ] {
            assert_eq!(PhotoPath::parse(rel).map(|p| p.to_string()).as_deref(), Some(rel));
        }
    }

    #[test]
    fn photo_paths_reject_anything_that_could_escape_or_inject() {
        for rel in [
            "",
            "../x.jpg",
            "a/../../x.jpg",
            "/etc/passwd.jpg",
            ".downloading/x.jpg",
            "-rf.jpg",
            "a b.jpg",
            "a';rm -rf x;'.jpg",
            "a\"b.jpg",
            "a$(id).jpg",
            "a\n.jpg",
            "frame-sync/notes.txt",
            "frame-sync/x.jpg.md5",
            "frame-sync/fs_0123456789abcdef",
        ] {
            assert_eq!(PhotoPath::parse(rel), None, "{rel:?}");
        }
    }

    #[test]
    fn a_photo_path_knows_where_the_photo_came_from() {
        let source = |rel: &str| PhotoPath::parse(rel).unwrap().source();
        assert_eq!(source("frame-sync/fs_0123456789abcdef.jpg"), Source::Synced);
        assert_eq!(source("frame-sync/up_0123456789abcdef.jpg"), Source::Added);
        assert_eq!(source("frame-sync/holiday.jpg"), Source::Added);
        assert_eq!(source("123user/photos/a.webp"), Source::Phone);
        assert_eq!(source("fs_0123456789abcdef.jpg"), Source::Phone, "right name, wrong folder");
        assert_eq!(Source::ALL.map(Source::as_str), ["synced", "added", "phone"]);
        assert_eq!(json!(Source::Synced), json!("synced"));
    }

    #[test]
    fn a_photo_path_gives_its_parts() {
        let path = PhotoPath::parse("frame-sync/fs_0123456789abcdef.jpg").unwrap();
        assert_eq!(path.synced_name(), SyncedName::parse("fs_0123456789abcdef.jpg"));
        assert_eq!(path.within(Path::new("/photos")), Path::new("/photos/frame-sync/fs_0123456789abcdef.jpg"));
        assert_eq!(PhotoPath::parse("frame-sync/up_0123456789abcdef.jpg").unwrap().synced_name(), None);
        assert_eq!(PhotoPath::parse("other/fs_0123456789abcdef.jpg").unwrap().synced_name(), None);
    }

    #[test]
    fn synced_names_are_exactly_fs_plus_16_lowercase_hex() {
        assert!(SyncedName::parse("fs_0123456789abcdef.jpg").is_some());
        for name in [
            "fs_0123456789abcde.jpg",
            "fs_0123456789abcdef0.jpg",
            "fs_0123456789ABCDEF.jpg",
            "fs_0123456789abcdeg.jpg",
            "up_0123456789abcdef.jpg",
            "fs_0123456789abcdef.png",
            "../fs_0123456789abcdef.jpg",
            "",
        ] {
            assert_eq!(SyncedName::parse(name), None, "{name}");
        }
    }

    #[test]
    fn names_made_from_a_digest_are_always_valid() {
        let digest: Digest = [0xab; 20];
        let name = SyncedName::from_digest(&digest);
        assert_eq!(name.as_str(), "fs_abababababababab.jpg");
        assert_eq!(SyncedName::parse(name.as_str()), Some(name.clone()));
        assert_eq!(name.path().as_str(), "frame-sync/fs_abababababababab.jpg");
        assert_eq!(PhotoPath::parse(name.path().as_str()), Some(name.path()));
        let added = PhotoPath::added(&digest);
        assert_eq!(added.as_str(), "frame-sync/up_abababababababab.jpg");
        assert_eq!((added.source(), PhotoPath::parse(added.as_str())), (Source::Added, Some(added.clone())));
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn paths_and_names_parse_out_of_json_or_fail_with_a_reason() {
        assert_eq!(from_value::<PhotoPath>(json!("a/b.jpg")).unwrap().as_str(), "a/b.jpg");
        let error = from_value::<PhotoPath>(json!("../b.jpg")).unwrap_err().to_string();
        assert!(error.contains("not a photo path"), "{error}");
        assert!(from_value::<PhotoPath>(json!(5)).is_err());
        assert!(from_value::<SyncedName>(json!("fs_0123456789abcdef.jpg")).is_ok());
        assert!(from_value::<SyncedName>(json!("holiday.jpg")).is_err());
    }

    #[test]
    fn bounded_lists_enforce_their_length() {
        type OneToThree = Bounded<u8, 1, 3>;
        assert!(OneToThree::new(vec![]).is_none());
        assert_eq!(OneToThree::new(vec![1, 2]).unwrap().into_inner(), [1, 2]);
        assert!(OneToThree::new(vec![1, 2, 3, 4]).is_none());
        assert_eq!(from_value::<OneToThree>(json!([7])).unwrap().len(), 1);
        assert!(from_value::<OneToThree>(json!([])).unwrap_err().to_string().contains("expected 1 to 3 items, got 0"));
        assert!(from_value::<NonEmpty<u8>>(json!([])).unwrap_err().to_string().contains("at least 1"));
        assert!(from_value::<Bounded<u8, 0, 2>>(json!([])).is_ok());
        assert!(from_value::<NonEmpty<PhotoPath>>(json!(["ok.jpg", "../bad.jpg"])).is_err(), "items are parsed too");
    }

    #[test]
    fn pins_are_four_to_eight_digits() {
        for pin in ["1234", "00000000", "987654"] {
            assert!(Pin::parse(pin).is_some(), "{pin}");
        }
        for pin in ["", "123", "123456789", "12a4", "12 34", "١٢٣٤", "-123"] {
            assert!(Pin::parse(pin).is_none(), "{pin:?}");
        }
        assert_eq!(format!("{:?}", Pin::parse("2468").unwrap()), "Pin(****)", "never printed");
    }

    #[test]
    fn a_pin_change_is_a_new_pin_or_a_removal() {
        assert_eq!(from_value::<PinChange>(json!("")).unwrap(), PinChange::Remove);
        assert_eq!(from_value::<PinChange>(json!("2468")).unwrap(), PinChange::Set(Pin::parse("2468").unwrap()));
        assert!(from_value::<PinChange>(json!("12")).unwrap_err().to_string().contains("A PIN is 4 to 8 digits."));
        assert!(from_value::<PinChange>(json!(2468)).is_err());
    }

    #[test]
    fn album_links_are_trimmed_https_addresses() {
        assert_eq!(
            AlbumLink::parse(" https://photos.app.goo.gl/abc ").unwrap().as_str(),
            "https://photos.app.goo.gl/abc"
        );
        for link in ["", "https://", "http://insecure", "/home/me/Pictures", "file:///etc", "https://a b"] {
            assert_eq!(AlbumLink::parse(link), None, "{link:?}");
        }
        let links = from_value::<AlbumLinks>(json!([" https://a ", "", "  ", "https://b"])).unwrap();
        assert_eq!(links.0.iter().map(AlbumLink::as_str).collect::<Vec<_>>(), ["https://a", "https://b"]);
        assert_eq!(from_value::<AlbumLinks>(json!([])).unwrap(), AlbumLinks::default());
        assert!(from_value::<AlbumLinks>(json!(["/etc"]))
            .unwrap_err()
            .to_string()
            .contains("must start with https://"));
        assert!(from_value::<AlbumLinks>(json!("https://a")).is_err());
        assert!(from_value::<AlbumLinks>(json!([1])).is_err());
    }

    #[test]
    fn restore_is_all_or_named_photos() {
        assert_eq!(from_value::<Restore>(json!("all")).unwrap(), Restore::All);
        let named = from_value::<Restore>(json!(["fs_0123456789abcdef.jpg"])).unwrap();
        assert_eq!(named, Restore::These(vec![SyncedName::parse("fs_0123456789abcdef.jpg").unwrap()]));
        assert_eq!(from_value::<Restore>(json!([])).unwrap(), Restore::These(vec![]));
        for bad in [json!("some"), json!(["holiday.jpg"]), json!(5), json!(null)] {
            assert!(from_value::<Restore>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn likeness_comes_from_a_query_parameter_and_is_capped() {
        assert_eq!(Likeness::from_query(None), Likeness::IDENTICAL);
        assert_eq!(Likeness::from_query(Some("3")).bits(), 3);
        assert_eq!(Likeness::from_query(Some("99")).bits(), 10);
        assert_eq!(Likeness::from_query(Some("junk")).bits(), 0);
        assert_eq!(Likeness::from_query(Some("-1")).bits(), 0);
    }

    #[test]
    fn what_the_slideshow_shows_is_all_favourites_or_a_year() {
        assert_eq!(from_value::<Show>(json!("all")).unwrap(), Show::All);
        assert_eq!(from_value::<Show>(json!("favourites")).unwrap(), Show::Favourites);
        let year = from_value::<Show>(json!({ "year": 2023 })).unwrap();
        assert_eq!(json!(year), json!({ "year": 2023 }));
        for bad in [json!({ "year": 23 }), json!({ "year": "2023" }), json!("hidden"), json!({ "album": 1 })] {
            assert!(from_value::<Show>(bad.clone()).is_err(), "{bad}");
        }
        // A slideshow saved before there was a choice shows everything.
        assert_eq!(from_value::<Slideshow>(json!({ "every": 30 })).unwrap().show, Show::All);
    }

    #[test]
    fn a_mark_is_favourite_or_hidden() {
        assert_eq!(from_value::<Mark>(json!("favourite")).unwrap(), Mark::Favourite);
        assert_eq!(json!(Mark::Hidden), json!("hidden"));
        assert!(from_value::<Mark>(json!("loved")).is_err());
    }

    #[test]
    fn reminder_words_are_whole_words_whatever_their_capitals() {
        let words = Words::parse(" Bin ,recycling,, food waste ").unwrap();
        assert_eq!(json!(words), json!("bin, recycling, food waste".replace("bin", "Bin")));
        let found = ["Black bin", "BIN day", "bin", "Recycling (blue box)", "Garden and food waste today", "bin/box"];
        for title in found.into_iter().chain(["Bins out", "Put out the bins!", "Food wastes"]) {
            assert!(words.found_in(title), "{title:?}");
        }
        for title in ["Robin's party", "Cabinet meeting", "Binsy", "Bingo", "Food and waste", ""] {
            assert!(!words.found_in(title), "{title:?}");
        }
        assert!(Words::parse("café").unwrap().found_in("CAFÉ with Ana"));
        assert!(Words::parse("box").unwrap().found_in("Blue boxes"));

        let too_many = ["word"; 9].join(",");
        let too_long = "x".repeat(41);
        for bad in ["", " , ,", too_many.as_str(), too_long.as_str(), "a\u{7}b"] {
            assert_eq!(Words::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_reminder_comes_minutes_before_or_at_a_time_some_days_before() {
        let minutes = from_value::<Ahead>(json!({ "minutes": 30 })).unwrap();
        assert_eq!(minutes, Ahead::Minutes(30));
        assert_eq!(json!(minutes), json!({ "minutes": 30 }));
        let evening = from_value::<Ahead>(json!({ "days": 1, "at": "19:00" })).unwrap();
        assert_eq!(json!(evening), json!({ "days": 1, "at": "19:00" }));
        assert!(from_value::<Ahead>(json!({ "days": 0, "at": "07:30" })).is_ok());
        for bad in [
            json!({}),
            json!({ "minutes": 1441 }),
            json!({ "days": 15, "at": "19:00" }),
            json!({ "days": 1 }),
            json!({ "at": "19:00" }),
            json!({ "minutes": 5, "days": 1, "at": "19:00" }),
            json!({ "days": 1, "at": "7pm" }),
        ] {
            assert!(from_value::<Ahead>(bad.clone()).is_err(), "{bad}");
        }

        let rule = from_value::<Reminder>(json!({ "words": "bin", "when": { "minutes": 0 } })).unwrap();
        assert_eq!((rule.say, rule.chime), (None, false));
        assert!(from_value::<Reminder>(json!({ "words": "", "when": { "minutes": 0 } })).is_err());
        assert!(from_value::<Reminder>(json!({ "words": "bin", "when": { "minutes": 0 }, "say": "" })).is_err());
        let many: Vec<_> = (0..21).map(|_| json!({ "words": "bin", "when": { "minutes": 0 } })).collect();
        assert!(from_value::<Reminders>(json!(many)).is_err());
    }

    #[test]
    fn an_action_is_written_by_what_it_does() {
        let action = |value: serde_json::Value| from_value::<Action>(value);
        assert_eq!(action(json!({ "do": "next" })).unwrap(), Action::Next);
        assert_eq!(action(json!({ "do": "show", "tab": "todo" })).unwrap(), Action::Show { tab: Tab::Todo });
        assert_eq!(action(json!({ "do": "play" })).unwrap(), Action::Play { station: None });
        let play = action(json!({ "do": "play", "station": " Radio Four " })).unwrap();
        assert_eq!(play.station().map(Label::as_str), Some("Radio Four"));
        assert_eq!(serde_json::to_value(&play).unwrap(), json!({ "do": "play", "station": "Radio Four" }));
        assert_eq!(serde_json::to_value(Action::Quieter).unwrap(), json!({ "do": "quieter" }));
        assert_eq!(action(json!({ "do": "volume", "to": 100 })).unwrap(), Action::Volume { to: Loudness(100) });
        assert_eq!(action(json!({ "do": "timer", "minutes": 10 })).unwrap(), Action::Timer { minutes: Minutes(10) });
        assert!(action(json!({ "do": "note", "say": "Dinner is ready" })).is_ok());
        let cheer = Cheer { name: "Ana".into(), what: "Kind words".into(), points: 1, has: 3, goal: None };
        assert_eq!(
            serde_json::to_value(Action::Cheer(cheer)).unwrap(),
            json!({ "do": "cheer", "name": "Ana", "what": "Kind words", "points": 1, "has": 3, "goal": null })
        );
        for bad in [
            json!({}),
            json!({ "do": "explode" }),
            json!({ "do": "cheer", "name": "Ana", "what": "x", "points": 1, "has": 1, "goal": null }),
            json!({ "do": "show" }),
            json!({ "do": "show", "tab": "attic" }),
            json!({ "do": "volume", "to": 101 }),
            json!({ "do": "timer", "minutes": 0 }),
            json!({ "do": "timer", "minutes": 601 }),
            json!({ "do": "play", "station": "" }),
            json!({ "do": "note", "say": "" }),
        ] {
            assert!(action(bad.clone()).is_err(), "{bad}");
        }
        // A reminder rule can carry one, to be done when its note comes up.
        let rule = json!({ "words": "wake", "when": { "minutes": 0 }, "then": { "do": "play", "station": "Jazz" } });
        assert!(from_value::<Reminder>(rule).unwrap().act.is_some());
        let rule = json!({ "words": "wake", "when": { "minutes": 0 }, "then": { "do": "explode" } });
        assert!(from_value::<Reminder>(rule).is_err());
    }

    #[test]
    fn stations_have_a_name_each_and_a_web_address() {
        let link = |text: &str| StreamLink::parse(text);
        assert!(link(" https://stream.example/four.mp3 ").is_some());
        assert!(link("http://stream.example:8000/jazz?x=1").is_some());
        let long = format!("https://a/{}", "x".repeat(500));
        for bad in [
            "",
            "stream.example/four",
            "ftp://stream.example/x",
            "https://",
            "https:///x",
            "https://a b/x",
            "https://a/\"x",
            &long,
        ] {
            assert_eq!(link(bad), None, "{bad:?}");
        }
        let station = |name: &str| json!({ "name": name, "stream": "https://stream.example/x" });
        assert_eq!(from_value::<Stations>(json!([station("Jazz"), station("News")])).unwrap().into_inner().len(), 2);
        assert!(from_value::<Stations>(json!([station("Jazz"), station("jazz")])).is_err(), "asked for by name");
        let many: Vec<_> = (0..25).map(|n| station(&format!("Station {n}"))).collect();
        assert!(from_value::<Stations>(json!(many)).is_err());
    }

    #[test]
    fn what_the_screen_says_of_itself_is_read_leniently() {
        let said = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> =
                pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
            Said::from_query(|name| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
        };
        assert_eq!(said(&[]), Said::default());
        let full = said(&[("tab", "calendar"), ("paused", "1"), ("playing", "Jazz"), ("volume", "40")]);
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            json!({ "tab": "calendar", "paused": true, "playing": "Jazz", "volume": 40 })
        );
        assert_eq!(said(&[("tab", "attic"), ("paused", "yes"), ("playing", ""), ("volume", "loud")]), Said::default());
        assert_eq!(said(&[("volume", "101")]).volume, None);
    }

    #[test]
    fn a_spot_is_a_line_of_text_and_knows_when_it_is_a_postcode() {
        let spot = |text: &str| Spot::parse(text).unwrap();
        assert_eq!(spot("  12 High Street, Orpington ").as_str(), "12 High Street, Orpington");
        for (text, code) in [("br6 0aa", "BR60AA"), ("SE1 9SG", "SE19SG"), ("EC1A1BB", "EC1A1BB"), ("M1 1AE", "M11AE")]
        {
            assert_eq!(spot(text).postcode().as_deref(), Some(code), "{text}");
        }
        for text in ["Orpington station", "12 High Street", "BR6", "123 456", "A", "BR6 0A1", "é9 9éé"] {
            assert_eq!(spot(text).postcode(), None, "{text}");
        }
        let too_long = "x".repeat(121);
        for bad in ["", "   ", "a\nb", too_long.as_str()] {
            assert_eq!(Spot::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_trip_says_how_an_event_is_reached() {
        let trip = from_value::<Trip>(
            json!({ "title": " Dentist ", "place": "12 High St", "to": "BR6 0AA", "how": "transit" }),
        )
        .unwrap();
        assert_eq!((trip.margin, &trip.from, &trip.via), (10, &None, &None));
        assert!(trip.is_for("Dentist", Some("12 High St")));
        assert!(!trip.is_for("Dentist", None) && !trip.is_for("Dentist", Some("Elsewhere")));
        assert!(!trip.is_for("Dentist again", Some("12 High St")));
        assert_eq!(from_value::<Trip>(json!(trip)).unwrap(), trip, "what is saved reads back");
        // Another way of making it, for comparing; not by a station it does not name.
        assert_eq!(trip.by(How::Car).map(|other| (other.how, other.to == trip.to)), Some((How::Car, true)));
        assert_eq!(trip.by(How::Park), None);
        assert_eq!(trip.ways(true), [How::Transit, How::Walk, How::Cycle, How::Car]);
        assert_eq!(trip.ways(false), [How::Transit, How::Walk, How::Cycle]);

        let park = json!({ "title": "Match", "to": "Wembley Stadium", "how": "park", "via": "Orpington station", "margin": 20, "from": "SE1 9SG" });
        let park = from_value::<Trip>(park).unwrap();
        assert_eq!((park.how, park.margin, park.place.as_deref()), (How::Park, 20, None));
        assert!(park.is_for("Match", None));
        assert_eq!(park.ways(true).last(), Some(&How::Park));
        assert_eq!(park.by(How::Walk).unwrap().via, None, "the station is only for driving to");

        for bad in [
            json!({ "title": "x", "to": "BR6 0AA", "how": "park" }),
            json!({ "title": "x", "to": "BR6 0AA", "how": "car", "via": "Orpington station" }),
            json!({ "title": "x", "to": "BR6 0AA", "how": "teleport" }),
            json!({ "title": "x", "to": "", "how": "car" }),
            json!({ "title": "", "to": "BR6 0AA", "how": "car" }),
            json!({ "title": "x", "to": "BR6 0AA", "how": "car", "margin": 121 }),
            json!({ "title": "x", "to": "BR6 0AA", "how": "car", "place": "" }),
            json!({ "title": "x", "how": "car" }),
        ] {
            assert!(from_value::<Trip>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn driving_is_offered_only_with_a_key_which_is_never_printed() {
        assert_eq!(How::available(false), [How::Transit, How::Walk, How::Cycle]);
        assert_eq!(How::available(true), [How::Transit, How::Walk, How::Cycle, How::Car, How::Park]);
        let key = RoadsKey::parse(" AbCdEf0123456789-_xyz ").unwrap();
        assert_eq!(key.as_str(), "AbCdEf0123456789-_xyz");
        assert_eq!(format!("{key:?}"), "RoadsKey(****)");
        for bad in ["", "short", "has spaces in the middle of it", "semi;colon;semi;colon;semi"] {
            assert_eq!(RoadsKey::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_yoto_limiter_key_starts_with_yl_and_is_never_printed() {
        let key = YotoKey::parse(" yl_AbCdEf0123456789-_xyz ").unwrap();
        assert_eq!(key.as_str(), "yl_AbCdEf0123456789-_xyz");
        assert_eq!(format!("{key:?}"), "YotoKey(****)");
        for bad in ["", "yl_", "yl_short", "AbCdEf0123456789xyz", "yl_has spaces in it", "yl_semi;colon;semi"] {
            assert_eq!(YotoKey::parse(bad), None, "{bad:?}");
        }
        assert!(serde_json::from_value::<YotoKey>(json!("nope")).unwrap_err().to_string().contains("yl_"));
    }

    #[test]
    fn names_points_and_days_of_the_week_for_things_to_do() {
        assert_eq!(PersonName::parse("  Ana ").unwrap().as_str(), "Ana");
        let too_long = "x".repeat(31);
        for bad in ["", "  ", too_long.as_str()] {
            assert_eq!(PersonName::parse(bad), None, "{bad:?}");
        }
        assert_eq!(from_value::<Points>(json!(10000)).unwrap().get(), 10_000);
        assert_eq!(Points::default().get(), 0);
        for bad in [json!(10001), json!(-1), json!(1.5), json!("5")] {
            assert!(from_value::<Points>(bad.clone()).is_err(), "{bad}");
        }
        let days = from_value::<Weekdays>(json!([7, 1, 1, 3])).unwrap();
        assert_eq!(json!(days), json!([1, 3, 7]));
        assert!(days.has(chrono::Weekday::Mon) && days.has(chrono::Weekday::Sun) && !days.has(chrono::Weekday::Tue));
        for bad in [json!([]), json!([0]), json!([8]), json!([1, 9])] {
            assert!(from_value::<Weekdays>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_tick_is_for_a_day_about_now() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        for near in ["2026-10-13", "2026-10-14", "2026-10-15"] {
            assert!(Today::parse(Day::parse(near).unwrap(), today).is_some(), "{near}");
        }
        for far in ["2026-10-12", "2026-10-16", "2025-10-14"] {
            assert_eq!(Today::parse(Day::parse(far).unwrap(), today), None, "{far}");
        }
        let now = chrono::Utc::now().date_naive();
        assert_eq!(from_value::<Today>(json!(Day(now).to_string())).unwrap(), Today(Day(now)));
        let long_ago = from_value::<Today>(json!("2000-01-01")).unwrap_err().to_string();
        assert_eq!(long_ago, "that is not today");
    }

    #[test]
    fn points_are_given_or_taken_a_thousand_at_most() {
        for good in [1, -1, 1000, -1000, 250] {
            assert_eq!(from_value::<PointChange>(json!(good)).unwrap().get(), good);
        }
        for bad in [json!(0), json!(1001), json!(-1001), json!(i64::MAX), json!(1.5), json!("5")] {
            assert!(from_value::<PointChange>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_journey_is_planned_for_a_few_months_around_today() {
        let now = 1_800_000_000;
        let day = 24 * 60 * 60;
        for near in [now, now - 2 * day, now + 120 * day] {
            assert_eq!(Arrival::parse(near, now).map(Arrival::get), Some(near));
        }
        for far in [now - 2 * day - 1, now + 120 * day + 1, 0, i64::MIN, i64::MAX] {
            assert_eq!(Arrival::parse(far, now), None, "{far}");
        }
        assert_eq!(Arrival::parse(i64::MAX, i64::MAX).map(Arrival::get), Some(i64::MAX), "no overflow at the edge");
    }

    #[test]
    fn a_cover_picture_is_an_https_address_that_cannot_break_the_page() {
        let cover = CoverLink::parse("https://i.scdn.co/image/ab67616d0000b273").unwrap();
        assert_eq!(json!(cover), json!("https://i.scdn.co/image/ab67616d0000b273"));
        let long = format!("https://i.scdn.co/{}", "a".repeat(300));
        for bad in ["http://i.scdn.co/x.jpg", "https://", "https://a b", "https://a\"onload=\"x", "https://a<b", &long]
        {
            assert_eq!(CoverLink::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_sync_needs_at_least_one_album() {
        let albums = from_value::<AlbumsToSync>(json!(["", " https://photos.app.goo.gl/abc ", "  "])).unwrap();
        assert_eq!(albums.0.len(), 1);
        for nothing in [json!([]), json!(["", "   "])] {
            let error = from_value::<AlbumsToSync>(nothing).unwrap_err().to_string();
            assert!(error.starts_with("add at least one"), "{error}");
        }
        assert!(from_value::<AlbumsToSync>(json!(["photos.app.goo.gl/abc"])).is_err());
    }
}
