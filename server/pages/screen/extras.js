// Two things that sit over the frame's tabs: the menu a photo gets when it is pressed and
// held (make it a favourite, or hide it), and the reminders that calendar events bring up.
//
// Loaded after screen.js, whose helpers it uses.

// How long a photo must be held for its menu, and how far a finger may wander meanwhile.
const HOLD_MS = 650, HOLD_SLACK = 12;
// The menu closes by itself when left alone for this long.
const MENU_FOR_MS = 40 * 1000;
// At most this many reminders are on the screen at once, the soonest first.
const MOST_REMINDERS = 4;
// A reminder that was due no longer ago than this still sounds its chime.
const CHIME_LATE_MS = 90 * 1000;

// --- a photo's menu ---------------------------------------------------------------------

let menuPhoto = null;   // the photo the menu is about
let holdTimer = 0, holdFrom = null, menuTimer = 0;
let swallow = false;    // the touch that opened the menu must not also count as a tap

/// The photo on screen at a point, if the point is on one. A photo alone on the screen
/// answers for the black beside it too.
function photoAt(x, y) {
  const screen = shown[at], box = slides[front].querySelector('.collage');
  if (!screen || !box) return null;
  const frame = box.getBoundingClientRect();
  const across = (x - frame.left) / frame.width, down = (y - frame.top) / frame.height;
  const cell = screen.cells.filter((c) => across >= c.x && across < c.x + c.w && down >= c.y && down < c.y + c.h)[0];
  return cell ? cell.photo : screen.cells.length === 1 ? screen.cells[0].photo : null;
}

function drawMenu() {
  // With a PIN set, the menu is offered only once it has been typed.
  const locked = pinSet && !token;
  $('menu').classList.toggle('asking', locked);
  $('menu-said').textContent = locked ? 'Enter the PIN to mark this photo' : 'This photo';
  $('menu-favourite').textContent = menuPhoto[4] ? '★  No longer a favourite' : '☆  Make it a favourite';
  $('menu-favourite').classList.toggle('on', !menuPhoto[4]);
}

function openMenu(photo) {
  menuPhoto = photo;
  over = true;
  clearTimeout(timer);
  clearMenuPin();
  $('menu-picture').src = url('file', photo[0]);
  drawMenu();
  $('menu').classList.add('on');
  stayOpen();
}

function closeMenu() {
  if (!menuPhoto) return;
  menuPhoto = null;
  over = false;
  clearTimeout(menuTimer);
  $('menu').classList.remove('on');
  lockSettings();  // what the PIN bought was for this photo only
  schedule();
}

function stayOpen() {
  clearTimeout(menuTimer);
  menuTimer = setTimeout(closeMenu, MENU_FOR_MS);
}

/// A few words at the top of the screen for a moment, as for an event about to start.
function say(what, more) {
  $('soon-when').textContent = more;
  $('soon-what').textContent = what;
  $('soon').classList.add('on');
  clearTimeout(soonTimer);
  soonTimer = setTimeout(() => $('soon').classList.remove('on'), 8000);
}

async function markPhoto(mark) {
  const photo = menuPhoto;
  try {
    await send('/api/mark', { photo: photo[0], mark: mark });
  } catch (error) {
    if (/PIN/.test(error.message)) {
      token = null;
      pinSet = true;
      return drawMenu();
    }
    return say('That did not work', error.message);
  }
  photo[4] = mark === 'favourite';
  closeMenu();
  if (mark === 'hidden') {
    // It leaves the line and the screen; the photos that shared the screen with it come round again.
    const rest = (shown[at] ? shown[at].cells : []).map((cell) => cell.photo).filter((p) => p !== photo);
    photos = photos.filter((p) => p !== photo);
    queue = rest.concat(queue.filter((p) => p !== photo && rest.indexOf(p) < 0));
    shown = [];
    at = -1;
    say('Hidden from the frame', 'It can be shown again from a phone or computer');
    step(1);
  } else {
    say(photo[4] ? '★  A favourite' : 'No longer a favourite', photo[4] ? 'It will come round more often' : '');
    queue = queue.filter((p, i) => p !== photo || queue.indexOf(p) === i);  // its second turn, if it had one
    if (shown[at]) draw(shown[at]);
    drawSettings();
  }
}

const clearMenuPin = keypad($('menu-keys'), $('menu-dots'), $('menu-note'), (bought) => {
  token = bought;
  drawMenu();
});
$('menu-favourite').onclick = () => markPhoto(menuPhoto[4] ? null : 'favourite');
$('menu-hide').onclick = () => markPhoto('hidden');
$('menu-close').onclick = closeMenu;
// Touches on the menu are the menu's own: they neither step through photos nor bring up the tabs.
$('menu').addEventListener('click', (event) => {
  event.stopPropagation();
  stayOpen();
  touched();
});

function holdStarted(x, y) {
  clearTimeout(holdTimer);
  if (tab !== 'pictures' || asleep || busy || menuPhoto || document.body.classList.contains('bar')) return;
  holdFrom = [x, y];
  holdTimer = setTimeout(() => {
    const photo = photoAt(x, y);
    if (!photo || busy) return;
    swallow = true;
    openMenu(photo);
  }, HOLD_MS);
}
function holdMoved(x, y) {
  if (holdFrom && Math.abs(x - holdFrom[0]) + Math.abs(y - holdFrom[1]) > HOLD_SLACK) clearTimeout(holdTimer);
}
const holdOver = () => clearTimeout(holdTimer);

const pictures = $('pictures');
pictures.addEventListener('touchstart', (event) => {
  if (event.touches.length === 1) holdStarted(event.touches[0].clientX, event.touches[0].clientY);
  else holdOver();
}, { passive: true });
pictures.addEventListener('touchmove', (event) => holdMoved(event.touches[0].clientX, event.touches[0].clientY), { passive: true });
pictures.addEventListener('touchend', holdOver);
pictures.addEventListener('touchcancel', holdOver);
pictures.addEventListener('mousedown', (event) => holdStarted(event.clientX, event.clientY));
pictures.addEventListener('mousemove', (event) => holdMoved(event.clientX, event.clientY));
pictures.addEventListener('mouseup', holdOver);
pictures.addEventListener('mouseleave', holdOver);
// Lifting the finger that opened the menu is not a tap on what is under it. The next touch,
// wherever it lands, is a touch of its own.
['touchstart', 'mousedown'].forEach((kind) => document.addEventListener(kind, () => { swallow = false; }, true));
document.addEventListener('click', (event) => {
  if (!swallow) return;
  swallow = false;
  event.stopPropagation();
  event.preventDefault();
}, true);
// A long press is ours; the browser's own menu for it stays away.
document.addEventListener('contextmenu', (event) => event.preventDefault());

// --- reminders --------------------------------------------------------------------------

// The reminders touched away, by what they were for, each with when it would have left by
// itself (after which it is forgotten). Kept across restarts of the page.
let done = {};
try { done = JSON.parse(remembered('frameReminded') || '{}') || {}; } catch (error) { done = {}; }
const chimed = {};
let remindersDrawn = '';

/// When a rule's reminder for an event comes up: so many minutes before it starts, or at a
/// time of day so many days before.
function dueFor(event, rule) {
  if (typeof rule.when.minutes === 'number') return new Date(event.start.getTime() - rule.when.minutes * 60 * 1000);
  const time = rule.when.at.split(':').map(Number), day = dayAfter(startOfDay(event.start), -rule.when.days);
  return new Date(day.getFullYear(), day.getMonth(), day.getDate(), time[0], time[1]);
}

/// The reminders that are up at a moment: due, not yet over, and not touched away.
function remindersAt(now) {
  const up = [];
  events.forEach((event) => {
    event.rules.forEach((rule) => {
      const due = dueFor(event, rule).getTime();
      // It stays until the event is over, and at least an hour.
      const until = Math.max(event.end.getTime(), event.start.getTime(), due + 60 * 60 * 1000);
      const key = rule.words + '|' + event.title + '|' + event.start.getTime();
      if (now >= due && now < until && !done[key]) up.push({ key: key, event: event, rule: rule, due: due, until: until });
    });
  });
  // One note for one event, even if two rules are about it: the first rule's.
  const seen = {};
  return up.sort((x, y) => x.event.start - y.event.start)
    .filter((r) => !seen[r.event.title + r.event.start] && (seen[r.event.title + r.event.start] = true))
    .slice(0, MOST_REMINDERS);
}

function dismiss(reminder) {
  done[reminder.key] = reminder.until;
  const now = Date.now();
  Object.keys(done).forEach((key) => { if (done[key] < now) delete done[key]; });
  remember('frameReminded', JSON.stringify(done));
  remind(now);
}

remind = (now) => {
  const up = remindersAt(now), today = new Date(now);
  up.forEach((r) => {
    if (chimed[r.key]) return;
    chimed[r.key] = true;
    if (r.rule.chime && !asleep && now - r.due < CHIME_LATE_MS) playChime();
  });
  const key = JSON.stringify(up.map((r) => [r.key, r.rule.say, dayText(r.event, today)]));
  if (key === remindersDrawn) return;
  remindersDrawn = key;
  const box = $('reminders');
  box.textContent = '';
  up.forEach((r) => {
    const when = dayText(r.event, today) + (r.event.allDay ? '' : '  ·  ' + clockText(r.event.start));
    const note = element('button', 'reminder', '');
    note.appendChild(element('div', 'what', r.rule.say || r.event.title));
    note.appendChild(element('div', 'when', (r.rule.say ? r.event.title + '  ·  ' : '') + when));
    note.appendChild(element('div', 'tick', '✓'));
    note.onclick = (event) => {
      event.stopPropagation();
      dismiss(r);
      touched();
    };
    box.appendChild(note);
  });
};
