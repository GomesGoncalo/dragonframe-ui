//! Looking names up on Android.
//!
//! This program is built to run without Android's own libraries, and the library it is
//! built with (musl) looks names up the way other Linux systems do: by asking the servers
//! listed in `/etc/resolv.conf`. Android has no such file, and musl then asks this very
//! machine, on port 53, where on Android nobody answers.
//!
//! So this answers there: it passes each question on to the servers the frame's network
//! gave it (frameserver reads them from Android and hands them over with `--dns`) and
//! passes the answer back. Only this machine can ask.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use log::{info, warn};

const PATIENCE: Duration = Duration::from_secs(4);

/// Answer name lookups on this machine by asking these servers, from now on.
pub fn forward_to(servers: Vec<String>) {
    let asked_here = match UdpSocket::bind(("127.0.0.1", 53)) {
        Ok(socket) => socket,
        Err(e) => {
            warn!("cannot answer name lookups here ({e}); Spotify will not be reached unless something else does");
            return;
        }
    };
    info!("name lookups go to {}", servers.join(", "));
    std::thread::spawn(move || {
        let mut question = [0u8; 1500];
        loop {
            let Ok((length, asker)) = asked_here.recv_from(&mut question) else {
                continue;
            };
            let (question, servers) = (question[..length].to_vec(), servers.clone());
            let Ok(reply_from) = asked_here.try_clone() else {
                continue;
            };
            // Each question gets its own thread, so one slow server does not hold up the rest.
            std::thread::spawn(move || {
                if let Some(answer) = servers.iter().find_map(|server| ask(server, &question)) {
                    let _ = reply_from.send_to(&answer, asker);
                }
            });
        }
    });
}

/// Put a question to one server and wait a little for its answer.
fn ask(server: &str, question: &[u8]) -> Option<Vec<u8>> {
    let server: SocketAddr = format!("{server}:53").parse().ok()?;
    let socket = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    socket.set_read_timeout(Some(PATIENCE)).ok()?;
    socket.send_to(question, server).ok()?;
    let mut answer = [0u8; 4096];
    let (length, from) = socket.recv_from(&mut answer).ok()?;
    (from.ip() == server.ip()).then(|| answer[..length].to_vec())
}
