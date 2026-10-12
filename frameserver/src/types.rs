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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json};

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
}
