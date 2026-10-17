// Yoto, a part of the Utilities tab of the frame's screen: how much listening time each Yoto
// player has left today, drawn so that a small child can see it without reading or numbers.
//
// Each player is a disc that shrinks as its time is used, the way a kitchen timer for children
// does: green while there is plenty, orange when it is nearly gone, red at the very end. In
// the middle, notes dance while it plays, a play sign shows it may play, a moon shows it is
// sleeping (all used up, or outside the hours it may play) and two bars show it is paused.
// The words underneath are for grown-ups.
//
// It comes from Yoto Limiter, which the frame's server follows live with a key that only
// reads (see yoto.rs). Pausing, adding time and changing limits stay in Yoto Limiter's own
// app. This part is only there once a key is kept (see `body.no-yoto`). While it is in view
// it asks the server every few seconds and, between answers, counts down by itself.
//
// Loaded after screen.js, whose helpers it uses.

const YOTO_EVERY_MS = 10 * 1000;
// Under this much time left (seconds) the disc turns orange, and under this, red.
const YOTO_NEARLY = 10 * 60, YOTO_LAST = 3 * 60;
// The drawings in the middle of the disc, in the same 24 by 24 grid as ICONS.
const YOTO_MIDDLES = {
  playing: '<g class="notes"><path class="tune one" d="M7 16.5V8l5-1.2v8"/><circle class="tune one" cx="5.6" cy="16.6" r="1.6"/><circle class="tune one" cx="10.6" cy="15" r="1.6"/>'
    + '<path class="tune two" d="M17.5 13.5V6.5"/><circle class="tune two" cx="16" cy="13.6" r="1.6"/></g>',
  sleeping: '<path d="M16.5 15.5A6 6 0 1 1 10 6a4.8 4.8 0 0 0 6.5 9.5z"/><path class="star" d="M17 5.5l.5 1.3 1.3.5-1.3.5-.5 1.3-.5-1.3-1.3-.5 1.3-.5z"/>',
  paused: '<path d="M9.5 7.5v9M14.5 7.5v9"/>',
  waiting: '<path class="go" d="M9.5 7.5l7 4.5-7 4.5z"/>',
};

let yoto = null;      // what the server last said: { set_up, in_touch, heard, problem, players }
let yotoAt = 0;       // when it said it, in milliseconds since 1970

const yotoInView = () => tab === 'utilities' && document.body.dataset.tool === 'yoto';

/// A length of time, for grown-ups: "38 min" or "1 h 05".
function listening(seconds) {
  const minutes = Math.max(0, Math.floor(seconds / 60));
  if (minutes < 1) return seconds > 0 ? 'under a minute' : '0 min';
  if (minutes < 60) return minutes + ' min';
  return Math.floor(minutes / 60) + ' h' + (minutes % 60 ? ' ' + ('0' + minutes % 60).slice(-2) : '');
}

async function refreshYoto() {
  if (document.body.classList.contains('no-yoto')) return;
  try {
    yoto = await (await fetch('/api/yoto', { cache: 'no-store' })).json();
    yotoAt = Date.now();
  } catch (error) {
    return;
  }
  drawYoto();
}

/// How a player is: what the disc shows (how full, what colour, what in the middle) and the
/// words underneath.
function yotoLooks(player, left) {
  const now = Date.now() / 1000;
  const share = !player.limited ? 1 : player.limit_secs > 0 ? Math.max(0, Math.min(1, left / player.limit_secs)) : 0;
  const colour = !player.limited || left > YOTO_NEARLY ? 'plenty' : left > YOTO_LAST ? 'nearly' : 'last';
  const looks = { share: share, colour: colour, middle: player.playing ? 'playing' : 'waiting', said: '', more: '' };
  if (player.held_until && player.held_until > now) {
    return Object.assign(looks, { middle: 'paused', colour: 'resting', said: 'Paused', more: 'until ' + clockText(new Date(player.held_until * 1000)) });
  }
  if (player.locked) {
    return Object.assign(looks, { middle: 'sleeping', colour: 'resting', said: 'Sleeping',
      more: player.allowed_from ? 'Wakes up at ' + player.allowed_from : 'Not time for listening' });
  }
  if (player.limited && (player.exhausted || left <= 0)) {
    return Object.assign(looks, { share: 0, middle: 'sleeping', said: 'All done for today', more: 'More tomorrow' });
  }
  if (player.blocked) return Object.assign(looks, { said: 'That card is done for today', more: listening(left) + ' left for other cards' });
  if (!player.limited) return Object.assign(looks, { said: 'No limit today', more: listening(player.used_secs) + ' listened' });
  looks.said = colour === 'plenty' ? 'Lots of time left' : colour === 'nearly' ? 'A little time left' : 'Almost done';
  looks.more = listening(left) + ' left of ' + listening(player.limit_secs);
  return looks;
}

/// The disc: the time left is the coloured slice, from twelve o'clock round, as on a clock.
function yotoDisc(looks) {
  const size = 100, middle = size / 2, radius = 47;
  let slice = '';
  if (looks.share >= 0.999) {
    slice = '<circle class="left" cx="50" cy="50" r="' + radius + '"/>';
  } else if (looks.share > 0.001) {
    const angle = looks.share * 2 * Math.PI;
    const x = middle + radius * Math.sin(angle), y = middle - radius * Math.cos(angle);
    slice = '<path class="left" d="M50 50L50 ' + (middle - radius) + 'A' + radius + ' ' + radius + ' 0 '
      + (looks.share > 0.5 ? 1 : 0) + ' 1 ' + x.toFixed(2) + ' ' + y.toFixed(2) + 'Z"/>';
  }
  return '<svg class="disc ' + looks.colour + '" viewBox="0 0 100 100">'
    + '<circle class="used" cx="50" cy="50" r="' + radius + '"/>' + slice
    + '<circle class="hub" cx="50" cy="50" r="19"/>'
    + '<g class="middle ' + looks.middle + '" transform="translate(32 32) scale(1.5)">' + YOTO_MIDDLES[looks.middle] + '</g>'
    + '</svg>';
}

function drawYoto() {
  if (!yoto) return;
  const since = (yoto.heard || 0) + (Date.now() - yotoAt) / 1000;
  const note = yoto.problem ? yoto.problem.charAt(0).toUpperCase() + yoto.problem.slice(1) + '.'
    : !yoto.in_touch && yoto.players.length ? 'Yoto Limiter has not been heard from lately. This is how things were.'
    : !yoto.players.length ? 'Waiting to hear from Yoto Limiter…' : '';
  $('yoto-note').textContent = note;

  const box = $('yoto-players');
  box.className = 'players count-' + Math.min(yoto.players.length, 3);
  // Drawn afresh only when what shows has changed, so the notes keep dancing smoothly.
  const drawn = yoto.players.map((player) => {
    // Counting down from the last answer while it plays; never below nothing.
    const left = player.remaining_secs - (player.playing && yoto.in_touch ? since : 0);
    const looks = yotoLooks(player, left);
    looks.share = Math.round(looks.share * 400) / 400;
    return { player: player, looks: looks };
  });
  const key = JSON.stringify(drawn);
  if (key === box.dataset.drawn) return;
  box.dataset.drawn = key;
  box.textContent = '';
  drawn.forEach((one) => {
    const panel = element('div', 'player ' + one.looks.colour, '');
    const name = panel.appendChild(element('div', 'name', one.player.name));
    if (!one.player.connected) name.appendChild(element('span', 'offline', 'Offline'));
    panel.appendChild(element('div', 'face', '')).innerHTML = yotoDisc(one.looks);
    panel.appendChild(element('div', 'said', one.looks.said));
    const card = one.player.card ? (one.player.playing ? 'Playing ' : 'In it: ') + one.player.card : '';
    panel.appendChild(element('div', 'more', [one.looks.more, card].filter(Boolean).join('  ·  ')));
    box.appendChild(panel);
  });
}

// Asked often while in view; the disc moves on every couple of seconds between answers.
setInterval(() => {
  if (yotoInView()) refreshYoto();
}, YOTO_EVERY_MS);
setInterval(() => {
  if (yotoInView()) drawYoto();
}, 2000);
$('tools').addEventListener('click', () => {
  if (yotoInView()) refreshYoto();
});
refreshYoto();
