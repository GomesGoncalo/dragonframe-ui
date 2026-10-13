//! framespot: makes the frame a speaker that Spotify's own apps can play on ("Spotify
//! Connect"), using the open-source librespot.
//!
//! It is run by frameserver, never by hand, and talks to it over its three pipes:
//!
//! - **standard output** carries the sound: 16-bit stereo samples at 44.1 kHz, written
//!   at the speed they are to be heard at, and nothing at all while nothing plays;
//! - **standard error** carries lines of text. One that starts with `@ ` is news, as
//!   JSON (`@ {"event":"track","title":"…"}`); the rest is the log;
//! - **standard input** takes commands, one a line: `play`, `pause`, `next`, `previous`,
//!   `stop`, `volume <0 to 100>`. When it closes, frameserver has gone and so does this.
//!
//! The sound always leaves here at full strength. How loud it is heard is the business of
//! whatever plays it (the frame's screen); the volume set in the Spotify app is only
//! passed on as news, and a volume set on the frame is told to the app with `volume`, so
//! that the two show the same.
//!
//! Nobody signs in here and nothing is kept: the frame shows up in the Spotify app of
//! anyone on the same network, and the app hands over what is needed when the frame is
//! chosen there. After a restart it has to be chosen again.
//!
//! Arguments: `--name <what the frame is called in Spotify>`, `--tmp <a folder for the
//! pieces of the track being played>`, and on the frame `--dns <server>[,<server>]`
//! (see `dns`).

mod dns;
mod sink;
mod volume;

use std::path::PathBuf;
use std::pin::Pin;
use std::process::exit;

use futures_util::StreamExt;
use librespot::connect::{ConnectConfig, Spirc};
use librespot::core::authentication::Credentials;
use librespot::core::config::DeviceType;
use librespot::core::{Session, SessionConfig};
use librespot::metadata::audio::{AudioItem, UniqueFields};
use librespot::playback::config::PlayerConfig;
use librespot::playback::mixer::{Mixer, NoOpVolume};
use librespot::playback::player::{Player, PlayerEvent};
use log::{error, info, warn};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncBufReadExt, BufReader};

/// What was asked for on the command line.
struct Asked {
    name: String,
    tmp: PathBuf,
    dns: Vec<String>,
}

fn asked() -> Asked {
    let mut asked = Asked { name: "Frame".to_string(), tmp: std::env::temp_dir(), dns: Vec::new() };
    let mut args = std::env::args().skip(1);
    while let (Some(flag), Some(value)) = (args.next(), args.next()) {
        match flag.as_str() {
            "--name" => asked.name = value,
            "--tmp" => asked.tmp = PathBuf::from(value),
            "--dns" => asked.dns = value.split(',').map(str::to_string).collect(),
            other => {
                eprintln!("framespot: unknown argument {other}");
                exit(2);
            }
        }
    }
    asked
}

/// Tell frameserver something, as a line of JSON marked as news.
fn say(news: &Value) {
    eprintln!("@ {news}");
}

/// What is said about a track when it comes up.
fn track_news(item: &AudioItem) -> Value {
    let (by, album) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => {
            (artists.0.iter().map(|artist| artist.name.as_str()).collect::<Vec<_>>().join(", "), album.clone())
        }
        UniqueFields::Local { artists, album, .. } => {
            (artists.clone().unwrap_or_default(), album.clone().unwrap_or_default())
        }
        UniqueFields::Episode { show_name, .. } => (show_name.clone(), String::new()),
    };
    // The largest picture of the cover there is.
    let cover = item.covers.iter().max_by_key(|cover| cover.width).map(|cover| cover.url.clone());
    json!({ "event": "track", "title": item.name, "by": by, "album": album, "cover": cover, "length_ms": item.duration_ms })
}

/// What the player says, passed on as news. Much of it is of no interest to the frame.
fn player_news(event: PlayerEvent) -> Option<Value> {
    match event {
        PlayerEvent::TrackChanged { audio_item } => Some(track_news(&audio_item)),
        PlayerEvent::Playing { position_ms, .. } => Some(json!({ "event": "playing", "position_ms": position_ms })),
        PlayerEvent::Paused { position_ms, .. } => Some(json!({ "event": "paused", "position_ms": position_ms })),
        PlayerEvent::Stopped { .. } => Some(json!({ "event": "stopped" })),
        // Someone moved to another place in the track, or the count of where it is was put right.
        PlayerEvent::Seeked { position_ms, .. } | PlayerEvent::PositionCorrection { position_ms, .. } => {
            Some(json!({ "event": "position", "position_ms": position_ms }))
        }
        PlayerEvent::VolumeChanged { volume } => Some(json!({ "event": "volume", "percent": volume::percent(volume) })),
        PlayerEvent::SessionConnected { .. } => Some(json!({ "event": "chosen" })),
        PlayerEvent::SessionDisconnected { .. } => Some(json!({ "event": "let_go" })),
        _ => None,
    }
}

/// The same frame is the same device to Spotify every time it starts.
fn device_id(name: &str) -> String {
    Sha1::digest(name.as_bytes()).iter().fold(String::new(), |mut hex, byte| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}"); // writing to a String cannot fail
        hex
    })
}

/// Do what a line from frameserver asks, if someone has chosen the frame in Spotify.
fn obey(command: &str, spirc: Option<&Spirc>) {
    let Some(spirc) = spirc else {
        return;
    };
    let command = command.trim();
    if let Some(percent) = command.strip_prefix("volume ").and_then(|percent| percent.parse::<u8>().ok()) {
        if let Err(e) = spirc.set_volume(volume::from_percent(percent)) {
            warn!("could not set the volume: {e}");
        }
        return;
    }
    let done = match command {
        "play" => spirc.play(),
        "pause" => spirc.pause(),
        "next" => spirc.next(),
        "previous" => spirc.prev(),
        // Hand the music back: the app sees the frame is no longer what it plays on.
        "stop" => spirc.disconnect(true),
        other => {
            warn!("unknown command {other:?}");
            Ok(())
        }
    };
    if let Err(e) = done {
        warn!("could not {command}: {e}");
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .parse_filters("libmdns=warn,librespot=info,framespot=info")
        .format_timestamp(None)
        .init();
    let asked = asked();
    if !asked.dns.is_empty() {
        dns::forward_to(asked.dns.clone());
    }
    if let Err(e) = std::fs::create_dir_all(&asked.tmp) {
        warn!("no folder for the track being played ({}): {e}", asked.tmp.display());
    }

    let session_config =
        SessionConfig { device_id: device_id(&asked.name), tmp_dir: asked.tmp.clone(), ..SessionConfig::default() };
    // The volume the app shows to begin with is soon replaced by the frame's own.
    let connect_config =
        ConnectConfig { name: asked.name.clone(), device_type: DeviceType::Speaker, ..ConnectConfig::default() };
    let launch = librespot::discovery::find(None).unwrap_or_else(|e| {
        error!("no way of being found on the network: {e}");
        exit(1)
    });
    let mut discovery =
        librespot::discovery::Discovery::builder(session_config.device_id.clone(), session_config.client_id.clone())
            .name(asked.name.clone())
            .device_type(DeviceType::Speaker)
            .zeroconf_backend(launch)
            .launch()
            .unwrap_or_else(|e| {
                error!("could not announce the frame on the network: {e}");
                exit(1)
            });
    let mixer: std::sync::Arc<dyn Mixer> = std::sync::Arc::new(volume::Remembered::default());

    let mut session = Session::new(session_config.clone(), None);
    let player =
        Player::new(PlayerConfig::default(), session.clone(), Box::new(NoOpVolume), || Box::new(sink::Paced::new()));
    let mut events = player.get_player_event_channel();
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let mut spirc: Option<Spirc> = None;
    let mut spirc_task: Option<SpircTask> = None;
    info!("waiting to be chosen in Spotify as {:?}", asked.name);
    say(&json!({ "event": "ready", "name": asked.name }));

    loop {
        tokio::select! {
            // Someone chose the frame in their Spotify app, which handed over what it takes to play as them.
            credentials = discovery.next() => {
                let Some(credentials) = credentials else {
                    error!("no longer announced on the network");
                    exit(1);
                };
                if let Some(old) = spirc.take() {
                    let _ = old.shutdown();
                }
                if let Some(task) = spirc_task.take() {
                    task.await;
                }
                if !session.is_invalid() {
                    session.shutdown();
                }
                session = Session::new(session_config.clone(), None);
                player.set_session(session.clone());
                match connect(&connect_config, &session, credentials, &player, &mixer).await {
                    Some((new, task)) => {
                        spirc = Some(new);
                        spirc_task = Some(task);
                    }
                    None => say(&json!({ "event": "let_go" })),
                }
            },
            // The connection to Spotify ended: whoever was playing has to choose the frame again.
            () = async { spirc_task.as_mut().expect("checked by the guard").await }, if spirc_task.is_some() => {
                warn!("the connection to Spotify ended");
                spirc = None;
                spirc_task = None;
                say(&json!({ "event": "let_go" }));
            },
            event = events.recv() => {
                let Some(event) = event else {
                    error!("the player stopped");
                    exit(1);
                };
                if let Some(news) = player_news(event) {
                    say(&news);
                }
            },
            line = commands.next_line() => match line {
                Ok(Some(command)) => obey(&command, spirc.as_ref()),
                // frameserver has gone; so do we.
                _ => break,
            },
        }
    }
    if let Some(spirc) = spirc {
        let _ = spirc.shutdown();
    }
}

type SpircTask = Pin<Box<dyn std::future::Future<Output = ()>>>;

/// Start playing as whoever chose the frame.
async fn connect(
    config: &ConnectConfig,
    session: &Session,
    credentials: Credentials,
    player: &std::sync::Arc<Player>,
    mixer: &std::sync::Arc<dyn Mixer>,
) -> Option<(Spirc, SpircTask)> {
    match Spirc::new(config.clone(), session.clone(), credentials, player.clone(), mixer.clone()).await {
        Ok((spirc, task)) => Some((spirc, Box::pin(task))),
        Err(e) => {
            error!("could not connect to Spotify: {e}");
            None
        }
    }
}
