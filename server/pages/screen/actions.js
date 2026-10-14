// Things the frame's screen can be told to do, and the ways of telling it.
//
// `act` is the one place where an action is carried out: the next photo, a tab, a timer, a
// radio station, a note. Whatever can be done by touching the screen can be asked for here,
// written the way the server writes it: { do: 'next' }, { do: 'show', tab: 'calendar' },
// { do: 'play', station: 'Jazz' } (see `Action` in the server's types.rs for all of them).
//
// Three things tell it so far:
//   - the server, for the phone or computer page (its remote control): this page asks
//     /api/acts what to do with a request the server holds open until there is something,
//     so an action is here within a moment;
//   - keys, from a keyboard or a remote control (`KEYS` below);
//   - reminder rules that say what to do when their note comes up (extras.js).
//
// Loaded after utilities.js and music.js, whose doings it calls.

// After a request for what to do has failed, the next one waits this long.
const ASK_AGAIN_MS = 5000;
// A note put on the screen stays this long.
const NOTE_MS = 60 * 1000;

function act(action) {
  switch (action.do) {
    case 'next':
    case 'previous':
      if (tab !== 'pictures') showTab('pictures');
      step(action.do === 'next' ? 1 : -1, true);
      break;
    case 'pause': setPaused(true); break;
    case 'resume': setPaused(false); break;
    case 'show': showTab(action.tab); break;
    case 'timer': startTimer(action.minutes); break;
    case 'play': playMusic(action.station || null); break;
    case 'stop': stopMusic(); break;
    case 'skip': skipStation(); break;
    case 'louder': setVolume(volume + VOLUME_STEP); break;
    case 'quieter': setVolume(volume - VOLUME_STEP); break;
    case 'volume': setVolume(action.to); break;
    case 'note':
      $('soon-when').textContent = 'Note';
      $('soon-what').textContent = action.say;
      $('soon').classList.add('on');
      clearTimeout(soonTimer);
      soonTimer = setTimeout(() => $('soon').classList.remove('on'), NOTE_MS);
      if (!asleep) playChime();
      break;
    default:  // something a newer server knows of and this page does not
  }
}

// --- from the server ---------------------------------------------------------------------

let lastAct = null;   // the number of the last action had from the server
let asking = null;    // what can cut short the request that is waiting

/// How the screen stands, said with each request: for the phone page's remote control.
function standing() {
  const parts = ['tab=' + tab, 'volume=' + volume];
  if (paused) parts.push('paused=1');
  if (nowPlaying()) parts.push('playing=' + encodeURIComponent(nowPlaying().slice(0, 60)));
  if (spotify.version !== null) parts.push('spotify=' + spotify.version);
  return parts.join('&');
}

async function listen() {
  let wait = 0;
  try {
    asking = window.AbortController ? new AbortController() : null;
    const address = '/api/acts?' + (lastAct === null ? '' : 'after=' + lastAct + '&') + standing();
    const reply = await fetch(address, { cache: 'no-store', signal: asking ? asking.signal : undefined });
    if (!reply.ok) throw new Error('acts ' + reply.status);
    const told = await reply.json();
    lastAct = told.last;
    if (told.spotify) spotifyNews(told.spotify);
    (told.acts || []).forEach((action) => {
      try { act(action); } catch (error) { /* one that cannot be done does not stop the rest */ }
    });
  } catch (error) {
    // Cut short to say how things stand now: ask again at once. Anything else: in a while.
    if (error.name !== 'AbortError') wait = ASK_AGAIN_MS;
  }
  setTimeout(listen, wait);
}

let saidStanding = '';
// A tab was opened, the slideshow paused, a station started: say so now rather than at the
// end of the request that is waiting. (Not for news of Spotify, which came from the server.)
const standingSaid = () => standing().replace(/&spotify=\d+/, '');
standingChanged = () => {
  if (standingSaid() === saidStanding) return;
  saidStanding = standingSaid();
  if (asking) asking.abort();
};

// --- keys ----------------------------------------------------------------------------------

// What each key does, by the name browsers give it. The arrows, the space bar and the
// digits are for a keyboard (in the simulator); the rest are the keys of a media remote.
const KEYS = {
  ArrowLeft: { do: 'previous' }, ArrowRight: { do: 'next' },
  MediaTrackPrevious: { do: 'previous' }, MediaTrackNext: { do: 'skip' },
  MediaPlay: { do: 'play' }, MediaStop: { do: 'stop' }, MediaPause: { do: 'stop' },
  AudioVolumeUp: { do: 'louder' }, AudioVolumeDown: { do: 'quieter' },
  1: { do: 'show', tab: 'pictures' }, 2: { do: 'show', tab: 'calendar' }, 3: { do: 'show', tab: 'todo' },
  4: { do: 'show', tab: 'utilities' }, 5: { do: 'show', tab: 'settings' },
};
document.addEventListener('keydown', (event) => {
  if (event.key === ' ') act({ do: paused ? 'resume' : 'pause' });
  else if (event.key === 'MediaPlayPause') act({ do: nowPlaying() ? 'stop' : 'play' });
  else if (KEYS[event.key]) act(KEYS[event.key]);
});

saidStanding = standingSaid();
listen();
