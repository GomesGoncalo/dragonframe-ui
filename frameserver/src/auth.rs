//! Optional PIN. Looking at photos is always open; with a PIN set, every
//! change needs a session that was unlocked with it.
//!
//! The PIN and sessions are stored salted and hashed in the settings file.
//! This guards against people on the home network, not against someone who
//! already has root on the frame.

use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::store::{sha1_hex, Protection, Settings};
use crate::types::{hex, Pin};

const MAX_ATTEMPTS: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);
const MAX_SESSIONS: usize = 20;

fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    let from_os = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if from_os.is_err() {
        // No random device: fall back to something unpredictable enough for a home network.
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        return sha1_hex(format!("{nanos}-{}-{:p}", std::process::id(), &bytes).as_bytes())[..32].to_string();
    }
    hex(&bytes)
}

fn digest(salt: &str, secret: &str) -> String {
    sha1_hex(format!("frameserver\0{salt}\0{secret}").as_bytes())
}

impl Protection {
    fn new(pin: &Pin) -> Protection {
        let salt = random_hex();
        Protection { hash: digest(&salt, pin.as_str()), salt, sessions: Vec::new() }
    }

    fn accepts(&self, attempt: &str) -> bool {
        digest(&self.salt, attempt) == self.hash
    }

    /// Start a session and return its token; only a hash of it is kept.
    fn start_session(&mut self) -> String {
        let token = random_hex();
        self.sessions.push(digest("session", &token));
        let excess = self.sessions.len().saturating_sub(MAX_SESSIONS);
        self.sessions.drain(..excess);
        token
    }

    fn has_session(&self, token: &str) -> bool {
        self.sessions.contains(&digest("session", token))
    }
}

/// Set or change the PIN. Everyone else is signed out; the caller gets a fresh session token.
pub fn set_pin(settings: &mut Settings, pin: &Pin) -> String {
    settings.protection.insert(Protection::new(pin)).start_session()
}

/// Remove the PIN, and with it every session.
pub fn clear_pin(settings: &mut Settings) {
    settings.protection = None;
}

/// Whether a request carrying `token` may make changes.
pub fn allowed(settings: &Settings, token: Option<&str>) -> bool {
    match &settings.protection {
        None => true,
        Some(protection) => token.is_some_and(|token| protection.has_session(token)),
    }
}

/// Why an attempt to unlock was turned down.
#[derive(Debug, PartialEq)]
pub enum Denied {
    WrongPin {
        attempts_left: u32,
    },
    TooManyAttempts {
        wait_secs: u64,
    },
    /// There is no PIN to unlock with.
    NoPinSet,
}

/// How guessing is going.
#[derive(Clone, Copy)]
enum Attempts {
    /// This many wrong PINs in a row so far.
    Wrong(u32),
    /// Too many: nobody may try until then.
    LockedUntil(Instant),
}

/// Slows down guessing: after a few wrong PINs, nobody can try for a minute.
pub struct Guard {
    attempts: Mutex<Attempts>,
}

impl Default for Guard {
    fn default() -> Self {
        Guard { attempts: Mutex::new(Attempts::Wrong(0)) }
    }
}

impl Guard {
    /// Check a PIN; on success start a session in `settings` and return its token.
    pub fn unlock(&self, settings: &mut Settings, attempt: &str) -> Result<String, Denied> {
        let protection = settings.protection.as_mut().ok_or(Denied::NoPinSet)?;
        let mut attempts = self.attempts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let wrong_so_far = match *attempts {
            Attempts::LockedUntil(until) => match until.checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => return Err(Denied::TooManyAttempts { wait_secs: left.as_secs() + 1 }),
                _ => 0, // the lockout has run out
            },
            Attempts::Wrong(count) => count,
        };
        if protection.accepts(attempt) {
            *attempts = Attempts::Wrong(0);
            return Ok(protection.start_session());
        }
        let wrong = wrong_so_far + 1;
        if wrong >= MAX_ATTEMPTS {
            *attempts = Attempts::LockedUntil(Instant::now() + LOCKOUT);
            return Err(Denied::TooManyAttempts { wait_secs: LOCKOUT.as_secs() });
        }
        *attempts = Attempts::Wrong(wrong);
        Err(Denied::WrongPin { attempts_left: MAX_ATTEMPTS - wrong })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AlbumLink;

    fn pin(text: &str) -> Pin {
        Pin::parse(text).unwrap()
    }

    #[test]
    fn without_a_pin_everything_is_allowed() {
        let settings = Settings::default();
        assert!(allowed(&settings, None));
        assert!(allowed(&settings, Some("anything")));
    }

    #[test]
    fn with_a_pin_only_a_valid_session_is_allowed() {
        let mut settings = Settings { daily_sync: true, ..Settings::default() };
        let token = set_pin(&mut settings, &pin("2468"));
        assert!(settings.protection.is_some());
        assert!(allowed(&settings, Some(&token)));
        assert!(!allowed(&settings, None));
        assert!(!allowed(&settings, Some("")));
        assert!(!allowed(&settings, Some("not-the-token")));
        assert!(settings.daily_sync, "other settings are untouched");
    }

    #[test]
    fn neither_the_pin_nor_the_session_token_is_stored_in_the_clear() {
        let mut settings = Settings::default();
        let token = set_pin(&mut settings, &pin("2468"));
        let stored = format!("{:?}", settings.protection);
        assert!(!stored.contains("2468"));
        assert!(!stored.contains(&token));
        let mut other = Settings::default();
        set_pin(&mut other, &pin("2468"));
        assert_ne!(
            settings.protection.unwrap().hash,
            other.protection.unwrap().hash,
            "salted: same PIN, different hash"
        );
    }

    #[test]
    fn unlocking_with_the_pin_starts_a_new_session() {
        let mut settings = Settings::default();
        let first = set_pin(&mut settings, &pin("2468"));
        let second = Guard::default().unlock(&mut settings, "2468").unwrap();
        assert_ne!(first, second);
        assert!(allowed(&settings, Some(&first)) && allowed(&settings, Some(&second)));
    }

    #[test]
    fn unlocking_when_no_pin_is_set_says_so() {
        assert_eq!(Guard::default().unlock(&mut Settings::default(), "2468"), Err(Denied::NoPinSet));
    }

    #[test]
    fn changing_the_pin_signs_everyone_else_out() {
        let mut settings = Settings::default();
        let old = set_pin(&mut settings, &pin("2468"));
        let new = set_pin(&mut settings, &pin("1357"));
        assert!(!allowed(&settings, Some(&old)));
        assert!(allowed(&settings, Some(&new)));
        assert_eq!(Guard::default().unlock(&mut settings, "2468"), Err(Denied::WrongPin { attempts_left: 4 }));
    }

    #[test]
    fn removing_the_pin_opens_everything_again() {
        let albums = vec![AlbumLink::parse("https://a").unwrap()];
        let mut settings = Settings { albums: albums.clone(), ..Settings::default() };
        set_pin(&mut settings, &pin("2468"));
        clear_pin(&mut settings);
        assert_eq!(settings, Settings { albums, ..Settings::default() });
        assert!(allowed(&settings, None));
    }

    #[test]
    fn only_the_most_recent_sessions_are_kept() {
        let mut settings = Settings::default();
        let first = set_pin(&mut settings, &pin("2468"));
        let guard = Guard::default();
        let mut last = String::new();
        for _ in 0..MAX_SESSIONS {
            last = guard.unlock(&mut settings, "2468").unwrap();
        }
        assert_eq!(settings.protection.as_ref().unwrap().sessions.len(), MAX_SESSIONS);
        assert!(!allowed(&settings, Some(&first)), "the oldest session was dropped");
        assert!(allowed(&settings, Some(&last)));
    }

    #[test]
    fn repeated_wrong_pins_lock_out_even_the_right_one() {
        let mut settings = Settings::default();
        set_pin(&mut settings, &pin("2468"));
        let guard = Guard::default();
        for left in (1..MAX_ATTEMPTS).rev() {
            assert_eq!(guard.unlock(&mut settings, "0000"), Err(Denied::WrongPin { attempts_left: left }));
        }
        assert_eq!(guard.unlock(&mut settings, "0000"), Err(Denied::TooManyAttempts { wait_secs: 60 }));
        assert!(matches!(guard.unlock(&mut settings, "2468"), Err(Denied::TooManyAttempts { .. })));
    }

    #[test]
    fn guessing_may_resume_once_the_lockout_has_run_out() {
        let mut settings = Settings::default();
        set_pin(&mut settings, &pin("2468"));
        let guard = Guard::default();
        *guard.attempts.lock().unwrap() = Attempts::LockedUntil(Instant::now());
        assert_eq!(guard.unlock(&mut settings, "0000"), Err(Denied::WrongPin { attempts_left: MAX_ATTEMPTS - 1 }));
        *guard.attempts.lock().unwrap() = Attempts::LockedUntil(Instant::now());
        assert!(guard.unlock(&mut settings, "2468").is_ok());
    }

    #[test]
    fn a_correct_pin_resets_the_wrong_attempt_count() {
        let mut settings = Settings::default();
        set_pin(&mut settings, &pin("2468"));
        let guard = Guard::default();
        for _ in 0..MAX_ATTEMPTS - 1 {
            assert!(guard.unlock(&mut settings, "0000").is_err());
        }
        assert!(guard.unlock(&mut settings, "2468").is_ok());
        assert_eq!(guard.unlock(&mut settings, "0000"), Err(Denied::WrongPin { attempts_left: MAX_ATTEMPTS - 1 }));
    }

    #[test]
    fn random_tokens_differ() {
        let (a, b) = (random_hex(), random_hex());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
