// What the frame's own screen does. See index.html for what the page is.
'use strict';

const params = new URLSearchParams(location.search);
const RELOAD_PHOTOS_MS = 10 * 60 * 1000;
const RELOAD_CALENDAR_MS = 5 * 60 * 1000;
const RELOAD_WEATHER_MS = 10 * 60 * 1000;
const ALIVE_MS = 30 * 1000;
const RETRY_MS = 15 * 1000;
const DAY_MS = 24 * 60 * 60 * 1000;
// The tabs stay up this long after a touch on Pictures. Settings is left after the second of
// these when the screen is set never to go back to Pictures by itself.
const BAR_MS = 6 * 1000, LEAVE_SETTINGS_MS = 2 * 60 * 1000;
const PICTURE = /\.(jpe?g|png|webp)$/i;
// How many of the photos next in line may be brought forward to share the screen, and how many may share it.
const LOOK_AHEAD = 12, MOST_TOGETHER = 4;
// What each further photo on screen must save in empty space (as a log ratio) to be worth adding.
const PER_EXTRA_PHOTO = 0.03;
const HISTORY = 40;
// Each photo's date shows for this long when its screen comes up (and whenever the tabs are up).
const DATES_MS = 5 * 1000;
// A photo enlarged to be looked at stays this long before the slideshow goes on.
const ALONE_MS = 2 * 60 * 1000;
// "Coming up" lists at most this many events, fewer if they do not fit.
const AGENDA_ITEMS = 12, DAY_ITEMS = 5, CALENDAR_DAYS = 75;
// A month or week looked at away from today is fetched with this many days from its first.
const BROWSE_DAYS = 50;
// Today's events are listed under the clock between these hours.
const MORNING_FROM = 6, MORNING_UNTIL = 10, MORNING_ITEMS = 3;
// A note about an event stays up this long; one that is found later than this is not sounded.
const SOON_MS = 60 * 1000, LATE_MS = 90 * 1000;
const PACES = [[10, '10 s'], [15, '15 s'], [30, '30 s'], [60, '1 min'], [300, '5 min']];
const ORDERS = [['shuffled', 'Shuffled'], ['recent', 'Recent more often'], ['newest', 'Newest first']];
// In "Recent more often", a photo taken within this many days comes round this many times a round.
const RECENT_TURNS = [[31, 3], [365, 2]];
const ON_OFF = [[true, 'On'], [false, 'Off']];
const RESTS = [[1, '1 min'], [2, '2 min'], [5, '5 min'], [15, '15 min'], [30, '30 min'], [null, 'Never']];
// Light colours or dark, kept by each browser: always one, or light by day and dark by night.
const THEMES = [['dark', 'Dark'], ['light', 'Light'], ['auto', 'Light by day, dark by night']];
// By day means while the sun is up where the weather is for; without the weather, these hours.
const DAY_FROM = 7, DAY_UNTIL = 19;
// How loud a chime is, as a share of the loudest the frame is set to. It does not go up and
// down with the music's volume: a note has to be heard however quietly the music plays.
const CHIME_LOUDNESS = [[0.12, 'Soft'], [0.3, 'Normal'], [0.75, 'Loud']];
// While a chime sounds, and this long after it starts, the music is turned down to make room.
const CHIME_MS = 2300;
const CHIMES = [[null, 'Off'], [0, 'At the time'], [5, '5 min before'], [10, '10 min before'], [15, '15 min before'], [30, '30 min before']];
// A colour for each subscribed calendar, by its place in the list; events added by hand are grey.
const COLOURS = ['#6cb6ff', '#f2a65a', '#7ee0a1', '#e58ad6', '#f2e36b', '#ff8f85', '#9aa5ff', '#5fd4d0'];
const ADDED_COLOUR = '#c9ccd1';
// Dates are written the way they are where the frame is, whatever language it was set up in:
// month first in the Americas, day first elsewhere.
const ZONE = (Intl.DateTimeFormat().resolvedOptions().timeZone || '');
const LOCALE = /^(America|US)\//.test(ZONE) || ZONE === 'Pacific/Honolulu' ? 'en-US' : 'en-GB';
// What the sky is doing, by WMO weather code: [codes up to, picture, words].
const SKIES = [
  [0, '☀️', 'Clear'], [2, '🌤️', 'Partly cloudy'], [3, '☁️', 'Cloudy'], [48, '🌫️', 'Fog'], [57, '🌦️', 'Drizzle'],
  [67, '🌧️', 'Rain'], [77, '🌨️', 'Snow'], [82, '🌧️', 'Showers'], [86, '🌨️', 'Snow showers'], [99, '⛈️', 'Thunderstorm'],
];

// The drawings on the tabs, as the insides of a 24 by 24 SVG drawn in lines.
const ICONS = {
  pictures: '<rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="8.5" cy="10" r="1.5"/><path d="M21 15.5l-5-5L7 19"/>',
  calendar: '<rect x="3" y="5" width="18" height="16" rx="2"/><path d="M3 10h18M8 3v4M16 3v4"/>',
  settings: '<path d="M4 7h9M19 7h1M4 17h1M11 17h9"/><circle cx="16" cy="7" r="2.5"/><circle cx="8" cy="17" r="2.5"/>',
  pause: '<path d="M9 6v12M15 6v12"/>',
  grow: '<path d="M14 4h6v6M10 20H4v-6M20 4l-7 7M4 20l7-7"/>',
  shrink: '<path d="M20 10h-6V4M4 14h6v6M14 10l7-7M10 14l-7 7"/>',
  back: '<path d="M15 5l-7 7 7 7"/>',
  on: '<path d="M9 5l7 7-7 7"/>',
  play: '<path d="M8 5.5l10.5 6.5L8 18.5z"/>',
  month: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9.5h18M3 14.5h18M9 9.5V20M15 9.5V20"/>',
  week: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7.5 4v16M12 4v16M16.5 4v16"/>',
  night: '<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>',
  sound: '<path d="M6 16v-5a6 6 0 0 1 12 0v5l2 2H4zM10 20.5a2 2 0 0 0 4 0"/>',
  wifi: '<path d="M3 9.5a14 14 0 0 1 18 0M6 13a9.5 9.5 0 0 1 12 0M9 16.5a5 5 0 0 1 6 0"/><circle cx="12" cy="19.5" r=".6"/>',
  frame: '<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8.5 21h7M12 17v4"/>',
  home: '<path d="M3.5 11L12 3.5l8.5 7.5M5.5 9.5V20h13V9.5M10 20v-5.5h4V20"/>',
  nownext: '<rect x="2.5" y="6" width="8" height="12" rx="2"/><rect x="13.5" y="8" width="6" height="8" rx="1.5"/><path d="M10.5 12h3"/>',
  yoto: '<rect x="4" y="6" width="16" height="15" rx="3"/><path d="M7.5 6V3.5M16.5 6V3.5"/><rect x="8" y="10" width="8" height="5.5" rx="1"/>',
  todo: '<rect x="3.5" y="3.5" width="17" height="17" rx="3"/><path d="M8 12.5l3 3 5.5-6.5"/>',
  utilities: '<circle cx="12" cy="13.5" r="7.5"/><path d="M12 9.5v4l2.5 2.5M9.5 2.5h5"/>',
  alarm: '<circle cx="12" cy="13" r="7"/><path d="M12 9.5V13l2.3 2.3M5.5 3.5L2.5 6.5M18.5 3.5l3 3"/>',
  pomodoro: '<circle cx="12" cy="14" r="7"/><path d="M12 7c0-2 1-3.5 3-3.5M8.5 6.5l3.5 1.2 3.5-1.2"/>',
  music: '<path d="M9 17.5V5.5l10-2v12"/><circle cx="6.5" cy="17.5" r="2.5"/><circle cx="16.5" cy="15.5" r="2.5"/>',
  stop: '<rect x="6.5" y="6.5" width="11" height="11" rx="1.5"/>',
  skip: '<path d="M6 5.5l9 6.5-9 6.5zM18 5.5v13"/>',
  rewind: '<path d="M18 5.5l-9 6.5 9 6.5zM6 5.5v13"/>',
};

const $ = (id) => document.getElementById(id);
const slides = [$('a'), $('b')];
let tab = 'pictures';
// How things are set. The server keeps these; ?every= and ?order= in the address win, for trying things out.
let slideshow = { every: 15, order: 'shuffled', memories: true, morning: true, weather: true, home_after: 5, show: 'all' };
let night = { on: false, from: '23:00', until: '07:00', when_dark: false };
let chime = null;   // minutes before an event to sound, or null for no chime
let photos = [];    // every photo the frame shows, as [path, taken, width, height, favourite]
let queue = [];     // the photos still to come this time round
let shown = [];     // the screens shown so far, for stepping back
let at = -1;        // which of them is on screen
let front = 0;      // which slide element is on screen
let timer = 0;
let paused = false;
let over = false;   // something is open over the pictures (a photo's menu), so they wait
let busy = false;
let asleep = false;
let pictureRules = 0;  // how many rules give events a picture, for Now and Next
let events = [];    // the calendar around today: { title, place, allDay, start, end, colour, from }
// Looking around the calendar: the month or week it has been moved to (null for this one), the
// events of that month or week, and the day that was picked to see what is on it.
let cursor = null, browsed = [], picked = null;
let weather = null; // { now: { temp, code, day }, days: [{ date, code, max, min, rain }] }
let barTimer = 0, leaveTimer = 0, deviceTimer = 0, soonTimer = 0, datedTimer = 0;
// With a PIN set, Settings asks for it each time it is opened; the token is what the PIN bought.
let token = null, pinSet = false;
let heard = false;  // whether the server has said how things are set yet
let rulesKnown = [];  // the reminder rules the calendar was last fetched with
// What reminders.js does with the events each time they are looked at.
let remind = () => {};
// What extras.js has to say about leaving for an event that is travelled to, for the lists.
let leaving = () => '';
// And what it shows when an event in a list is touched.
let showEvent = () => {};
// And how it forgets the plans it holds, when the trips may have changed.
let forgetPlans = () => {};
// What todo.js does when the server says the things to do have changed.
let todoNews = () => {};
let todoKnown = null;
let calendarKnown = null;  // the server's count of changes to the calendar, as last heard
// Two things the Utilities tab (utilities.js) has a say in: what is asked of the display each
// time the page reports in, and whether its tab may be left for Pictures by itself.
let aliveNeeds = () => '';
let staysOpen = () => false;
// What music.js does with the radio stations the server says there are.
let stationsNews = () => {};
// And how it turns the music down for a moment, so that a chime is heard over it.
let quietMusic = () => {};
// What actions.js does when how the screen stands has changed (a tab opened, the slideshow
// paused): it tells the server, for whoever is holding the remote control.
let standingChanged = () => {};

const url = (kind, path) => '/' + kind + '?p=' + encodeURIComponent(path);
const shape = (photo) => (photo[2] && photo[3] ? photo[2] / photo[3] : 0);
const everyMs = () => Math.max(3, Number(params.get('every')) || slideshow.every) * 1000;
const orderNow = () => params.get('order') || slideshow.order;
const yearOf = (photo) => new Date(photo[1] * 1000).getFullYear();
const sameChoice = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/// The photos the slideshow is set to show: all of them, the favourites, or one year's.
/// A choice that no photo answers to shows them all, rather than an empty screen.
function chosen() {
  const show = slideshow.show;
  const some = show === 'favourites' ? photos.filter((p) => p[4])
    : show && show.year ? photos.filter((p) => yearOf(p) === show.year) : photos;
  return some.length ? some : photos;
}

function shuffle(list) {
  for (let i = list.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    const kept = list[i];
    list[i] = list[j];
    list[j] = kept;
  }
  return list;
}

function element(tag, name, text) {
  const node = document.createElement(tag);
  node.className = name;
  node.textContent = text;
  return node;
}

/// Give a button its drawing and its words.
function label(button, icon, words) {
  button.innerHTML = '<svg viewBox="0 0 24 24">' + ICONS[icon] + '</svg>';
  button.appendChild(document.createTextNode(words));
}

// --- a disc that empties as time passes, for those too young to read a clock ------------
// Used by Yoto (listening time left) and by the timers drawn as a picture. The coloured slice
// is the time left, from twelve o'clock round as on a clock, like a children's kitchen timer.

// What can be drawn in the middle of a disc, in the same 24 by 24 grid as ICONS.
const DISC_MIDDLES = {
  playing: '<path class="tune one" d="M7 16.5V8l5-1.2v8"/><circle class="tune one" cx="5.6" cy="16.6" r="1.6"/><circle class="tune one" cx="10.6" cy="15" r="1.6"/>'
    + '<path class="tune two" d="M17.5 13.5V6.5"/><circle class="tune two" cx="16" cy="13.6" r="1.6"/>',
  sleeping: '<path class="moon" d="M16.5 15.5A6 6 0 1 1 10 6a4.8 4.8 0 0 0 6.5 9.5z"/><path class="moon star" d="M17 5.5l.5 1.3 1.3.5-1.3.5-.5 1.3-.5-1.3-1.3-.5 1.3-.5z"/>',
  paused: '<path d="M9.5 7.5v9M14.5 7.5v9"/>',
  stop: '<path class="stop" d="M8.9 3.5h6.2l5.4 5.4v6.2l-5.4 5.4H8.9l-5.4-5.4V8.9z"/><path class="bar" d="M8.5 12h7"/>',
  waiting: '<path class="solid" d="M9.5 7.5l7 4.5-7 4.5z"/>',
  sand: '<path d="M7 4h10M7 20h10M8.5 4c0 4.5 3.5 5 3.5 8s-3.5 3.5-3.5 8M15.5 4c0 4.5-3.5 5-3.5 8s3.5 3.5 3.5 8"/><path class="solid sand" d="M9.8 19.6h4.4L12 16.5z"/>',
};

/// The outline of the slice for a share of the time (0 to 1) still to go.
function discSlice(share) {
  if (share >= 0.999) return 'M50 3A47 47 0 1 1 50 97A47 47 0 1 1 50 3Z';
  if (share <= 0.001) return 'M50 50';
  const angle = share * 2 * Math.PI;
  return 'M50 50L50 3A47 47 0 ' + (share > 0.5 ? 1 : 0) + ' 1 ' + (50 + 47 * Math.sin(angle)).toFixed(2)
    + ' ' + (50 - 47 * Math.cos(angle)).toFixed(2) + 'Z';
}

/// Draw a disc in `box`, or move on the one already there. `looks` is { share, colour, middle }:
/// the colour is plenty, nearly, last or resting, and the middle one of DISC_MIDDLES (or none).
/// With `picture` (an event's emoji) in `looks`, that is in the middle instead, larger.
/// From one moment to the next only the slice changes, so what moves in the middle moves smoothly.
function drawDisc(box, looks) {
  let disc = box.firstElementChild;
  const middle = looks.picture ? 'picture' : looks.middle || '';
  const drawn = middle + (looks.picture || '');
  if (!disc || disc.getAttribute('data-middle') !== drawn) {
    box.innerHTML = '<svg class="disc" viewBox="0 0 100 100">'
      + '<circle class="used" cx="50" cy="50" r="47"/><path class="left"/>'
      + (middle === 'picture' ? '<circle class="hub" cx="50" cy="50" r="31"/><text class="picture" x="50" y="52"></text>'
        : middle ? '<circle class="hub" cx="50" cy="50" r="19"/><g class="middle ' + middle + '" transform="translate(32 32) scale(1.5)">' + DISC_MIDDLES[middle] + '</g>' : '')
      + '</svg>';
    disc = box.firstElementChild;
    disc.setAttribute('data-middle', drawn);
    if (looks.picture) disc.querySelector('.picture').textContent = looks.picture;
  }
  disc.setAttribute('class', 'disc ' + looks.colour);
  disc.querySelector('.left').setAttribute('d', discSlice(looks.share));
}

/// Ask the frame's server to change something. With a PIN set, that takes the token the PIN bought.
async function send(path, body) {
  const headers = { 'X-Frame-App': '1', 'Content-Type': 'application/json' };
  if (token) headers['X-Frame-Token'] = token;
  const reply = await fetch(path, { method: 'POST', headers: headers, body: JSON.stringify(body || {}) });
  const answer = await reply.json();
  if (reply.status === 401 && tab === 'settings') askForPin();
  if (!reply.ok) throw new Error(answer.error || 'refused ' + reply.status);
  return answer;
}

// --- photos ---------------------------------------------------------------------------

async function refreshPhotos() {
  try {
    const reply = await fetch('/api/slides', { cache: 'no-store' });
    if (!reply.ok) throw new Error('slides ' + reply.status);
    const found = ((await reply.json()).photos || []).filter((p) => PICTURE.test(p[0]));
    if (found.length) {
      photos = found;
      // Photos still in line keep their place; ones deleted since are dropped.
      const known = new Map(photos.map((p) => [p[0], p]));
      queue = queue.map((p) => known.get(p[0])).filter(Boolean);
      drawSettings();  // the years there are photos from may have changed
    }
    setTimeout(refreshPhotos, RELOAD_PHOTOS_MS);
  } catch (error) {
    setTimeout(refreshPhotos, RETRY_MS);
  }
  document.body.classList.toggle('empty', photos.length === 0);
  $('notice').textContent = photos.length ? '' : 'No photos to show yet. Waiting for the frame…';
  if (photos.length && at < 0) step(1);
}

function load(src) {
  return new Promise((resolve, reject) => {
    const image = new Image();
    // Decoded before it counts as loaded, so showing it later does not stall the fade.
    image.onload = () => (image.decode ? image.decode().then(() => resolve(image), () => resolve(image)) : resolve(image));
    image.onerror = reject;
    image.src = src;
  });
}

/// Whether a photo was taken on today's date in an earlier year.
function isMemory(photo) {
  const taken = new Date(photo[1] * 1000), today = new Date();
  return taken.getDate() === today.getDate() && taken.getMonth() === today.getMonth() && taken.getFullYear() < today.getFullYear();
}

/// Bring the photos taken on this day in earlier years to the head of the line, oldest first.
function memoriesFirst() {
  if (!slideshow.memories) return;
  const memories = queue.filter(isMemory).sort((x, y) => x[1] - y[1]);
  if (memories.length) queue = memories.concat(queue.filter((p) => !isMemory(p)));
}

// An arrangement is a row of columns, each one photo or several one above the other.
// Every photo keeps its own shape, so the arrangement as a whole has a shape too: `total`,
// its width for a height of one. The closer that is to the screen's, the less is left empty.
const columnShape = (column) => 1 / column.reduce((sum, p) => sum + 1 / shape(p), 0);

function layOut(columns) {
  const total = columns.reduce((sum, column) => sum + columnShape(column), 0);
  const cells = [];
  let x = 0;
  columns.forEach((column) => {
    const wide = columnShape(column) / total;
    let y = 0;
    column.forEach((p) => {
      const tall = columnShape(column) / shape(p);
      cells.push({ photo: p, x: x, y: y, w: wide, h: tall });
      y += tall;
    });
    x += wide;
  });
  return { total: total, cells: cells };
}

/// Every way of sharing `group` out among columns.
function intoColumns(group) {
  if (!group.length) return [[]];
  const first = group[0];
  const ways = [];
  intoColumns(group.slice(1)).forEach((columns) => {
    ways.push([[first]].concat(columns));
    columns.forEach((column, i) => {
      ways.push(columns.map((other, k) => (k === i ? [first].concat(other) : other)));
    });
  });
  return ways;
}

/// The arrangement of `head`, alone or with some of `others`, that leaves least of the screen empty.
/// On a wide screen that is mostly photos side by side; on a tall one, mostly one above the other.
function arrange(head, others, screen) {
  let best = null;
  const tryGroup = (group) => {
    const cost = PER_EXTRA_PHOTO * (group.length - 1);
    intoColumns(group).forEach((columns) => {
      const total = columns.reduce((sum, column) => sum + columnShape(column), 0);
      const empty = Math.abs(Math.log(total / screen));
      if (!best || empty + cost < best.cost) best = { cost: empty + cost, columns: columns };
    });
  };
  const grow = (group, from) => {
    tryGroup(group);
    if (group.length === MOST_TOGETHER) return;
    for (let i = from; i < others.length; i++) grow(group.concat([others[i]]), i + 1);
  };
  grow([head], 0);
  // Vary which photo is where, so the same shapes do not always give the same picture.
  return layOut(shuffle(best.columns.map(shuffle)));
}

/// One time round the photos, in the order asked for. Shuffled, a favourite comes round once more
/// than it would otherwise; recent photos more often still in "Recent more often".
function round(some, order, now) {
  if (order === 'newest') return some.slice().sort((x, y) => y[1] - x[1]);
  const line = [];
  some.forEach((p) => {
    const days = (now - p[1] * 1000) / DAY_MS;
    const recent = order === 'recent' ? RECENT_TURNS.find((turns) => days <= turns[0]) : null;
    const turns = (recent ? recent[1] : 1) + (p[4] ? 1 : 0);
    for (let i = 0; i < turns; i++) line.push(p);
  });
  return shuffle(line);
}

/// The next screen: the photo at the head of the line and whatever goes well beside it.
async function nextScreen() {
  if (!queue.length) {
    queue = round(chosen(), orderNow(), Date.now());
    memoriesFirst();
  }
  const head = queue.shift();
  if (!shape(head)) {
    // Not measured by the frame yet: the picture itself says what shape it is.
    const image = await load(url('file', head[0]));
    head[2] = image.naturalWidth;
    head[3] = image.naturalHeight;
  }
  let others = queue.slice(0, LOOK_AHEAD).filter((p, i, list) => shape(p) && p !== head && list.indexOf(p) === i);
  // A memory shares the screen with other memories while there are any, so the year means something.
  const together = slideshow.memories && isMemory(head) ? others.filter(isMemory) : [];
  if (together.length) others = together;
  const screen = arrange(head, others, window.innerWidth / window.innerHeight);
  // Each photo beside the head leaves the line once; a favourite's second turn stays in it.
  screen.cells.forEach((cell) => {
    const place = queue.indexOf(cell.photo);
    if (cell.photo !== head && place >= 0) queue.splice(place, 1);
  });
  return screen;
}

/// Size and centre an arrangement's box: the whole width or the whole height of the screen,
/// whichever its shape reaches first.
function place(box) {
  const fit = Number(box.dataset.shape) / (window.innerWidth / window.innerHeight);
  box.style.width = 100 * Math.min(1, fit) + '%';
  box.style.height = 100 * Math.min(1, 1 / fit) + '%';
  box.style.left = 50 * (1 - Math.min(1, fit)) + '%';
  box.style.top = 50 * (1 - Math.min(1, 1 / fit)) + '%';
}
const standsTall = () => window.innerHeight > window.innerWidth;
let tall = standsTall();
let turned = false;

/// The frame was turned: what is on screen was arranged for the other shape, so arrange it afresh.
function rearrange() {
  if (busy) {
    turned = true;  // a screen is being drawn; do it once that is done
    return;
  }
  turned = false;
  const ahead = shown.slice(Math.max(0, at)).concat(prepared ? [prepared.screen] : []);
  prepared = null;
  const again = ahead.reduce((all, screen) => all.concat(screen.cells.map((cell) => cell.photo)), []);
  queue = again.concat(queue);
  shown = [];
  at = -1;
  step(1);
}

window.addEventListener('resize', () => {
  const boxes = document.querySelectorAll('.collage');
  for (let i = 0; i < boxes.length; i++) place(boxes[i]);
  drawCalendar(new Date(), true);
  if (tall !== standsTall()) {
    tall = standsTall();
    rearrange();
  }
});

/// The screen after this one, made ready while this one is looked at: chosen, with its photos
/// loaded and decoded, so a touch (or the timer) only has to show it.
let prepared = null, preparing = null;
function prepare() {
  if (preparing || prepared) return;
  preparing = (async () => {
    try {
      const screen = await nextScreen();
      // Taken from the line as it stands now: if the line or the frame's shape changes, it is not used.
      const made = { screen: screen, queue: queue, left: queue.length, tall: tall };
      await Promise.all(screen.cells.map((cell) => load(url('file', cell.photo[0]))));
      prepared = made;
    } catch (error) {
      // A photo that will not load: the next step will find that out itself and go on.
    }
    preparing = null;
  })();
}

/// The prepared screen, if the line it came from is still the line.
function takePrepared() {
  const made = prepared;
  prepared = null;
  return made && made.queue === queue && made.left === queue.length && made.tall === tall ? made.screen : null;
}

async function draw(screen, quick) {
  await Promise.all(screen.cells.map((cell) => load(url('file', cell.photo[0]))));
  const next = slides[1 - front];
  // A touch asks for the photo now: a short fade. The timer's is slow and gentle.
  next.style.transitionDuration = slides[front].style.transitionDuration = quick ? '.25s' : '';
  next.textContent = '';
  const box = element('div', 'collage', '');
  box.dataset.shape = screen.total;
  place(box);
  screen.cells.forEach((cell) => {
    const div = element('div', 'cell', '');
    div.style.backgroundImage = 'url("' + url('file', cell.photo[0]) + '")';
    div.style.left = 100 * cell.x + '%';
    div.style.top = 100 * cell.y + '%';
    div.style.width = 100 * cell.w + '%';
    div.style.height = 100 * cell.h + '%';
    // When it was taken; for a memory, that it is one and from which year.
    const taken = new Date(cell.photo[1] * 1000);
    div.appendChild(element('div', 'taken', (cell.photo[4] ? '★  ' : '') + (slideshow.memories && isMemory(cell.photo)
      ? 'On this day  ·  ' + taken.getFullYear()
      : taken.toLocaleDateString(LOCALE, { day: 'numeric', month: 'long', year: 'numeric' }))));
    // A photo that shares the screen can be given all of it: a button on it while the tabs are up.
    // One that was given it has a button that puts the others back.
    if (screen.cells.length > 1 || screen.alone) {
      const grow = element('button', 'grow only', '');
      label(grow, screen.alone ? 'shrink' : 'grow', '');
      grow.setAttribute('aria-label', screen.alone ? 'Back to the photos it was with' : 'Show this photo alone');
      grow.onclick = (event) => {
        event.stopPropagation();
        if (screen.alone) restore();
        else enlarge(cell.photo);
        touched();
      };
      div.appendChild(grow);
    }
    box.appendChild(div);
  });
  next.appendChild(box);
  next.classList.add('on');
  slides[front].classList.remove('on');
  front = 1 - front;
  // The dates show for a few seconds, then leave the photos to themselves.
  document.body.classList.add('dated');
  clearTimeout(datedTimer);
  datedTimer = setTimeout(() => document.body.classList.remove('dated'), DATES_MS);
}

/// Show one photo alone, as the next screen. It stays a good while, for looking at, unless
/// the photos it was with are put back first; then the slideshow goes on.
function enlarge(photo) {
  if (busy || !shape(photo)) return;
  shown.splice(at + 1, 0, { total: shape(photo), alone: true, cells: [{ photo: photo, x: 0, y: 0, w: 1, h: 1 }] });
  step(1);
}

/// Put back the screen a photo was enlarged from.
async function restore() {
  if (busy || at < 1 || !shown[at].alone) return;
  busy = true;
  clearTimeout(timer);
  shown.splice(at, 1);
  at--;
  try {
    await draw(shown[at]);
  } catch (error) {
    // The screen it came from will not load; the next one comes at the usual time.
  }
  busy = false;
  schedule();
}

/// Show the next screen of photos in a while, if the slideshow is running and in view.
function schedule() {
  clearTimeout(timer);
  const stays = shown[at] && shown[at].alone ? Math.max(everyMs(), ALONE_MS) : everyMs();
  if (!paused && !asleep && !over && tab === 'pictures') timer = setTimeout(() => step(1), stays);
}

/// Move one screen on (or back). A screen with a photo that will not load is skipped.
async function step(by, quick) {
  if (busy || !photos.length) return;
  busy = true;
  clearTimeout(timer);
  for (let tries = 0; tries < 5; tries++) {
    try {
      if (by < 0) {
        at = Math.max(0, at - 1);
      } else if (at < shown.length - 1) {
        at++;
      } else {
        if (preparing) await preparing;
        shown.push(takePrepared() || await nextScreen());
        if (shown.length > HISTORY) shown.shift();
        at = shown.length - 1;
      }
      await draw(shown[at], quick);
      break;
    } catch (error) {
      shown.splice(at, 1);
      at = Math.min(at, shown.length - 1);
    }
  }
  busy = false;
  if (turned) rearrange();
  else schedule();
  // While this one is looked at, get the one after it ready.
  if (at === shown.length - 1) prepare();
}

function setPaused(now) {
  paused = now;
  document.body.classList.toggle('paused', paused);
  label($('pause'), paused ? 'play' : 'pause', paused ? 'Play' : 'Pause');
  schedule();
  standingChanged();
}

// --- the weather ----------------------------------------------------------------------

function sky(code, day) {
  for (let i = 0; i < SKIES.length; i++) {
    if (code <= SKIES[i][0]) return { picture: code === 0 && day === false ? '🌙' : SKIES[i][1], words: SKIES[i][2] };
  }
  return { picture: '', words: '' };
}
const degrees = (value) => Math.round(value) + '°';

/// Fetch the weather and show it. Called now and then, and whenever the server says there is news.
async function refreshWeather() {
  try {
    const reply = await fetch('/api/weather', { cache: 'no-store' });
    if (!reply.ok) throw new Error('weather ' + reply.status);
    const found = await reply.json();
    weather = found.now ? found : null;
    applyTheme();
    drawWeather();
    drawCalendar(new Date(), true);
  } catch (error) {
    // Still the weather we had; the next turn will try again.
  }
}
setInterval(refreshWeather, RELOAD_WEATHER_MS);

function drawWeather() {
  let text = '';
  if (weather) {
    const now = sky(weather.now.code, weather.now.day), today = weather.days[0];
    text = now.picture + ' ' + degrees(weather.now.temp) + '  ' + now.words;
    if (today) text += '  ·  ' + degrees(today.max) + ' / ' + degrees(today.min);
  }
  $('sky').textContent = slideshow.weather ? text : '';
  $('sky-now').textContent = weather ? sky(weather.now.code, weather.now.day).picture + ' ' + degrees(weather.now.temp) : '';
}
