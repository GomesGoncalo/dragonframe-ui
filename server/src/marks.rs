//! What has been said about photos on the frame's own screen or the page: which are
//! favourites, and which the frame should not show.
//!
//! A hidden photo is not deleted: it stays on the frame, out of the slideshow, until it is
//! shown again. Marks are kept in `marks.json`, by photo.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::debug;

use crate::store::{self, Config};
use crate::types::{Mark, PhotoPath};

pub struct Marks {
    path: PathBuf,
    marked: Mutex<BTreeMap<PhotoPath, Mark>>,
}

impl Marks {
    /// The marks kept so far. A file that cannot be read, or an entry in it that is not a
    /// mark on a photo, counts for nothing.
    pub fn load(cfg: &Config) -> Marks {
        let path = cfg.data_dir.join("marks.json");
        let saved: BTreeMap<String, serde_json::Value> =
            std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        let marked = saved
            .into_iter()
            .filter_map(|(photo, mark)| Some((PhotoPath::parse(&photo)?, serde_json::from_value(mark).ok()?)))
            .collect();
        Marks { path, marked: Mutex::new(marked) }
    }

    fn marked(&self) -> std::sync::MutexGuard<'_, BTreeMap<PhotoPath, Mark>> {
        self.marked.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every mark, for looking many photos up at once.
    pub fn all(&self) -> BTreeMap<PhotoPath, Mark> {
        self.marked().clone()
    }

    /// Mark a photo, or with `None` take its mark away. Blocking: the marks are saved.
    pub fn set(&self, photo: &PhotoPath, mark: Option<Mark>) {
        let mut marked = self.marked();
        let before = match mark {
            Some(mark) => marked.insert(photo.clone(), mark),
            None => marked.remove(photo),
        };
        if before != mark {
            self.save(&marked);
        }
    }

    /// Photos that were deleted have no marks to keep. Blocking.
    pub fn forget(&self, photos: &[PhotoPath]) {
        let mut marked = self.marked();
        let before = marked.len();
        for photo in photos {
            marked.remove(photo);
        }
        if marked.len() != before {
            self.save(&marked);
        }
    }

    fn save(&self, marked: &BTreeMap<PhotoPath, Mark>) {
        if let Err(e) = store::write_atomically(&self.path, serde_json::json!(marked).to_string().as_bytes()) {
            debug!(error = %e, "could not save the photo marks");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempFrame;

    fn photo(name: &str) -> PhotoPath {
        PhotoPath::parse(name).unwrap()
    }

    #[test]
    fn marks_are_kept_changed_and_taken_away() {
        let frame = TempFrame::new("marks");
        let marks = Marks::load(&frame.0);
        let (a, b) = (photo("frame-sync/a.jpg"), photo("b.jpg"));
        assert!(marks.all().is_empty());

        marks.set(&a, Some(Mark::Favourite));
        marks.set(&b, Some(Mark::Hidden));
        marks.set(&a, Some(Mark::Hidden));
        assert_eq!(marks.all().get(&a), Some(&Mark::Hidden), "a photo has one mark, the latest");

        // They are there again after a restart.
        let again = Marks::load(&frame.0);
        assert_eq!(again.all().len(), 2);
        again.set(&a, None);
        again.forget(std::slice::from_ref(&b));
        assert!(Marks::load(&frame.0).all().is_empty());
    }

    #[test]
    fn a_damaged_file_costs_only_what_is_damaged() {
        let frame = TempFrame::new("marks-damaged");
        let path = frame.0.data_dir.join("marks.json");
        std::fs::write(&path, r#"{"a.jpg":"favourite","../etc/passwd":"hidden","b.jpg":"loved"}"#).unwrap();
        let marks = Marks::load(&frame.0);
        assert_eq!(marks.all().into_iter().collect::<Vec<_>>(), [(photo("a.jpg"), Mark::Favourite)]);

        std::fs::write(&path, "not json").unwrap();
        assert!(Marks::load(&frame.0).all().is_empty());
    }
}
