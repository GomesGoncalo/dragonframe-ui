//! Where the sound goes: standard output, at the speed it is to be heard at.
//!
//! The player hands over sound as fast as it can decode it and relies on whatever plays
//! it to take its time; a sound card does that by itself. A pipe does not, so this
//! counts what it has written and waits whenever it gets ahead of the clock. Without
//! that a whole track would be gone in seconds, and Spotify would be told it had been
//! heard.

use std::io::Write;
use std::process::exit;
use std::time::{Duration, Instant};

use librespot::playback::audio_backend::{Sink, SinkResult};
use librespot::playback::convert::Converter;
use librespot::playback::decoder::AudioPacket;

const SAMPLES_A_SECOND: u64 = 44_100 * 2;
/// The sound may run this far ahead of the clock, so that a late moment here is not heard.
const AHEAD: Duration = Duration::from_millis(400);

pub struct Paced {
    /// When this stretch of sound began, and how many samples of it have gone out.
    began: Option<Instant>,
    written: u64,
}

impl Paced {
    pub fn new() -> Paced {
        Paced { began: None, written: 0 }
    }
}

impl Sink for Paced {
    /// Playing starts or goes on after a pause: the clock starts afresh.
    fn start(&mut self) -> SinkResult<()> {
        self.began = None;
        self.written = 0;
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let Ok(samples) = packet.samples() else {
            return Ok(()); // sound that was not decoded is not ours to play
        };
        let bytes: Vec<u8> = converter.f64_to_s16(samples).iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let mut out = std::io::stdout().lock();
        if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
            exit(0); // nobody is listening any more: frameserver has gone
        }
        drop(out);
        let began = *self.began.get_or_insert_with(Instant::now);
        self.written += samples.len() as u64;
        let due = Duration::from_micros(self.written * 1_000_000 / SAMPLES_A_SECOND);
        if let Some(early) = due.checked_sub(began.elapsed() + AHEAD) {
            std::thread::sleep(early);
        }
        Ok(())
    }
}
