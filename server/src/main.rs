//! The server that runs on the Dragon Touch frame itself.
//!
//! It serves two pages. `/` is for a phone or computer on the same network: see
//! every photo on the frame, add and delete photos, find duplicates, sync Google
//! Photos albums, follow calendars, and set the frame up. `/frame` is the frame's
//! own screen (pictures, calendar, settings), shown full screen by a small app on
//! the frame. The pages' sources are plain html, css and js files in `pages/`.
//!
//! Where things are:
//!
//! - `app`: the state every part shares.
//! - `routes`: one module per area of the HTTP interface (photos, calendars, the
//!   frame, the PIN, the pages themselves).
//! - `tasks`: what the server does by itself in the background.
//! - `store`, `index`, `faces`, `gphotos`: photos on disk, what is known about
//!   them, and syncing albums.
//! - `ical`, `calendar`, `weather`: calendars and the forecast.
//! - `home`: the doorbell, cameras, alarm and sensors, as a relay elsewhere reports them.
//! - `device`, `night`: the frame itself (Wi-Fi, screen, light) and night mode.
//! - `auth`, `types`, `dns`, `plain`: the PIN, values checked where they enter,
//!   and two pieces of networking the frame needs.
//!
//! The server is async (tokio + axum). Work that blocks, such as decoding images,
//! walking the photo folder and running Android commands, lives in plain
//! functions and is run off the async threads through [`app::blocking`].

mod app;
mod auth;
mod calendar;
mod device;
mod dns;
mod faces;
mod gphotos;
mod home;
mod ical;
mod index;
mod marks;
mod night;
mod plain;
mod routes;
mod store;
mod tasks;
mod types;
mod weather;

use std::path::PathBuf;

use tracing::{error, info, warn, Level};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::app::App;
use crate::device::Device;
use crate::plain::{Peer, PlainHttp};
use crate::routes::router;
use crate::store::Config;

/// Log to stderr and to daily files in `<data dir>/logs` (a week is kept).
/// `FRAME_LOG` sets the level: error, warn, info (default), debug or trace.
fn init_logging(cfg: &Config) {
    let level = std::env::var("FRAME_LOG").ok().and_then(|l| l.parse::<Level>().ok()).unwrap_or(Level::INFO);
    let builder = tracing_subscriber::fmt().with_max_level(level).with_ansi(false);
    let files = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("frameserver")
        .filename_suffix("log")
        .max_log_files(7)
        .build(store::log_dir(cfg));
    match files {
        Ok(files) => builder.with_writer(std::io::stderr.and(files)).init(),
        Err(e) => {
            builder.with_writer(std::io::stderr).init();
            warn!(error = %e, "cannot write log files; logging to stderr only");
        }
    }
}

#[tokio::main]
async fn main() {
    let env_path = |name: &str, default: &str| PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.into()));
    let cfg = Config {
        cloud_dir: env_path("FRAME_CLOUD_DIR", "/storage/emulated/0/CloudAlbum/cloud"),
        data_dir: env_path("FRAME_DATA_DIR", "/data/local/frameserver"),
    };
    let port = std::env::var("FRAME_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080u16);
    // Every address by default; FRAME_BIND=127.0.0.1 keeps a copy run on a computer to itself.
    let bind = std::env::var("FRAME_BIND").unwrap_or_else(|_| "0.0.0.0".into());
    if let Err(e) = std::fs::create_dir_all(cfg.data_dir.join("thumbs")) {
        eprintln!("cannot create {}: {e}", cfg.data_dir.display());
        std::process::exit(1);
    }
    init_logging(&cfg);

    let listener = match tokio::net::TcpListener::bind((bind.as_str(), port)).await {
        Ok(listener) => listener,
        Err(e) => {
            error!(port, error = %e, "cannot listen");
            std::process::exit(1);
        }
    };
    let app = App::new(cfg, Device::from_env(), port);
    info!(
        port,
        photos = %app.cfg.cloud_dir.display(),
        data = %app.cfg.data_dir.display(),
        version = env!("CARGO_PKG_VERSION"),
        "frameserver listening"
    );
    tasks::start(&app).await;
    if let Err(e) = axum::serve(PlainHttp(listener), router(app).into_make_service_with_connect_info::<Peer>()).await {
        error!(error = %e, "the server stopped");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
