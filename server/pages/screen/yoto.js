// Yoto, a part of the Utilities tab of the frame's screen: how much listening time each Yoto
// player has left today, drawn so that a small child can see it without reading or numbers.
//
// Each player is a disc that shrinks as its time is used, the way a kitchen timer for children
// does: green while there is plenty, orange when it is nearly gone, red at the very end. In
// the middle, notes dance while it plays, a play sign shows it may play, a stop sign shows
// today's time is all used up, a moon shows it is sleeping (outside the hours it may play,
// as at night) and two bars show it is paused.
// The words underneath are for grown-ups, and so is the quiet button under them: after the
// frame's PIN it gives more time today or less, and pauses the player, for now or for a while.
// It is never offered without a PIN, which is what keeps it from the child.
//
// It comes from Yoto Limiter, which the frame's server follows live (see yoto.rs); the
// grown-ups' button also needs the key to have the control scope. Changing the limits
// themselves stays in Yoto Limiter's own app. This part is only there once a key is kept
// (see `body.no-yoto`). While it is in view
// it asks the server every few seconds and, between answers, counts down by itself.
//
// Loaded after screen.js, whose helpers it uses.

const YOTO_EVERY_MS = 10 * 1000;
// Under this much time left (seconds) the disc turns orange, and under this, red.
const YOTO_NEARLY = 10 * 60, YOTO_LAST = 3 * 60;
// The grown-ups' panel closes by itself when left alone for this long, and the PIN with it.
const YOTO_ASK_FOR_MS = 60 * 1000;
let yoto = null;      // what the server last said: { set_up, in_touch, heard, problem, players }
let yotoAt = 0;       // when it said it, in milliseconds since 1970
let yotoAsking = null, yotoAskTimer = 0;  // the player the grown-ups' panel is open for

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
    return Object.assign(looks, { share: 0, middle: 'stop', said: 'All done for today', more: 'More tomorrow' });
  }
  if (player.blocked) return Object.assign(looks, { said: 'That card is done for today', more: listening(left) + ' left for other cards' });
  if (!player.limited) return Object.assign(looks, { said: 'No limit today', more: listening(player.used_secs) + ' listened' });
  looks.said = colour === 'plenty' ? 'Lots of time left' : colour === 'nearly' ? 'A little time left' : 'Almost done';
  looks.more = listening(left) + ' left of ' + listening(player.limit_secs);
  return looks;
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
  const drawn = yoto.players.map((player) => {
    // Counting down from the last answer while it plays; never below nothing.
    const left = player.remaining_secs - (player.playing && yoto.in_touch ? since : 0);
    return { player: player, looks: yotoLooks(player, left) };
  });
  // The panels are made afresh only when more than the discs has changed; the discs move on in place.
  const key = JSON.stringify(drawn.map((one) => [one.player, one.looks.colour, one.looks.middle, one.looks.said]));
  if (key !== box.dataset.drawn) {
    box.dataset.drawn = key;
    box.textContent = '';
    drawn.forEach((one) => {
      const panel = element('div', 'player ' + one.looks.colour, '');
      const name = panel.appendChild(element('div', 'name', one.player.name));
      if (!one.player.connected) name.appendChild(element('span', 'offline', 'Offline'));
      panel.appendChild(element('div', 'face', ''));
      panel.appendChild(element('div', 'said', one.looks.said));
      panel.appendChild(element('div', 'more', ''));
      const grown = panel.appendChild(element('button', 'press grown-up', 'For grown-ups'));
      grown.onclick = () => openYotoAsk(one.player.id);
      box.appendChild(panel);
    });
  }
  drawn.forEach((one, i) => {
    const panel = box.children[i];
    drawDisc(panel.querySelector('.face'), one.looks);
    const card = one.player.card ? (one.player.playing ? 'Playing ' : 'In it: ') + one.player.card : '';
    const more = [one.looks.more, card].filter(Boolean).join('  ·  ');
    const words = panel.querySelector('.more');
    if (words.textContent !== more) words.textContent = more;
  });
}

// --- for grown-ups: more time or less, and a pause, after the PIN ------------------------

const yotoPlayer = () => (yoto ? yoto.players : []).filter((player) => player.id === yotoAsking)[0];

/// The panel as things are now: no PIN set, the PIN to type, or what can be done.
function drawYotoAsk() {
  const player = yotoPlayer();
  if (!player) return closeYotoAsk();
  const locked = pinSet && !token, box = $('yoto-acts');
  $('yoto-ask').classList.toggle('no-pin', !pinSet);
  $('yoto-ask').classList.toggle('asking', locked);
  $('yoto-ask-said').textContent = locked ? 'Enter the PIN to change ' + player.name : player.name;
  $('yoto-ask-why').textContent = pinSet ? ''
    : 'Set a PIN for the frame first (in Settings, or from a phone), so that only grown-ups can do this.';
  box.textContent = '';
  if (locked || !pinSet) return;
  const row = (label, buttons) => {
    const line = box.appendChild(element('div', 'row', ''));
    line.appendChild(element('span', 'label', label));
    buttons.forEach((b) => {
      const button = line.appendChild(element('button', 'press' + (b.on ? ' on' : ''), b.text));
      button.onclick = () => yotoOrder(b.order, b.done);
    });
  };
  const time = (minutes) => ({
    text: (minutes > 0 ? '+' : '−') + Math.abs(minutes) + ' min',
    order: { time: minutes },
    done: Math.abs(minutes) + (minutes > 0 ? ' more minutes' : ' minutes fewer'),
  });
  if (player.limited) {
    row('More time', [time(5), time(15), time(30)]);
    row('Less time', [time(-5), time(-15)]);
  }
  const held = player.held_until && player.held_until > Date.now() / 1000;
  const pause = [];
  if (held) pause.push({ text: 'Play again', order: 'resume', done: 'It may play again', on: true });
  else if (player.playing && player.connected) pause.push({ text: 'Pause now', order: 'pause', done: 'Paused' });
  pause.push({ text: '30 min', order: { hold: 30 }, done: 'Paused for 30 minutes' });
  pause.push({ text: '1 h', order: { hold: 60 }, done: 'Paused for an hour' });
  row(held ? 'Paused' : 'Pause', pause);
}

function openYotoAsk(id) {
  yotoAsking = id;
  clearYotoPin();
  drawYotoAsk();
  $('yoto-ask').classList.add('on');
  stayYotoAsk();
}

function closeYotoAsk() {
  if (yotoAsking === null) return;
  yotoAsking = null;
  clearTimeout(yotoAskTimer);
  $('yoto-ask').classList.remove('on');
  lockSettings();  // what the PIN bought was for this only
}

function stayYotoAsk() {
  clearTimeout(yotoAskTimer);
  yotoAskTimer = setTimeout(closeYotoAsk, YOTO_ASK_FOR_MS);
}

async function yotoOrder(order, done) {
  const player = yotoPlayer();
  if (!player) return closeYotoAsk();
  try {
    yoto = await send('/api/yoto/order', { player: player.id, order: order });
    yotoAt = Date.now();
  } catch (error) {
    if (error.message === 'PIN required') {
      token = null;
      pinSet = true;
      return drawYotoAsk();
    }
    if (/^Set a PIN/.test(error.message)) {
      pinSet = false;
      return drawYotoAsk();
    }
    $('yoto-ask-why').textContent = error.message.replace(/\.?$/, '.');
    return;
  }
  closeYotoAsk();
  drawYoto();
  say(done, player.name);
}

const clearYotoPin = keypad($('yoto-keys'), $('yoto-dots'), $('yoto-pin-note'), (bought) => {
  token = bought;
  drawYotoAsk();
});
$('yoto-ask-close').onclick = closeYotoAsk;
// Touches on the panel are its own: they neither step through photos nor bring up the tabs.
$('yoto-ask').addEventListener('click', (event) => {
  event.stopPropagation();
  stayYotoAsk();
  touched();
});

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
