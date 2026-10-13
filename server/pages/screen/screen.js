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
const ORDERS = [['shuffled', 'Shuffled'], ['newest', 'Newest first']];
const ON_OFF = [[true, 'On'], [false, 'Off']];
const RESTS = [[1, '1 min'], [2, '2 min'], [5, '5 min'], [15, '15 min'], [30, '30 min'], [null, 'Never']];
// Light colours or dark, kept by each browser: always one, or light by day and dark by night.
const THEMES = [['dark', 'Dark'], ['light', 'Light'], ['auto', 'Light by day, dark by night']];
// By day means while the sun is up where the weather is for; without the weather, these hours.
const DAY_FROM = 7, DAY_UNTIL = 19;
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
  todo: '<rect x="3.5" y="3.5" width="17" height="17" rx="3"/><path d="M8 12.5l3 3 5.5-6.5"/>',
  utilities: '<circle cx="12" cy="13.5" r="7.5"/><path d="M12 9.5v4l2.5 2.5M9.5 2.5h5"/>',
  alarm: '<circle cx="12" cy="13" r="7"/><path d="M12 9.5V13l2.3 2.3M5.5 3.5L2.5 6.5M18.5 3.5l3 3"/>',
  pomodoro: '<circle cx="12" cy="14" r="7"/><path d="M12 7c0-2 1-3.5 3-3.5M8.5 6.5l3.5 1.2 3.5-1.2"/>',
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

const url = (kind, path) => '/' + kind + '?p=' + encodeURIComponent(path);
const shape = (photo) => (photo[2] && photo[3] ? photo[2] / photo[3] : 0);
const everyMs = () => Math.max(3, Number(params.get('every')) || slideshow.every) * 1000;
const newestFirst = () => (params.get('order') || slideshow.order) === 'newest';
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
    image.onload = () => resolve(image);
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

/// The next screen: the photo at the head of the line and whatever goes well beside it.
async function nextScreen() {
  if (!queue.length) {
    queue = chosen().slice();
    if (newestFirst()) queue.sort((x, y) => y[1] - x[1]);
    // Shuffled, a favourite comes round twice as often as the rest.
    else shuffle(queue = queue.concat(queue.filter((p) => p[4])));
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
  const again = shown.slice(Math.max(0, at)).reduce((all, screen) => all.concat(screen.cells.map((cell) => cell.photo)), []);
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

async function draw(screen) {
  await Promise.all(screen.cells.map((cell) => load(url('file', cell.photo[0]))));
  const next = slides[1 - front];
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
async function step(by) {
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
        shown.push(await nextScreen());
        if (shown.length > HISTORY) shown.shift();
        at = shown.length - 1;
      }
      await draw(shown[at]);
      break;
    } catch (error) {
      shown.splice(at, 1);
      at = Math.min(at, shown.length - 1);
    }
  }
  busy = false;
  if (turned) rearrange();
  else schedule();
}

function setPaused(now) {
  paused = now;
  document.body.classList.toggle('paused', paused);
  label($('pause'), paused ? 'play' : 'pause', paused ? 'Play' : 'Pause');
  schedule();
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

// --- the calendar --------------------------------------------------------------------

const startOfDay = (date) => new Date(date.getFullYear(), date.getMonth(), date.getDate());
const dayAfter = (date, days) => new Date(date.getFullYear(), date.getMonth(), date.getDate() + days);
const clockText = (date) => date.toLocaleTimeString(LOCALE, { hour: '2-digit', minute: '2-digit', hour12: false });
/// The Monday of the week a day is in.
const weekStart = (date) => dayAfter(startOfDay(date), -((date.getDay() + 6) % 7));
const dayKey = (date) => date.getFullYear() + '-' + (date.getMonth() + 1) + '-' + date.getDate();

/// A time from the frame's server: a number is a moment, text is a day or a time on our own clock.
function when(value) {
  if (typeof value === 'number') return new Date(value * 1000);
  const n = String(value).split(/[-T:]/).map(Number);
  return new Date(n[0], n[1] - 1, n[2], n[3] || 0, n[4] || 0, n[5] || 0);
}

/// The events of `days` days from `first`, as this page keeps them.
async function fetchEvents(first, days) {
  const from = Math.floor(first.getTime() / 1000);
  const reply = await fetch('/api/calendar?from=' + from + '&to=' + (from + days * 24 * 60 * 60), { cache: 'no-store' });
  if (!reply.ok) throw new Error('calendar ' + reply.status);
  const found = await reply.json();
  return (found.events || []).map((e) => ({
    title: e.title, place: e.place, allDay: e.all_day, start: when(e.start), end: when(e.end),
    colour: typeof e.feed === 'number' ? COLOURS[e.feed % COLOURS.length] : ADDED_COLOUR,
    // Which calendar it is on, to say so beside it.
    from: e.added ? 'Added here' : e.calendar || '',
    // The reminder rules that are about it (see reminders.js).
    rules: (e.remind || []).map((rule) => (found.reminders || [])[rule]).filter(Boolean),
    // Which trip says how it is reached, if one does.
    trip: typeof e.trip === 'number' ? e.trip : null,
  }));
}

/// The first day a month or week view of `date` shows: the start of its month or of its week,
/// whichever is earlier, so that both views are whole.
const viewStart = (date) => new Date(Math.min(new Date(date.getFullYear(), date.getMonth(), 1), weekStart(date)));

let calendarTimer = 0;
async function refreshCalendar() {
  clearTimeout(calendarTimer);
  let again = RELOAD_CALENDAR_MS;
  try {
    events = await fetchEvents(viewStart(new Date()), CALENDAR_DAYS);
    if (!cursor) browsed = events;
    drawCalendar(new Date(), true);
    remind(Date.now());
  } catch (error) {
    again = RETRY_MS;
  }
  clearTimeout(calendarTimer);
  calendarTimer = setTimeout(refreshCalendar, again);
}

/// Show the month or week the calendar has been moved to, fetching its events.
async function browse() {
  drawCalendar(new Date(), true);  // at once, with the days in place; their events follow
  const asked = cursor;
  if (!asked) return;
  try {
    const found = await fetchEvents(viewStart(asked), BROWSE_DAYS);
    if (cursor !== asked) return;  // moved on while this was on its way
    browsed = found;
    drawCalendar(new Date(), true);
  } catch (error) {
    // The days are shown without their events; moving again tries again.
  }
}

/// Move the calendar a month or a week on (or back), whichever is being looked at.
function move(by) {
  const now = new Date(), base = cursor || now;
  if (document.body.dataset.view === 'week') {
    cursor = dayAfter(startOfDay(base), 7 * by);
    if (weekStart(cursor).getTime() === weekStart(now).getTime()) cursor = null;
  } else {
    cursor = new Date(base.getFullYear(), base.getMonth() + by, 1);
    if (cursor.getMonth() === now.getMonth() && cursor.getFullYear() === now.getFullYear()) cursor = null;
  }
  picked = null;
  if (!cursor) browsed = events;
  browse();
}

/// Back to this month and week, with nothing picked.
function showToday() {
  cursor = null;
  picked = null;
  browsed = events;
  drawCalendar(new Date(), true);
}

/// Pick a day to see what is on it; picking it again lets go of it.
function pick(day) {
  picked = picked && picked.getTime() === day.getTime() ? null : day;
  drawCalendar(new Date(), true);
}

/// The day an event is listed under: "Today", "Tomorrow" or the date.
function dayText(event, now) {
  const today = startOfDay(now);
  // Something that began earlier and is still on is happening today.
  const day = startOfDay(event.start < today ? today : event.start);
  const away = Math.round((day - today) / DAY_MS);
  return away === 0 ? 'Today' : away === 1 ? 'Tomorrow'
    : day.toLocaleDateString(LOCALE, { weekday: 'long', day: 'numeric', month: 'long' });
}

/// When in a day an event is: its time, or that it takes the whole day.
function timeText(event, day) {
  return event.allDay || event.start < startOfDay(day) ? 'All day' : clockText(event.start);
}

/// The events on each day, keyed by day: whole-day events on every day they cover.
function eventsByDay(list) {
  const days = {};
  list.forEach((e) => {
    const last = e.allDay ? new Date(e.end.getTime() - 1) : e.start;
    for (let day = startOfDay(e.start), n = 0; day <= last && n < 400; day = dayAfter(day, 1), n++) {
      (days[dayKey(day)] = days[dayKey(day)] || []).push(e);
    }
  });
  return days;
}
const inOrder = (list) => (list || []).slice().sort((x, y) => (y.allDay - x.allDay) || (x.start - y.start));

function item(event, text) {
  const node = element('div', 'item', '');
  node.style.borderLeftColor = event.colour;
  if (text) node.appendChild(element('div', 'when', text));
  node.appendChild(element('div', 'what', event.title));
  return node;
}

/// An event in a list: its name, then its time, place and the calendar it is on.
function listed(event, day) {
  const node = item(event, '');
  const about = element('div', 'when', timeText(event, day) + (event.place ? '  ·  ' + event.place : ''));
  if (event.from) {
    about.appendChild(document.createTextNode('  ·  '));
    about.appendChild(element('span', 'from', event.from)).style.color = event.colour;
  }
  node.appendChild(about);
  const leave = leaving(event);
  if (leave) node.appendChild(element('div', 'when leave', leave));
  // A touch shows all there is to say about it.
  node.onclick = (touch) => {
    touch.stopPropagation();
    showEvent(event);
    touched();
  };
  return node;
}

let calendarDrawn = '';
function drawCalendar(now, again) {
  const today = startOfDay(now);
  // The month and week on show: this one, unless the calendar has been moved.
  const shown = cursor || now;
  // Whole-day events last until their final day is over; the rest until they end.
  const coming = events.filter((e) => (e.allDay ? e.end > today : e.end >= now || e.start >= now)).slice(0, AGENDA_ITEMS);
  // Redrawn only when what it would show has changed.
  const key = JSON.stringify([today.getTime(), events.length, coming.map((e) => [e.title, e.place, e.from, dayText(e, now), timeText(e, now)])]);
  if (key === calendarDrawn && !again) return;
  calendarDrawn = key;
  const byDay = eventsByDay(browsed);
  const sameDay = (a, b) => !!a && !!b && a.getTime() === b.getTime();

  // Beside the month: the day that was picked, or else what is coming up, day by day.
  const agenda = $('agenda');
  agenda.textContent = '';
  if (picked) {
    $('agenda-title').textContent = picked.toLocaleDateString(LOCALE, { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' });
    const those = inOrder(byDay[dayKey(picked)]);
    if (!those.length) agenda.appendChild(element('div', 'none', 'Nothing on this day.'));
    those.forEach((e) => agenda.appendChild(listed(e, picked)));
  } else {
    $('agenda-title').textContent = 'Coming up';
    if (!coming.length) agenda.appendChild(element('div', 'none', 'Nothing on the calendar.'));
    let heading = '';
    coming.forEach((e) => {
      const day = dayText(e, now);
      if (day !== heading) agenda.appendChild(element('div', 'heading', day));
      heading = day;
      agenda.appendChild(listed(e, now));
    });
  }
  // Leave off whatever does not fit, and a day's heading left with nothing under it.
  const room = agenda.parentNode;
  while (room.scrollHeight > room.clientHeight && agenda.children.length > 1) {
    agenda.removeChild(agenda.lastChild);
    if (agenda.lastChild.className === 'heading') agenda.removeChild(agenda.lastChild);
  }

  $('month-name').textContent = shown.toLocaleDateString(LOCALE, { month: 'long', year: 'numeric' });
  const grid = $('grid');
  grid.textContent = '';
  // Weeks start on Monday; 2024-01-01 was one.
  for (let day = 1; day <= 7; day++) {
    grid.appendChild(element('span', 'head', new Date(2024, 0, day).toLocaleDateString(LOCALE, { weekday: 'short' })));
  }
  const blanks = (new Date(shown.getFullYear(), shown.getMonth(), 1).getDay() + 6) % 7;
  const days = new Date(shown.getFullYear(), shown.getMonth() + 1, 0).getDate();
  for (let i = 0; i < blanks; i++) grid.appendChild(element('span', '', ''));
  for (let n = 1; n <= days; n++) {
    const day = new Date(shown.getFullYear(), shown.getMonth(), n);
    const cell = element('span', (sameDay(day, today) ? 'today ' : '') + (sameDay(day, picked) ? 'picked' : ''), n);
    cell.dataset.day = day.getTime();
    const colours = [];
    (byDay[dayKey(day)] || []).forEach((e) => {
      if (colours.indexOf(e.colour) < 0 && colours.length < 4) colours.push(e.colour);
    });
    if (colours.length) {
      const dots = element('span', 'dots', '');
      colours.forEach((colour) => { dots.appendChild(element('i', '', '')).style.background = colour; });
      cell.appendChild(dots);
    }
    grid.appendChild(cell);
  }

  // The week, Monday to Sunday, with the forecast for the days that have one.
  const week = $('week');
  week.textContent = '';
  const forecast = {};
  ((weather && weather.days) || []).forEach((d) => { forecast[dayKey(when(d.date))] = d; });
  const monday = weekStart(shown), sunday = dayAfter(monday, 6);
  for (let n = 0; n < 7; n++) {
    const day = dayAfter(monday, n);
    const box = element('div', 'day' + (sameDay(day, today) ? ' today' : ''), '');
    box.dataset.day = day.getTime();
    box.appendChild(element('div', 'name', day.toLocaleDateString(LOCALE, { weekday: 'long' })));
    box.appendChild(element('div', 'number', day.getDate()));
    const outlook = forecast[dayKey(day)];
    box.appendChild(element('div', 'sky', outlook ? sky(outlook.code).picture + ' ' + degrees(outlook.max) + ' / ' + degrees(outlook.min) : ''));
    const those = inOrder(byDay[dayKey(day)]);
    those.slice(0, DAY_ITEMS).forEach((e) => box.appendChild(item(e, e.allDay ? '' : clockText(e.start))));
    if (those.length > DAY_ITEMS) box.appendChild(element('div', 'more', '+ ' + (those.length - DAY_ITEMS) + ' more'));
    week.appendChild(box);
  }
  // Which week that is, said beside the arrows: "12 to 18 October 2026".
  const sameMonth = monday.getMonth() === sunday.getMonth();
  $('period').textContent = document.body.dataset.view !== 'week' ? ''
    : monday.toLocaleDateString(LOCALE, sameMonth ? { day: 'numeric' } : { day: 'numeric', month: 'long' })
      + ' to ' + sunday.toLocaleDateString(LOCALE, { day: 'numeric', month: 'long', year: 'numeric' });
  $('to-today').style.visibility = cursor || picked ? 'visible' : 'hidden';

  // In the morning, what is on today goes under the clock.
  const hour = now.getHours();
  const morning = slideshow.morning && hour >= MORNING_FROM && hour < MORNING_UNTIL;
  const todays = morning ? inOrder(eventsByDay(events)[dayKey(today)]) : [];
  $('morning').textContent = todays.length
    ? 'Today:  ' + todays.slice(0, MORNING_ITEMS).map((e) => (e.allDay ? e.title : e.title + ' ' + clockText(e.start))).join('  ·  ')
    : '';
}

// --- events about to start: a note on the screen, and a chime ---------------------------

let audio = null;
/// Two soft notes, the second lower. Made here rather than played from a file.
function playChime() {
  try {
    audio = audio || new (window.AudioContext || window.webkitAudioContext)();
    if (audio.state === 'suspended') audio.resume();
    [[659.25, 0], [523.25, 0.45]].forEach((note) => {
      const tone = audio.createOscillator(), loudness = audio.createGain();
      const start = audio.currentTime + note[1];
      tone.type = 'sine';
      tone.frequency.value = note[0];
      loudness.gain.setValueAtTime(0.0001, start);
      loudness.gain.exponentialRampToValueAtTime(0.3, start + 0.03);
      loudness.gain.exponentialRampToValueAtTime(0.0001, start + 1.6);
      tone.connect(loudness);
      loudness.connect(audio.destination);
      tone.start(start);
      tone.stop(start + 1.7);
    });
  } catch (error) {
    // No sound on this browser; the note on the screen still shows.
  }
}

const announced = {};
/// Announce any event whose moment (its start, less the minutes asked for) has just come.
function announceEvents(now) {
  if (chime === null || asleep) return;
  events.forEach((e) => {
    if (e.allDay) return;
    const due = e.start.getTime() - chime * 60 * 1000, key = e.title + '|' + e.start.getTime() + '|' + chime;
    if (announced[key] || now < due || now - due > LATE_MS) return;
    announced[key] = true;
    $('soon-when').textContent = chime ? 'In ' + chime + ' minutes  ·  ' + clockText(e.start) : 'Now  ·  ' + clockText(e.start);
    $('soon-what').textContent = e.title;
    $('soon').classList.add('on');
    clearTimeout(soonTimer);
    soonTimer = setTimeout(() => $('soon').classList.remove('on'), SOON_MS);
    playChime();
  });
}

let dayShown = '';
function tick() {
  const now = new Date();
  const date = now.toLocaleDateString(LOCALE, { weekday: 'long', day: 'numeric', month: 'long' });
  $('time').textContent = clockText(now);
  $('date').textContent = date;
  $('today').textContent = date;
  $('now').textContent = clockText(now);
  if (dayShown && dayShown !== date) memoriesFirst();  // a new day has its own memories
  dayShown = date;
  drawCalendar(now, now.getMinutes() === 0);
  applyTheme();
  setTimeout(tick, 1000 * (60 - now.getSeconds()) + 50);
}
setInterval(() => { announceEvents(Date.now()); remind(Date.now()); }, 10 * 1000);

// --- settings ------------------------------------------------------------------------

function choices(box, list, chosen, choose) {
  box.textContent = '';
  list.forEach((choice) => {
    const button = element('button', 'press' + (choice[0] === chosen ? ' on' : ''), choice[1]);
    button.onclick = () => choose(choice[0]);
    box.appendChild(button);
  });
}

let theme = 'dark';

/// Put on the colours that are chosen. Called when they are chosen, every minute, and when
/// the weather (which knows whether it is day) comes in.
function applyTheme() {
  const hour = new Date().getHours();
  const day = weather && weather.now && typeof weather.now.day === 'boolean' ? weather.now.day : hour >= DAY_FROM && hour < DAY_UNTIL;
  document.body.classList.toggle('light', theme === 'light' || (theme === 'auto' && day));
}

function drawSettings() {
  choices($('themes'), THEMES, theme, (chosen) => {
    theme = chosen;
    remember('frameScreenTheme', theme);
    applyTheme();
    drawSettings();
    touched();
  });
  const show = (change) => {
    const before = slideshow;
    slideshow = Object.assign({}, slideshow, change);
    // Start the line again in the new order, or with the photos now chosen.
    if (slideshow.order !== before.order || !sameChoice(slideshow.show, before.show)) queue = [];
    if (change.memories) memoriesFirst();
    drawSettings();
    drawWeather();
    drawCalendar(new Date(), true);
    touched();
    send('/api/slideshow', slideshow).catch(() => {});
  };
  // Which photos: all, the favourites, or one of the years there are photos from.
  const years = [];
  photos.forEach((p) => { if (years.indexOf(yearOf(p)) < 0) years.push(yearOf(p)); });
  const favourites = photos.filter((p) => p[4]).length;
  const which = [['all', 'Everything'], ['favourites', '★ Favourites']].concat(years.sort().map((year) => [year, String(year)]));
  choices($('shows'), which, slideshow.show && slideshow.show.year ? slideshow.show.year : slideshow.show,
    (choice) => show({ show: typeof choice === 'number' ? { year: choice } : choice }));
  $('shows-note').textContent = slideshow.show === 'favourites' && !favourites
    ? 'There are no favourites yet, so everything is shown. Press and hold a photo to make it one.'
    : slideshow.show && slideshow.show.year && years.indexOf(slideshow.show.year) < 0
      ? 'There are no photos from ' + slideshow.show.year + ', so everything is shown.'
      : 'Press and hold a photo on Pictures to make it a favourite (★ ' + favourites + ' so far) or to hide it.';
  choices($('paces'), PACES, slideshow.every, (every) => show({ every: every }));
  choices($('orders'), ORDERS, slideshow.order, (order) => show({ order: order }));
  choices($('rests'), RESTS, slideshow.home_after, (minutes) => show({ home_after: minutes }));
  choices($('memories'), ON_OFF, slideshow.memories, (on) => show({ memories: on }));
  choices($('show-weather'), ON_OFF, slideshow.weather, (on) => show({ weather: on }));
  choices($('morning-on'), ON_OFF, slideshow.morning, (on) => show({ morning: on }));

  const dark = (change) => {
    night = Object.assign({}, night, change);
    drawSettings();
    send('/api/night', night).catch(() => {});
  };
  choices($('night-on'), ON_OFF, night.on, (on) => dark({ on: on }));
  choices($('night-dark'), ON_OFF, night.when_dark, (on) => dark({ when_dark: on }));
  $('night-from').textContent = night.from;
  $('night-until').textContent = night.until;
  const steppers = document.querySelectorAll('[data-hours]');
  for (let i = 0; i < steppers.length; i++) {
    steppers[i].onclick = () => {
      const which = steppers[i].dataset.hours, parts = night[which].split(':').map(Number);
      const minutes = (parts[0] * 60 + parts[1] + Number(steppers[i].dataset.by) + 1440) % 1440;
      const change = {};
      change[which] = ('0' + Math.floor(minutes / 60)).slice(-2) + ':' + ('0' + (minutes % 60)).slice(-2);
      dark(change);
    };
  }

  choices($('chimes'), CHIMES, chime, (before) => {
    chime = before;
    drawSettings();
    send('/api/chime', { before: chime }).catch(() => {});
  });
}

let deviceDrawn = '';
async function refreshDevice() {
  let device;
  try {
    device = await (await fetch('/api/device', { cache: 'no-store' })).json();
  } catch (error) {
    return;
  }
  // A PIN set from a phone while Settings is open here locks it here too.
  const locked = document.body.classList.contains('locked');
  if (device.pin && !token && !locked) askForPin();
  if (!device.pin || token) document.body.classList.remove('locked');
  const key = JSON.stringify(device);
  if (key === deviceDrawn) return;
  deviceDrawn = key;
  // Someone may have changed these from a phone while this was open.
  if (JSON.stringify(device.night) !== JSON.stringify(night)) {
    night = device.night;
    drawSettings();
  }
  const wifi = device.wifi;
  const name = $('wifi-name'), detail = $('wifi-detail');
  if (!wifi) {
    name.textContent = 'The frame did not say';
    name.className = 'big';
    detail.textContent = '';
  } else if (wifi.network) {
    const strength = wifi.signal === null ? '' : wifi.signal >= -60 ? 'Strong signal' : wifi.signal >= -72 ? 'Good signal' : 'Weak signal';
    name.textContent = wifi.network;
    name.className = 'big good';
    detail.textContent = strength + (device.simulated ? ' · simulated on this computer' : '');
  } else {
    name.textContent = wifi.on ? 'Not connected' : 'Wi-Fi is off';
    name.className = 'big bad';
    detail.textContent = 'Photos and the calendar cannot update until the frame is on Wi-Fi.';
  }
  $('address').textContent = device.address || 'Connect to Wi-Fi first';
  const code = $('code');
  code.style.display = device.address ? 'block' : 'none';
  // Asked for again whenever the address changes, as it does on another network.
  if (device.address) code.src = '/api/address.svg?for=' + encodeURIComponent(device.address);
  $('place').textContent = device.place || 'Not chosen yet';
  const light = device.light;
  $('night-note').textContent = 'The screen comes back on by itself when the night is over. To use the frame while it is off, press its power button.'
    + (typeof light === 'number' ? ' The room now measures ' + Math.round(light) + ' lux; under 4 counts as dark.' : '');
}

$('wifi-change').onclick = async () => {
  const note = $('wifi-note');
  try {
    await send('/api/wifi/settings');
    note.textContent = 'Opening the list of networks… The frame comes back here a minute after the last touch.';
  } catch (error) {
    note.textContent = 'The list of networks could not be opened.';
  }
};
$('chime-test').onclick = playChime;

// Leaving this screen takes two touches, so it does not happen by accident.
let confirming = 0;
$('use-original').onclick = async () => {
  const button = $('use-original');
  if (!confirming) {
    button.textContent = 'Touch again to switch';
    confirming = setTimeout(() => { confirming = 0; button.textContent = 'Use the original screen'; }, 5000);
    return;
  }
  clearTimeout(confirming);
  confirming = 0;
  button.textContent = 'Switching…';
  try {
    await send('/api/screen', { show: 'legacy' });
  } catch (error) {
    button.textContent = 'Could not switch. Touch to try again';
  }
};

// --- the PIN -------------------------------------------------------------------------

/// A keypad for the PIN in `keys`, with the dots typed so far in `dots` and what went wrong in
/// `note`. `entered` is given the token a right PIN bought. Returns what clears the keypad.
function keypad(keys, dots, note, entered) {
  let typed = '';
  const show = () => { dots.textContent = '•'.repeat(typed.length); };
  const tryPin = async () => {
    const pin = typed;
    typed = '';
    show();
    if (!pin) return;
    try {
      const bought = (await send('/api/unlock', { pin: pin })).token;
      note.textContent = '';
      entered(bought);
    } catch (error) {
      note.textContent = error.message;
    }
  };
  ['1', '2', '3', '4', '5', '6', '7', '8', '9', '⌫', '0', 'OK'].forEach((key) => {
    const button = element('button', key === 'OK' ? 'go' : '', key);
    button.onclick = () => {
      if (key === 'OK') return tryPin();
      typed = key === '⌫' ? typed.slice(0, -1) : (typed + key).slice(0, 8);
      note.textContent = '';
      show();
    };
    keys.appendChild(button);
  });
  return () => { typed = ''; note.textContent = ''; show(); };
}

const clearPin = keypad($('keys'), $('pin-dots'), $('pin-note'), (bought) => {
  token = bought;
  document.body.classList.remove('locked');
  refreshDevice();
});

/// Hide the settings behind the keypad.
function askForPin() {
  token = null;
  clearPin();
  document.body.classList.add('locked');
}

/// Give up what the PIN bought, so the next person to open Settings is asked again.
function lockSettings() {
  if (token) send('/api/lock').catch(() => {});
  token = null;
  clearPin();
}

/// Tell the server this screen is running, and learn how things are set.
async function sayAlive() {
  try {
    const reply = await (await fetch('/api/alive' + aliveNeeds(), { cache: 'no-store' })).json();
    const wanted = reply.slideshow;
    if (wanted && JSON.stringify(wanted) !== JSON.stringify(slideshow)) {
      if (wanted.order !== slideshow.order || !sameChoice(wanted.show, slideshow.show)) queue = [];
      slideshow = wanted;
      drawSettings();
      drawWeather();
      drawCalendar(new Date(), true);
      // A frame set to stay where it is left comes back up on the Calendar if that was open.
      if (!heard && wanted.home_after === null && remembered('frameTab') === 'calendar' && !params.get('tab')) showTab('calendar');
      touched();
    }
    heard = true;
    if (reply.chime !== chime) {
      chime = reply.chime;
      drawSettings();
    }
    pinSet = !!reply.pin;
    // An event added, a calendar or a trip changed: shown now, not at the calendar's next turn.
    if (calendarKnown !== null && reply.calendar !== calendarKnown) {
      forgetPlans();
      refreshCalendar();
    }
    calendarKnown = reply.calendar;
    if (todoKnown !== null && reply.todo !== todoKnown) todoNews();
    todoKnown = reply.todo;
    // Reminder rules changed from a phone take effect now, not at the calendar's next turn.
    if (!sameChoice(reply.reminders || [], rulesKnown)) {
      rulesKnown = reply.reminders || [];
      refreshCalendar();
    }
    // A place chosen a moment ago, or a fresh forecast, shows without waiting for the next turn.
    if ((reply.weather || null) !== (weather ? weather.fetched : null)) refreshWeather();
    // Home, in Utilities, is there only while the house is connected.
    document.body.classList.toggle('no-home', !reply.home);
    if (!reply.home && document.body.dataset.tool === 'home') choose('tools', 'tool', 'timers');
    asleep = !!reply.asleep;
    document.body.classList.toggle('asleep', asleep);
    schedule();
  } catch (error) {
    // The server will hear from us at the next turn.
  }
  setTimeout(sayAlive, ALIVE_MS);
}

// --- the tabs ------------------------------------------------------------------------

function remember(name, value) {
  try { localStorage.setItem(name, value); } catch (error) { /* nowhere to remember it */ }
}
function remembered(name) {
  try { return localStorage.getItem(name); } catch (error) { return null; }
}

/// Mark which of a row of buttons is chosen, and say so on <body> for the styles to act on.
function choose(row, what, name) {
  document.body.dataset[what] = name;
  const buttons = $(row).querySelectorAll('[data-' + what + ']');
  for (let i = 0; i < buttons.length; i++) buttons[i].classList.toggle('active', buttons[i].dataset[what] === name);
}

function showTab(name) {
  tab = name;
  choose('tabs', 'tab', name);
  // Settings is somewhere to visit; the frame comes back up on Pictures or Calendar.
  if (name !== 'settings') remember('frameTab', name);
  clearInterval(deviceTimer);
  if (name === 'settings') {
    refreshDevice();
    deviceTimer = setInterval(refreshDevice, 5000);
  } else {
    lockSettings();
  }
  // The calendar opens on today. ("Coming up" is also measured to see what fits, which
  // cannot be done while it is out of sight.)
  if (name === 'calendar') showToday();
  if (name === 'todo') todoNews();
  touched();
  schedule();
}

/// Something was touched: keep the tabs up a little longer, and do not leave Settings yet.
function touched() {
  clearTimeout(barTimer);
  clearTimeout(leaveTimer);
  if (tab === 'pictures' && document.body.classList.contains('bar')) {
    barTimer = setTimeout(() => document.body.classList.remove('bar'), BAR_MS);
  }
  // Left alone, Calendar and Settings give way to Pictures. If set never to, Settings still
  // gives way, to whichever of the other two was open before it.
  if (tab !== 'pictures' && slideshow.home_after !== null) {
    leaveTimer = setTimeout(() => {
      // Not while a timer is counting down on the tab that is open; look again later.
      if (tab === 'utilities' && staysOpen()) touched();
      else showTab('pictures');
    }, slideshow.home_after * 60 * 1000);
  } else if (tab === 'settings') {
    leaveTimer = setTimeout(() => showTab(remembered('frameTab') === 'calendar' ? 'calendar' : 'pictures'), LEAVE_SETTINGS_MS);
  }
}

$('tabs').addEventListener('click', (event) => {
  event.stopPropagation();
  const name = event.target.dataset.tab;
  if (name) showTab(name);
  else if (event.target.id === 'pause') setPaused(!paused);
  touched();
});
$('views').addEventListener('click', (event) => {
  const view = event.target.dataset.view;
  if (event.target.dataset.move) return move(Number(event.target.dataset.move));
  if (event.target.id === 'to-today') return showToday();
  if (!view) return;
  choose('views', 'view', view);
  remember('frameView', view);
  browse();
});
// A touch on a day picks it. From the week that goes to the month, where there is room to list the day.
$('grid').addEventListener('click', (event) => {
  const cell = event.target.closest('[data-day]');
  if (cell) pick(new Date(Number(cell.dataset.day)));
});
$('week').addEventListener('click', (event) => {
  const box = event.target.closest('[data-day]');
  if (!box) return;
  const day = new Date(Number(box.dataset.day)), now = new Date();
  picked = null;
  cursor = day.getMonth() === now.getMonth() && day.getFullYear() === now.getFullYear() ? null : day;
  if (!cursor) browsed = events;
  choose('views', 'view', 'month');
  pick(day);
  browse();
});
// A swipe across the calendar moves it a month or a week.
let swipeFrom = null;
$('calendar').addEventListener('touchstart', (event) => {
  swipeFrom = event.touches.length === 1 ? [event.touches[0].clientX, event.touches[0].clientY] : null;
}, { passive: true });
$('calendar').addEventListener('touchend', (event) => {
  if (!swipeFrom) return;
  const across = event.changedTouches[0].clientX - swipeFrom[0], down = event.changedTouches[0].clientY - swipeFrom[1];
  swipeFrom = null;
  if (Math.abs(across) > 0.08 * window.innerWidth && Math.abs(across) > 2 * Math.abs(down)) move(across < 0 ? 1 : -1);
}, { passive: true });
$('parts').addEventListener('click', (event) => {
  if (event.target.dataset.part) choose('parts', 'part', event.target.dataset.part);
});

// On Pictures the left and right quarters step through photos and the middle brings up the tabs.
document.addEventListener('click', (event) => {
  if (tab !== 'pictures') return touched();
  const x = event.clientX / window.innerWidth;
  if (x < 0.25) step(-1);
  else if (x > 0.75) step(1);
  else document.body.classList.toggle('bar');
  touched();
});
document.addEventListener('keydown', (event) => {
  if (event.key === 'ArrowLeft') step(-1);
  else if (event.key === 'ArrowRight') step(1);
  else if (event.key === ' ') setPaused(!paused);
  else if (event.key === '1') showTab('pictures');
  else if (event.key === '2') showTab('calendar');
  else if (event.key === '3') showTab('todo');
  else if (event.key === '4') showTab('utilities');
  else if (event.key === '5') showTab('settings');
});

// Every tab gets its drawing.
const withIcons = document.querySelectorAll('[data-icon]');
for (let i = 0; i < withIcons.length; i++) label(withIcons[i], withIcons[i].dataset.icon, withIcons[i].textContent);

theme = params.get('theme') || remembered('frameScreenTheme') || 'dark';
if (!THEMES.some((one) => one[0] === theme)) theme = 'dark';
tick();
drawSettings();
$('to-today').style.visibility = 'hidden';
choose('views', 'view', params.get('view') || (remembered('frameView') === 'week' ? 'week' : 'month'));
choose('parts', 'part', params.get('part') || 'pictures');
showTab(params.get('tab') || 'pictures');
refreshPhotos();
refreshCalendar();
refreshWeather();
sayAlive();
