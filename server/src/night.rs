//! Night mode: when the frame's screen should be off.
//!
//! The screen goes off for the hours the user chose and, if they asked for it,
//! whenever the room has been dark for a couple of minutes. It never goes off
//! under someone's finger: a frame that was touched in the last few minutes is
//! left on. Deciding is kept apart from doing, so it can be tested without a frame.

use std::time::Duration;

use chrono::NaiveTime;

use crate::device::Light;
use crate::types::Night;

/// Below this the room counts as dark, above the other as lit; in between it stays what it
/// was, so a reading that hovers does not switch the screen back and forth.
const DARK_BELOW: f32 = 4.0;
const LIT_ABOVE: f32 = 12.0;
/// The room must stay dark this long before the screen goes off (a hand over the sensor is not night).
pub const DARK_FOR: Duration = Duration::from_mins(2);
/// A frame touched more recently than this is being used, and stays on.
pub const LEFT_ALONE: Duration = Duration::from_mins(5);

/// A moment on the clock that waiting is measured by.
type Moment = tokio::time::Instant;

/// What has been seen of the room's light.
#[derive(Debug, Default)]
pub struct Room {
    /// When the sensor took the reading last looked at.
    read_at: Option<u64>,
    /// Since when the room has been dark, if it is.
    dark_since: Option<Moment>,
}

impl Room {
    /// Take in a reading. One that is missing, or the same one as before, says nothing new:
    /// the frame's sensor stops measuring at times, and old news must not be taken for night.
    pub fn note(&mut self, light: Option<Light>, now: Moment) {
        let Some(light) = light.filter(|light| self.read_at != Some(light.at)) else { return };
        self.read_at = Some(light.at);
        if light.lux < DARK_BELOW {
            self.dark_since.get_or_insert(now);
        } else if light.lux > LIT_ABOVE {
            self.dark_since = None;
        }
    }

    /// How long the room has been dark, if it is.
    pub fn dark_for(&self, now: Moment) -> Option<Duration> {
        self.dark_since.map(|since| now.saturating_duration_since(since))
    }
}

/// Whether the screen should be off at `time` of day, in a room dark for `dark_for`, on a
/// frame last touched `untouched` ago (None if the frame will not say).
pub fn screen_should_be_off(
    night: &Night,
    time: NaiveTime,
    dark_for: Option<Duration>,
    untouched: Option<Duration>,
) -> bool {
    let in_use = untouched.is_some_and(|idle| idle < LEFT_ALONE);
    let dark = night.when_dark && dark_for.is_some_and(|dark| dark >= DARK_FOR);
    night.on && !in_use && (night.covers(time) || dark)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ClockTime;

    fn at(text: &str) -> NaiveTime {
        ClockTime::parse(text).unwrap().0
    }

    fn night(when_dark: bool) -> Night {
        Night { on: true, when_dark, ..Night::default() } // 23:00 to 07:00
    }

    const LONG: Option<Duration> = Some(Duration::from_secs(3600));

    #[test]
    fn the_screen_is_off_during_the_chosen_hours() {
        assert!(screen_should_be_off(&night(false), at("23:30"), None, LONG));
        assert!(screen_should_be_off(&night(false), at("06:59"), None, LONG));
        assert!(!screen_should_be_off(&night(false), at("07:00"), None, LONG));
        assert!(!screen_should_be_off(&night(false), at("15:00"), None, LONG));
        assert!(
            screen_should_be_off(&night(false), at("23:30"), None, None),
            "a frame that will not say counts as left alone"
        );
    }

    #[test]
    fn night_mode_that_is_off_never_switches_the_screen_off() {
        let off = Night { on: false, when_dark: true, ..Night::default() };
        assert!(!screen_should_be_off(&off, at("03:00"), LONG, LONG));
    }

    #[test]
    fn a_dark_room_switches_the_screen_off_only_if_asked_and_only_after_a_while() {
        assert!(screen_should_be_off(&night(true), at("15:00"), Some(DARK_FOR), LONG));
        assert!(!screen_should_be_off(&night(true), at("15:00"), DARK_FOR.checked_sub(Duration::from_secs(1)), LONG));
        assert!(!screen_should_be_off(&night(true), at("15:00"), None, LONG), "a lit room");
        assert!(!screen_should_be_off(&night(false), at("15:00"), LONG, LONG), "not asked to follow the light");
    }

    #[test]
    fn a_frame_in_use_stays_on() {
        let just_now = LEFT_ALONE.checked_sub(Duration::from_secs(1));
        assert!(!screen_should_be_off(&night(true), at("23:30"), LONG, just_now));
        assert!(screen_should_be_off(&night(true), at("23:30"), LONG, Some(LEFT_ALONE)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_room_is_dark_from_the_first_dark_reading_until_a_lit_one() {
        let mut room = Room::default();
        let start = Moment::now();
        let light = |lux: f32, at: u64| Some(Light { lux, at });
        room.note(light(110.0, 1), start);
        assert_eq!(room.dark_for(start), None);

        room.note(light(1.0, 2), start);
        tokio::time::advance(Duration::from_secs(90)).await;
        room.note(light(0.0, 3), Moment::now());
        assert_eq!(room.dark_for(Moment::now()), Some(Duration::from_secs(90)), "counted from the first dark reading");

        room.note(light(8.0, 4), Moment::now());
        assert!(room.dark_for(Moment::now()).is_some(), "a reading in between changes nothing");
        room.note(light(300.0, 4), Moment::now());
        assert!(room.dark_for(Moment::now()).is_some(), "the same reading again is not news");
        room.note(None, Moment::now());
        assert!(room.dark_for(Moment::now()).is_some(), "nor is no reading");
        room.note(light(300.0, 5), Moment::now());
        assert_eq!(room.dark_for(Moment::now()), None);
    }
}
