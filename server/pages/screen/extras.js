// Things that sit over the frame's tabs: the menu a photo gets when it is pressed and held
// (make it a favourite, or hide it), the reminders that calendar events bring up, and
// the note that says when to leave for an event that is travelled to.
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

// --- when to leave ----------------------------------------------------------------------

// An event that is travelled to gets a note this long before it is time to leave.
const LEAVE_NOTE_MS = 30 * 60 * 1000;
// Plans are asked for events starting within this long, and asked again this often (the
// journey planner knows about disruptions only near the time).
const PLAN_AHEAD_MS = 24 * 60 * 60 * 1000, PLAN_AGAIN_MS = 10 * 60 * 1000;
const plans = {};       // by trip and start: { leave, catch, steps, asked }
const listedPlans = {}; // the plans the calendar's lists want, however far off their events are
let planning = false;

const planKey = (event) => event.trip + '|' + event.start.getTime();

/// Ask the server when to leave for the events coming up that have a trip, one at a time.
async function refreshPlans(now) {
  if (planning) return;
  const next = events.concat(browsed === events ? [] : browsed).filter((event) => {
    const away = event.start.getTime() - now, plan = plans[planKey(event)];
    // Asked again often when it is near; a plan for next week, shown in a list, keeps for an hour.
    const again = away < PLAN_AHEAD_MS ? PLAN_AGAIN_MS : 6 * PLAN_AGAIN_MS;
    return event.trip !== null && !event.allDay && away > 0 && (away < PLAN_AHEAD_MS || listedPlans[planKey(event)])
      && (!plan || now - plan.asked > again);
  })[0];
  if (!next) return;
  planning = true;
  const key = planKey(next), before = plans[key];
  // Whatever comes of it, this one is not asked about again for a while.
  plans[key] = Object.assign({}, before, { asked: now });
  try {
    const reply = await fetch('/api/travel?trip=' + next.trip + '&arrive=' + Math.floor(next.start.getTime() / 1000), { cache: 'no-store' });
    const said = await reply.json();
    plans[key] = reply.ok ? Object.assign(said, { asked: now }) : Object.assign({}, before, { asked: now, error: said.error });
  } catch (error) {
    // The plan we had, if any, stands.
  }
  planning = false;
  if (tab === 'calendar') drawCalendar(new Date(), true);
  drawDetail();
  remind(Date.now());
}

// --- an event looked at closely ---------------------------------------------------------

const BY = { transit: 'by public transport', walk: 'on foot', cycle: 'by bicycle', car: 'by car', park: 'by car, then public transport' };
let detailed = null;    // the event being looked at
let detailTimer = 0;

function drawDetail() {
  const event = detailed;
  if (!event) return;
  const day = event.start.toLocaleDateString(LOCALE, { weekday: 'long', day: 'numeric', month: 'long' });
  $('detail-what').textContent = event.title;
  $('detail-about').textContent = day + (event.allDay ? '' : '  ·  ' + clockText(event.start))
    + (event.place ? '  ·  ' + event.place : '') + (event.from ? '  ·  ' + event.from : '');
  const plan = event.trip === null || event.allDay ? null : plans[planKey(event)];
  const steps = $('detail-steps');
  steps.textContent = '';
  let leave = '', more = '';
  if (event.trip === null || event.allDay) {
    more = event.allDay ? '' : 'No trip is set up for this. Trips are set up from a phone or computer, under Calendar, then Getting there.';
  } else if (event.start <= Date.now()) {
    more = 'It has started.';
  } else if (plan && typeof plan.leave === 'number') {
    leave = 'Leave by ' + clockText(new Date(plan.leave * 1000));
    more = 'Arriving ' + clockText(new Date(plan.arrive * 1000)) + '  ·  ' + (BY[plan.how] || '')
      + (plan.margin ? '  ·  ' + plan.margin + ' min to spare' : '');
    (plan.steps || []).forEach((step) => steps.appendChild(element('li', '', step)));
  } else if (plan && plan.error) {
    more = 'The way there could not be worked out: ' + plan.error;
  } else {
    more = 'Working out when to leave…';
    listedPlans[planKey(event)] = true;
    refreshPlans(Date.now());
  }
  $('detail-leave').textContent = leave;
  $('detail-more').textContent = more;
}

showEvent = (event) => {
  detailed = event;
  drawDetail();
  $('detail').classList.add('on');
  clearTimeout(detailTimer);
  detailTimer = setTimeout(closeDetail, 2 * 60 * 1000);
};
function closeDetail() {
  detailed = null;
  clearTimeout(detailTimer);
  $('detail').classList.remove('on');
}
$('detail-close').onclick = closeDetail;
$('detail').addEventListener('click', (event) => {
  event.stopPropagation();
  touched();
});

/// For the calendar's lists: when to leave for an event and how, once that is known.
leaving = (event) => {
  if (event.trip === null || event.allDay || event.start <= Date.now()) return '';
  listedPlans[planKey(event)] = true;
  const plan = plans[planKey(event)];
  if (!plan) {
    refreshPlans(Date.now());
    return 'Working out when to leave…';
  }
  if (typeof plan.leave !== 'number') return plan.error ? 'No way there was worked out' : '';
  const how = plan.catch || (plan.steps || [])[0] || '';
  return 'Leave by ' + clockText(new Date(plan.leave * 1000)) + (how ? '  ·  ' + how : '');
};

/// The notes about leaving that are up at a moment.
function leavingAt(now) {
  const up = [];
  events.forEach((event) => {
    const plan = event.trip === null || event.allDay ? null : plans[planKey(event)];
    if (!plan || typeof plan.leave !== 'number') return;
    const leave = plan.leave * 1000, due = leave - LEAVE_NOTE_MS, until = event.start.getTime();
    const key = 'leave|' + event.title + '|' + until;
    if (now < due || now >= until || done[key]) return;
    const how = plan.catch || (plan.steps || [])[0] || '';
    up.push({
      key: key, event: event, due: due, until: until, chime: true,
      // It sounds when it comes up and again when it is time to go.
      sounds: [[key, due], [key + '|go', leave]],
      what: now < leave ? 'Leave by ' + clockText(new Date(leave)) : 'Time to leave',
      when: event.title + '  ·  ' + clockText(event.start) + (how ? '  ·  ' + how : ''),
    });
  });
  return up;
}

/// The reminders that are up at a moment: due, not yet over, and not touched away.
function remindersAt(now) {
  const up = [], today = new Date(now);
  events.forEach((event) => {
    event.rules.forEach((rule) => {
      const due = dueFor(event, rule).getTime();
      // It stays until the event is over, and at least an hour.
      const until = Math.max(event.end.getTime(), event.start.getTime(), due + 60 * 60 * 1000);
      const key = rule.words + '|' + event.title + '|' + event.start.getTime();
      if (now < due || now >= until || done[key]) return;
      const when = dayText(event, today) + (event.allDay ? '' : '  ·  ' + clockText(event.start));
      up.push({
        key: key, event: event, due: due, until: until, chime: rule.chime, sounds: [[key, due]],
        what: rule.say || event.title, when: (rule.say ? event.title + '  ·  ' : '') + when,
      });
    });
  });
  // One note for one event, even if two rules are about it: the first rule's.
  const seen = {};
  const rules = up.filter((r) => !seen[r.event.title + r.event.start] && (seen[r.event.title + r.event.start] = true));
  // Leaving comes before everything else; then whatever is soonest.
  return leavingAt(now).concat(rules.sort((x, y) => x.event.start - y.event.start)).slice(0, MOST_REMINDERS);
}

function dismiss(reminder) {
  done[reminder.key] = reminder.until;
  const now = Date.now();
  Object.keys(done).forEach((key) => { if (done[key] < now) delete done[key]; });
  remember('frameReminded', JSON.stringify(done));
  remind(now);
}

remind = (now) => {
  if (detailed && tab !== 'calendar') closeDetail();  // the screen went back to Pictures by itself
  refreshPlans(now);
  const up = remindersAt(now);
  up.forEach((r) => {
    r.sounds.forEach((sound) => {
      if (chimed[sound[0]] || now < sound[1]) return;
      chimed[sound[0]] = true;
      if (r.chime && !asleep && now - sound[1] < CHIME_LATE_MS) playChime();
    });
  });
  const key = JSON.stringify(up.map((r) => [r.key, r.what, r.when]));
  if (key === remindersDrawn) return;
  remindersDrawn = key;
  const box = $('reminders');
  box.textContent = '';
  up.forEach((r) => {
    const note = element('button', 'reminder', '');
    note.appendChild(element('div', 'what', r.what));
    note.appendChild(element('div', 'when', r.when));
    note.appendChild(element('div', 'tick', '✓'));
    note.onclick = (event) => {
      event.stopPropagation();
      dismiss(r);
      touched();
    };
    box.appendChild(note);
  });
};
