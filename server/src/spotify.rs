//! Spotify on the frame: the frame as a speaker that Spotify's own apps can play on.
//!
//! The playing is done by a program of its own, `framespot` (in `spotify/` of this
//! project, built on the open-source librespot), which this runs while the setting is
//! on. Nobody signs in on the frame and nothing of anyone's account is kept on it: the
//! frame shows up in the Spotify app of whoever is on the same network, and the app hands
//! over what is needed when the frame is chosen there.
//!
//! The two talk over the program's pipes. Its sound (plain 16-bit stereo samples at 44.1
//! kHz, at the speed they are to be heard) is passed on to whoever is listening at
//! `/api/spotify/stream`, which is how the frame's screen plays it: the same way it plays
//! a radio station. Its news (`@ {"event":…}` lines) says what is playing; and it is
//! given orders (`play`, `pause`, `next`, …) for the buttons on the screen.
//!
//! The sound arrives at full strength; the screen sets how loud it is. So that the volume
//! in the Spotify app and the one on the frame are the same one, the program says when it
//! is changed in the app (and the screen follows), and is told when it is changed on the
//! frame (and the app follows).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::body::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc, watch, Notify};
use tracing::{debug, info, warn};

use crate::app::blocking;
use crate::store::{self, Config};
use crate::types::Loudness;

/// What the frame is called in the Spotify app.
const NAME: &str = "Frame";
const PROGRAM: &str = "framespot";
/// A program that stopped by itself is started again after this long.
const START_AGAIN_AFTER: Duration = Duration::from_secs(15);
/// Stereo samples of 16 bits: sound is passed on in whole frames of this many bytes, so
/// that whoever starts listening in the middle hears left as left.
const FRAME_BYTES: usize = 4;
/// How many pieces of sound may wait for a listener that has fallen behind (a second or so).
const SOUND_WAITING: usize = 64;
const SAMPLE_RATE: u32 = 44_100;

/// An order for the player, from a button on the screen: `{"do": "pause"}`,
/// `{"do": "volume", "to": 40}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "do", rename_all = "lowercase")]
pub enum Order {
    Play,
    Pause,
    Next,
    Previous,
    /// Hand the music back to the app it came from.
    Stop,
    /// Have the Spotify app show this volume: the one the frame was set to.
    Volume {
        to: Loudness,
    },
}

impl Order {
    fn as_line(self) -> String {
        match self {
            Order::Play => "play\n".into(),
            Order::Pause => "pause\n".into(),
            Order::Next => "next\n".into(),
            Order::Previous => "previous\n".into(),
            Order::Stop => "stop\n".into(),
            Order::Volume { to } => format!("volume {}\n", to.percent()),
        }
    }
}

/// How far things have got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Switched off, or not installed.
    #[default]
    Off,
    /// The frame can be chosen in Spotify, and nobody has.
    Waiting,
    /// Someone chose it; nothing is playing.
    Idle,
    Playing,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Track {
    pub title: String,
    pub by: String,
    pub album: String,
    /// The address of a picture of the cover.
    pub cover: Option<String>,
}

/// What the program says. Anything else it may come to say is ignored.
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum News {
    Ready,
    Chosen,
    LetGo,
    Track {
        title: String,
        #[serde(default)]
        by: String,
        #[serde(default)]
        album: String,
        #[serde(default)]
        cover: Option<String>,
    },
    Playing,
    Paused,
    Stopped,
    /// The volume was set, in the app or from here.
    Volume {
        percent: u64,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Default, PartialEq)]
struct Known {
    stage: Stage,
    track: Option<Track>,
    /// The volume the Spotify app shows, once it has said.
    volume: Option<Loudness>,
}

pub struct Spotify {
    /// Where the program is, if it is installed.
    program: Option<PathBuf>,
    known: Mutex<Known>,
    /// Counts changes to what is known, to wake whoever waits for news.
    version: watch::Sender<u64>,
    sound: broadcast::Sender<Bytes>,
    /// The way to give the running program orders.
    orders: Mutex<Option<mpsc::UnboundedSender<Order>>>,
    /// The setting was changed.
    wanted: Notify,
}

/// One line of text from another program, cut to what a screen can show.
fn line(text: &str, most: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(most).collect::<String>().trim().to_string()
}

/// Where the program is: named by `FRAME_SPOTIFY` (the simulator does), or beside our data.
pub fn installed(cfg: &Config) -> Option<PathBuf> {
    let program = std::env::var_os("FRAME_SPOTIFY").map_or_else(|| cfg.data_dir.join(PROGRAM), PathBuf::from);
    program.is_file().then_some(program)
}

/// The start of a sound file that goes on for ever: what follows are the samples themselves.
pub fn wav_header() -> Bytes {
    const CHANNELS: u16 = 2;
    const BITS: u16 = 16;
    let block = CHANNELS * BITS / 8;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&u32::MAX.to_le_bytes()); // a length nobody knows
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // plain samples
    header.extend_from_slice(&CHANNELS.to_le_bytes());
    header.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    header.extend_from_slice(&(SAMPLE_RATE * u32::from(block)).to_le_bytes());
    header.extend_from_slice(&block.to_le_bytes());
    header.extend_from_slice(&BITS.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&u32::MAX.to_le_bytes());
    Bytes::from(header)
}

impl Spotify {
    pub fn new(program: Option<PathBuf>) -> Spotify {
        Spotify {
            program,
            known: Mutex::default(),
            version: watch::channel(0).0,
            sound: broadcast::channel(SOUND_WAITING).0,
            orders: Mutex::default(),
            wanted: Notify::new(),
        }
    }

    fn known(&self) -> MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Change what is known, and wake whoever waits for news if that changed anything.
    fn learn(&self, change: impl FnOnce(&mut Known)) {
        let changed = {
            let mut known = self.known();
            let before = known.clone();
            change(&mut known);
            before != *known
        };
        if changed {
            self.version.send_modify(|version| *version += 1);
        }
    }

    /// Take in a line the program wrote: news if it is marked as such, the log otherwise.
    fn hear(&self, said: &str) {
        let Some(news) = said.strip_prefix("@ ") else {
            debug!(said, "framespot");
            return;
        };
        let Ok(news) = serde_json::from_str::<News>(news) else {
            debug!(news, "framespot said something unreadable");
            return;
        };
        self.learn(|known| match news {
            News::Ready | News::LetGo => *known = Known { stage: Stage::Waiting, ..Known::default() },
            News::Chosen | News::Stopped => known.stage = Stage::Idle,
            News::Track { title, by, album, cover } => {
                // The picture comes from Spotify's own servers; anything else is not shown.
                let cover =
                    cover.filter(|at| at.starts_with("https://") && at.len() <= 300 && !at.contains(['"', ' ']));
                known.track =
                    Some(Track { title: line(&title, 120), by: line(&by, 120), album: line(&album, 120), cover });
            }
            News::Playing => known.stage = Stage::Playing,
            News::Paused => known.stage = Stage::Paused,
            News::Volume { percent } => known.volume = Loudness::new(percent).or(known.volume),
            News::Other => {}
        });
    }

    /// How things stand, for the pages: whether Spotify can play here at all, how far it
    /// has got, and what is playing.
    pub fn view(&self) -> Value {
        let known = self.known();
        json!({
            "installed": self.program.is_some(),
            "name": NAME,
            "stage": known.stage,
            "track": known.track,
            "volume": known.volume,
            "version": *self.version.borrow(),
        })
    }

    /// Wait until what is known is no longer version `seen`.
    pub async fn news_since(&self, seen: u64) {
        let mut version = self.version.subscribe();
        // The sender lives as long as we do, so this only ends with news.
        let _ = version.wait_for(|now| *now != seen).await;
    }

    /// The sound from now on, in pieces of whole frames.
    pub fn listen(&self) -> broadcast::Receiver<Bytes> {
        self.sound.subscribe()
    }

    /// Give the player an order. False if it is not running.
    pub fn order(&self, order: Order) -> bool {
        let orders = self.orders.lock().unwrap_or_else(PoisonError::into_inner);
        orders.as_ref().is_some_and(|orders| orders.send(order).is_ok())
    }

    /// The setting was changed: look at it again.
    pub fn setting_changed(&self) {
        self.wanted.notify_one();
    }

    /// Keep the program running for as long as the setting is on. Never returns, unless
    /// the program is not installed.
    pub async fn run(self: Arc<Self>, cfg: Arc<Config>) {
        let Some(program) = self.program.clone() else {
            return;
        };
        loop {
            let settings = Arc::clone(&cfg);
            if !blocking(move || store::load_settings(&settings).spotify).await {
                self.wanted.notified().await;
                continue;
            }
            info!(program = %program.display(), "starting Spotify");
            if let Err(e) = self.play_host(&program, &cfg).await {
                warn!(error = %e, "Spotify could not be started");
            }
            *self.orders.lock().unwrap_or_else(PoisonError::into_inner) = None;
            self.learn(|known| *known = Known::default());
            // Switched off: wait to be switched on. Stopped by itself: try again in a while.
            let settings = Arc::clone(&cfg);
            if blocking(move || store::load_settings(&settings).spotify).await {
                let _ = tokio::time::timeout(START_AGAIN_AFTER, self.wanted.notified()).await;
            }
        }
    }

    /// Run the program until it stops or the setting is switched off, passing on its sound
    /// and its news and giving it its orders.
    async fn play_host(&self, program: &Path, cfg: &Arc<Config>) -> std::io::Result<()> {
        let tmp = cfg.data_dir.join("spotify-tmp");
        let _ = tokio::fs::remove_dir_all(&tmp).await; // pieces of tracks from last time
        let mut command = Command::new(program);
        command.args(["--name", NAME, "--tmp"]).arg(&tmp);
        if let Some(servers) = blocking(crate::dns::servers_to_hand_on).await {
            command.args(["--dns", &servers.join(",")]);
        }
        let mut child =
            command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn()?;
        let (mut orders_in, mut sound, said) = match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, BufReader::new(stderr).lines()),
            _ => return Err(std::io::Error::other("the program's pipes could not be opened")),
        };
        let mut said = said;
        let (orders, mut ordered) = mpsc::unbounded_channel();
        *self.orders.lock().unwrap_or_else(PoisonError::into_inner) = Some(orders);

        let mut piece = vec![0u8; 16 * 1024];
        let mut held = 0; // bytes of a frame whose rest has not arrived yet
        loop {
            tokio::select! {
                read = sound.read(&mut piece[held..]) => {
                    let Ok(read @ 1..) = read else {
                        break;
                    };
                    let whole = (held + read) / FRAME_BYTES * FRAME_BYTES;
                    // Nobody listening is not an error: the sound is then simply not heard.
                    let _ = self.sound.send(Bytes::copy_from_slice(&piece[..whole]));
                    piece.copy_within(whole..held + read, 0);
                    held = held + read - whole;
                }
                line = said.next_line() => match line {
                    Ok(Some(line)) => self.hear(&line),
                    _ => break,
                },
                Some(order) = ordered.recv() => {
                    if orders_in.write_all(order.as_line().as_bytes()).await.is_err() {
                        break;
                    }
                }
                () = self.wanted.notified() => {
                    let settings = Arc::clone(cfg);
                    if !blocking(move || store::load_settings(&settings).spotify).await {
                        info!("Spotify was switched off");
                        break;
                    }
                }
                status = child.wait() => {
                    warn!(?status, "Spotify stopped by itself");
                    break;
                }
            }
        }
        let _ = child.kill().await;
        Ok(())
    }
}

#[cfg(test)]
pub mod testing {
    use std::path::PathBuf;

    use crate::store::Config;

    /// Put a stand-in for the program in the data folder and return where it is. It says
    /// it is ready; on `play` it says it was chosen, what the track is and that it plays,
    /// and sends ten bytes of sound; other orders it answers with the matching news.
    pub fn program(cfg: &Config) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let at = cfg.data_dir.join("framespot");
        let script = r#"#!/bin/sh
echo 'the log, not news' >&2
echo '@ {"event":"ready","name":"Frame"}' >&2
while read order; do
  case "$order" in
    play)
      echo '@ {"event":"chosen"}' >&2
      echo '@ {"event":"track","title":"Blue in Green","by":"Miles Davis","album":"Kind of Blue","cover":"https://i.scdn.co/image/abc","length_ms":337000}' >&2
      echo '@ {"event":"playing","position_ms":0}' >&2
      printf '0123456789'
      ;;
    pause) echo '@ {"event":"paused","position_ms":1000}' >&2 ;;
    next) echo '@ {"event":"track","title":"All Blues","cover":"http://elsewhere.example/x.jpg"}' >&2 ;;
    volume\ *) echo "@ {\"event\":\"volume\",\"percent\":${order#volume }}" >&2 ;;
    previous) echo '@ {"event":"something new"}' >&2; echo '@ not json' >&2 ;;
    stop) echo '@ {"event":"let_go"}' >&2 ;;
  esac
done
"#;
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        std::fs::write(&at, script).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempFrame;

    fn config(frame: &TempFrame) -> Arc<Config> {
        Arc::new(Config { cloud_dir: frame.0.cloud_dir.clone(), data_dir: frame.0.data_dir.clone() })
    }

    /// Wait (a few seconds at most) until the stage is the one expected.
    async fn until(spotify: &Spotify, stage: &str) {
        for _ in 0..400 {
            if spotify.view()["stage"] == stage {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never became {stage}: {}", spotify.view());
    }

    fn switch(cfg: &Config, on: bool) {
        let mut settings = store::load_settings(cfg);
        settings.spotify = on;
        store::save_settings(cfg, &settings);
    }

    #[test]
    fn the_sound_is_announced_as_cd_stereo_of_unknown_length() {
        let header = wav_header();
        assert_eq!(header.len(), 44);
        assert_eq!((&header[..4], &header[8..16], &header[36..40]), (&b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..]));
        assert_eq!(u32::from_le_bytes(header[24..28].try_into().unwrap()), 44_100);
        assert_eq!(u32::from_le_bytes(header[28..32].try_into().unwrap()), 176_400, "bytes a second");
        assert_eq!(u16::from_le_bytes(header[22..24].try_into().unwrap()), 2);
    }

    #[tokio::test]
    async fn without_the_program_there_is_no_spotify() {
        let frame = TempFrame::new("spotify-none");
        assert_eq!(installed(&frame.0), None);
        let spotify = Arc::new(Spotify::new(None));
        assert_eq!(
            spotify.view(),
            json!({ "installed": false, "name": "Frame", "stage": "off", "track": null, "volume": null, "version": 0 })
        );
        assert!(!spotify.order(Order::Play));
        // Nothing to keep running: this ends at once.
        tokio::time::timeout(Duration::from_secs(2), Arc::clone(&spotify).run(config(&frame))).await.unwrap();
    }

    #[tokio::test]
    async fn the_program_runs_while_switched_on_and_its_news_and_sound_are_passed_on() {
        let frame = TempFrame::new("spotify-run");
        let spotify = Arc::new(Spotify::new(Some(testing::program(&frame.0))));
        assert_eq!(installed(&frame.0), spotify.program);
        let running = tokio::spawn(Arc::clone(&spotify).run(config(&frame)));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(spotify.view()["stage"], "off", "not until it is switched on");

        switch(&frame.0, true);
        spotify.setting_changed();
        until(&spotify, "waiting").await;

        let mut sound = spotify.listen();
        let seen = spotify.view()["version"].as_u64().unwrap();
        assert!(spotify.order(Order::Play));
        tokio::time::timeout(Duration::from_secs(4), spotify.news_since(seen)).await.expect("news wakes whoever waits");
        until(&spotify, "playing").await;
        assert_eq!(
            spotify.view()["track"],
            json!({ "title": "Blue in Green", "by": "Miles Davis", "album": "Kind of Blue", "cover": "https://i.scdn.co/image/abc" })
        );
        // Ten bytes were sent: two whole frames are passed on, and half a frame is held back.
        let heard = tokio::time::timeout(Duration::from_secs(4), sound.recv()).await.unwrap().unwrap();
        assert_eq!(&heard[..], b"01234567");

        // A volume set on the frame is told to the app, which says it back.
        assert_eq!(spotify.view()["volume"], Value::Null);
        spotify.order(Order::Volume { to: Loudness::new(40).unwrap() });
        spotify.order(Order::Pause);
        until(&spotify, "paused").await;
        assert_eq!(spotify.view()["volume"], 40);
        spotify.order(Order::Next);
        spotify.order(Order::Previous); // answered with news we do not know: ignored
        for _ in 0..400 {
            if spotify.view()["track"]["title"] == "All Blues" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let track = spotify.view()["track"].clone();
        assert_eq!(
            (&track["title"], &track["cover"]),
            (&json!("All Blues"), &Value::Null),
            "a cover from elsewhere is not shown"
        );
        spotify.order(Order::Stop);
        until(&spotify, "waiting").await;
        assert_eq!((&spotify.view()["track"], &spotify.view()["volume"]), (&Value::Null, &Value::Null));

        switch(&frame.0, false);
        spotify.setting_changed();
        until(&spotify, "off").await;
        assert!(!spotify.order(Order::Play), "there is nothing to give orders to");
        running.abort();
    }
}
