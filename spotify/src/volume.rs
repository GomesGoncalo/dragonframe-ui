//! The volume, as Spotify counts it (0 to 65535) and as the frame does (0 to 100).
//!
//! Spotify's apps want something to keep the volume they set, so they can show it. This
//! keeps it and does nothing else with it: the sound is not made quieter here.

use std::sync::atomic::{AtomicU16, Ordering};

use librespot::core::Error;
use librespot::playback::mixer::{Mixer, MixerConfig};

/// Keeps the volume that was set, and leaves the sound alone.
pub struct Remembered(AtomicU16);

impl Default for Remembered {
    fn default() -> Self {
        Remembered(AtomicU16::new(u16::MAX / 2))
    }
}

impl Mixer for Remembered {
    fn open(_: MixerConfig) -> Result<Self, Error> {
        Ok(Remembered::default())
    }

    fn volume(&self) -> u16 {
        self.0.load(Ordering::Relaxed)
    }

    fn set_volume(&self, volume: u16) {
        self.0.store(volume, Ordering::Relaxed);
    }
}

/// Spotify's volume as a number from 0 to 100, to the nearest.
pub fn percent(volume: u16) -> u8 {
    u8::try_from((u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)).unwrap_or(100)
}

/// A number from 0 to 100 (anything more counts as 100) as Spotify's volume.
pub fn from_percent(percent: u8) -> u16 {
    u16::try_from(u32::from(percent.min(100)) * u32::from(u16::MAX) / 100).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_volume_the_frame_can_set_comes_back_as_itself() {
        for wanted in 0..=100 {
            assert_eq!(percent(from_percent(wanted)), wanted);
        }
        assert_eq!((from_percent(0), from_percent(100), from_percent(250)), (0, u16::MAX, u16::MAX));
        assert_eq!((percent(0), percent(u16::MAX), percent(u16::MAX / 2)), (0, 100, 50));
    }
}
