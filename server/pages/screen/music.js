// Music on the frame's screen: radio stations and Spotify, and the strip over the pictures
// that says what is playing.
//
// Radio. The stations are kept by the server and chosen on the phone or computer page
// (where a station can be looked up by name); this page plays one from the station's own
// address. Which one is playing, and how loudly, is this browser's own affair and is kept
// in its storage, so a station that was playing when the page was loaded again carries on.
//
// Spotify. When Spotify is switched on (phone or computer page) the frame can be chosen as
// a speaker in anyone's Spotify app. What the app then plays reaches this page as sound
// from the frame's own server (/api/spotify/stream) and is played like a station; the
// server says what the track is, and takes the orders of the buttons here (pause, next).
//
// One thing plays at a time: starting a station hands Spotify back to the app it came
// from, and Spotify starting stops the station.
//
// There is one volume. Spotify's sound arrives at full strength and is turned down here,
// like a station's. While the frame is chosen in a Spotify app, the volume set there
// (the phone's volume buttons) is taken up here, and one set here is told to the app.
//
// That volume is the music's alone. Chimes (reminders, timers, the doorbell) have their own
// loudness, and while one sounds the music is turned down to let it through.
//
// Loaded after utilities.js, whose `kept` it uses; actions.js is how anything else (the
// phone page, a key, a reminder rule) plays and stops the music.

// A stream that stops is tried again this often, this many times, before it is given up.
const RETRY_STREAM_MS = 5000, STREAM_TRIES = 6;
// A stream from which nothing has been heard this long after it was asked for counts as stopped.
const TUNE_MS = 20 * 1000;
const VOLUME_STEP = 10;
// How much of its volume the music keeps while a chime sounds.
const DUCKED = 0.25;
// After Spotify was told to stop, word that it is still playing is not believed for this long.
const SPOTIFY_HUSH_MS = 5000;
// For this long after the volume was set here, what Spotify says the volume is may be the
// one from before, on its way; after it, Spotify is believed again.
const VOLUME_SETTLES_MS = 2500;
const SPOTIFY_SAYS = {
  waiting: 'Spotify  ·  choose “Frame” as the speaker in the Spotify app to play here',
  idle: 'Spotify  ·  connected; play something in the Spotify app',
  playing: 'Spotify  ·  playing', paused: 'Spotify  ·  paused',
};

let stations = [];                              // [{ name, stream }], from the server
let playing = kept.read('framePlaying', null);  // the name of the station that is on, or null
let lastPlayed = kept.read('frameStation', null);
let volume = kept.read('frameVolume', 60);      // 0 to 100
let hearing = false;                            // sound is actually coming out
let musicNote = '';                             // what went wrong, if something did
let streamTries = 0, streamTimer = 0;
let musicDrawn = '';
// How Spotify stands, as the server last said: { installed, stage, track, version }.
let spotify = { installed: false, stage: 'off', track: null, version: null };
let fromSpotify = false;                        // what is playing here is Spotify's sound
let spotifyHushed = 0;
let volumeSetAt = 0, volumeTimer = 0;
let ducked = false, duckTimer = 0;
const player = new Audio();
player.preload = 'none';

/// Make the music as loud as it is set to be, or less while a chime sounds.
function setLoudness() {
  player.volume = volume / 100 * (ducked ? DUCKED : 1);
}
quietMusic = (ms) => {
  ducked = true;
  setLoudness();
  clearTimeout(duckTimer);
  duckTimer = setTimeout(() => { ducked = false; setLoudness(); }, ms);
};

const stationCalled = (name) => stations.filter((s) => s.name.toLowerCase() === String(name).toLowerCase())[0] || null;
/// What is playing, in words, or null: the station, or Spotify's track.
function nowPlaying() {
  if (!fromSpotify) return playing;
  const track = spotify.track;
  return track ? track.title + (track.by ? '  ·  ' + track.by : '') : 'Spotify';
}

/// Start (or start again) the sound of whatever is on: Spotify, or the station.
function tune() {
  clearTimeout(streamTimer);
  const station = playing && stationCalled(playing);
  if (!fromSpotify && !station) return;
  hearing = false;
  setLoudness();
  // Spotify's sound is asked for afresh each time, so nothing old is heard first.
  player.src = fromSpotify ? '/api/spotify/stream?from=' + Date.now() : station.stream;
  const started = player.play();
  // A browser that wants a touch first (not the frame's own) says so here.
  if (started && started.catch) started.catch((error) => { if (error.name === 'NotAllowedError') musicNote = 'Touch the station to start it.'; drawMusic(); });
  streamTimer = setTimeout(streamFailed, TUNE_MS);
  drawMusic();
}

/// Let go of whatever the player holds, which would otherwise keep arriving.
function hush() {
  clearTimeout(streamTimer);
  hearing = false;
  player.pause();
  player.removeAttribute('src');
  player.load();
}

/// Give Spotify's player an order: 'pause', 'next', … or { do: 'volume', to: 40 }.
function orderSpotify(what) {
  send('/api/spotify/do', typeof what === 'string' ? { do: what } : what).catch(() => {});
}

/// Stop listening to Spotify here. `order` is what the player is told: to pause, or (`stop`)
/// to hand the music back to the app, when something else is to play.
function leaveSpotify(order) {
  if (!fromSpotify) return;
  fromSpotify = false;
  spotifyHushed = Date.now() + SPOTIFY_HUSH_MS;
  hush();
  orderSpotify(order);
}

/// Play a station by name; with no name, the one that played last, or the first there is.
function playStation(name) {
  const station = stationCalled(name || lastPlayed || '') || (name ? null : stations[0]);
  if (!station) {
    musicNote = name ? 'There is no station called ' + name + '.' : '';
    return drawMusic();
  }
  leaveSpotify('stop');
  playing = lastPlayed = station.name;
  kept.write('framePlaying', playing);
  kept.write('frameStation', lastPlayed);
  musicNote = '';
  streamTries = 0;
  tune();
  standingChanged();
}

/// Stop whatever is playing: the station, or Spotify (which is paused, to be gone on with).
function stopMusic() {
  leaveSpotify('pause');
  playing = null;
  musicNote = '';
  kept.write('framePlaying', null);
  hush();
  drawMusic();
  standingChanged();
}

/// The station after the one that is on (or that was on last); with Spotify, the next track.
function skipStation() {
  if (fromSpotify) return orderSpotify('next');
  if (!stations.length) return;
  const now = stationCalled(playing || lastPlayed || '');
  playStation(stations[(stations.indexOf(now) + 1) % stations.length].name);
}

/// Play: Spotify if it is there and paused, otherwise a station (see `playStation`).
function playMusic(name) {
  if (!name && !playing && spotify.stage === 'paused') orderSpotify('play');
  else playStation(name);
}

/// Whether someone has chosen the frame in their Spotify app.
const spotifyChosen = (stage) => stage === 'idle' || stage === 'playing' || stage === 'paused';

function applyVolume(to) {
  volume = Math.max(0, Math.min(100, Math.round(to)));
  kept.write('frameVolume', volume);
  setLoudness();
  drawMusic();
  standingChanged();
}

/// Set the volume here, and have the Spotify app show the same.
function setVolume(to) {
  applyVolume(to);
  tellSpotifyVolume();
}

function tellSpotifyVolume() {
  if (!spotifyChosen(spotify.stage)) return;
  volumeSetAt = Date.now();
  orderSpotify({ do: 'volume', to: volume });
  // Should the app end up somewhere else all the same (it was changed there meanwhile), follow it.
  clearTimeout(volumeTimer);
  volumeTimer = setTimeout(followSpotifyVolume, VOLUME_SETTLES_MS + 100);
}

/// Take up the volume the Spotify app shows, unless one set here is still on its way there.
function followSpotifyVolume() {
  const theirs = spotify.volume;
  if (!spotifyChosen(spotify.stage) || typeof theirs !== 'number' || theirs === volume) return;
  if (Date.now() - volumeSetAt < VOLUME_SETTLES_MS) return;
  applyVolume(theirs);
}

/// The stream stopped or would not start: try again a few times, then say so and give up.
function streamFailed() {
  if (!playing && !fromSpotify) return;
  hearing = false;
  if (++streamTries > STREAM_TRIES) {
    const name = fromSpotify ? 'Spotify' : playing;
    stopMusic();
    musicNote = name + ' could not be played. ' + (name === 'Spotify' ? 'Its sound did not arrive.' : 'The station may be off the air, or the frame off Wi-Fi.');
  } else {
    clearTimeout(streamTimer);
    player.pause();
    streamTimer = setTimeout(tune, RETRY_STREAM_MS);
  }
  drawMusic();
}
player.addEventListener('playing', () => { clearTimeout(streamTimer); hearing = true; streamTries = 0; musicNote = ''; drawMusic(); });
// The sound has run dry. It usually comes back by itself; if it does not, start the stream again.
player.addEventListener('waiting', () => {
  if (!playing && !fromSpotify) return;
  hearing = false;
  clearTimeout(streamTimer);
  streamTimer = setTimeout(streamFailed, TUNE_MS);
  drawMusic();
});
player.addEventListener('error', streamFailed);
player.addEventListener('ended', streamFailed);  // neither a station nor Spotify's sound ends; the connection did

/// The server said how Spotify stands (with each answer to actions.js's question).
function spotifyNews(now) {
  if (now.version === spotify.version) return;
  const chosenBefore = spotifyChosen(spotify.stage);
  spotify = now;
  // Just chosen in an app: the app takes the frame's volume. After that, each follows the other.
  if (spotifyChosen(now.stage) && !chosenBefore) tellSpotifyVolume();
  else followSpotifyVolume();
  const sounding = now.stage === 'playing';
  if (sounding && !fromSpotify && Date.now() >= spotifyHushed) {
    // Someone started something in their Spotify app: the station gives way.
    playing = null;
    kept.write('framePlaying', null);
    fromSpotify = true;
    musicNote = '';
    streamTries = 0;
    tune();
  } else if (!sounding && fromSpotify) {
    // Paused or taken elsewhere from the app: quiet here at once.
    fromSpotify = false;
    hush();
  }
  drawMusic();
  standingChanged();
}

function drawMusic() {
  const key = JSON.stringify([stations, playing, hearing, volume, musicNote, spotify, fromSpotify]);
  if (key === musicDrawn) return;
  musicDrawn = key;
  const list = $('stations');
  list.textContent = '';
  stations.forEach((station) => {
    const button = element('button', 'press' + (station.name === playing ? ' on' : ''), station.name);
    button.onclick = () => (station.name === playing && hearing ? stopMusic() : playStation(station.name));
    list.appendChild(button);
  });
  $('music-said').textContent = !stations.length ? 'Radio'
    : !playing ? 'Radio  ·  touch a station to play it'
      : hearing ? 'Radio  ·  playing ' + playing : 'Radio  ·  tuning in to ' + playing + '…';
  $('music-note').textContent = musicNote || (stations.length ? ''
    : 'There are no stations yet. Add some from a phone or computer: the address is under Settings, Wi-Fi; then Frame, Radio stations.');
  $('volume').textContent = volume;
  $('music-stop').style.visibility = nowPlaying() ? 'visible' : 'hidden';

  // Spotify: there once it is switched on; its track and buttons once something was played.
  const track = spotify.track, going = spotify.stage === 'playing' || spotify.stage === 'paused';
  $('spotify-row').style.display = spotify.stage === 'off' ? 'none' : '';
  $('spotify-said').textContent = SPOTIFY_SAYS[spotify.stage] || 'Spotify';
  $('spotify-track').style.display = going && track ? '' : 'none';
  $('spotify-title').textContent = track ? track.title : '';
  $('spotify-by').textContent = track ? [track.by, track.album].filter(Boolean).join('  ·  ') : '';
  const cover = $('spotify-cover'), picture = track && track.cover ? track.cover : '';
  if (cover.getAttribute('src') !== picture) {
    if (picture) cover.src = picture; else cover.removeAttribute('src');
  }
  cover.style.visibility = picture ? 'visible' : 'hidden';
  label($('spotify-toggle'), spotify.stage === 'playing' ? 'pause' : 'play', '');

  // Over the pictures: what is on, with Stop and Next while the tabs are up.
  const said = nowPlaying();
  $('playing').style.display = said ? '' : 'none';
  $('playing-name').textContent = said ? '♪  ' + said + (hearing ? '' : '…') : '';
}

stationsNews = (found) => {
  if (JSON.stringify(found) === JSON.stringify(stations)) return;
  const first = !stations.length;
  stations = found;
  // A station that was taken off the list while it played stops; one that was playing when
  // the page was loaded again carries on, now that its address is known.
  if (playing && !stationCalled(playing)) stopMusic();
  else if (playing && first && !fromSpotify) tune();
  drawMusic();
};

$('music-stop').onclick = stopMusic;
$('quieter').onclick = () => setVolume(volume - VOLUME_STEP);
$('louder').onclick = () => setVolume(volume + VOLUME_STEP);
$('spotify-previous').onclick = () => orderSpotify('previous');
$('spotify-next').onclick = () => orderSpotify('next');
$('spotify-toggle').onclick = () => orderSpotify(spotify.stage === 'playing' ? 'pause' : 'play');
// On the pictures a touch means "next photo" or "tabs"; these two keep theirs to themselves.
$('playing-stop').onclick = (event) => { event.stopPropagation(); stopMusic(); touched(); };
$('playing-skip').onclick = (event) => { event.stopPropagation(); skipStation(); touched(); };
drawMusic();
