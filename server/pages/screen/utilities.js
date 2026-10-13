// The Utilities tab of the frame's screen: timers, alarms and a pomodoro clock.
//
// Everything here is kept in this browser's own storage, so it survives the page being
// loaded again and needs neither the server nor the PIN. The server is only told, each
// time the page says it is running, what this needs of the display: to stay on while
// something counts down, and to be on when an alarm is due (see `aliveNeeds`).
//
// Loaded after screen.js, whose helpers it uses.

const TIMER_MINUTES = [1, 3, 5, 10, 15, 30, 60];
const REPEATS = [['daily', 'Every day'], ['weekdays', 'Weekdays'], ['weekends', 'Weekends'], ['once', 'Once']];
const FOCUS_MINUTES = [15, 25, 45, 50], SHORT_MINUTES = [5, 10], LONG_MINUTES = [15, 30];
const MOST_TIMERS = 4, MOST_ALARMS = 6;
// A long break comes after this many rounds of focus.
const ROUNDS = 4;
// Ringing repeats this often and gives up after this long; a snoozed alarm rings again after this many minutes.
const RING_EVERY_MS = 2500, RING_FOR_MS = 3 * 60 * 1000, SNOOZE_MINUTES = 5;
// An alarm found later than this after its time (the page was not running) is let go.
const ALARM_LATE_MS = 2 * 60 * 1000;
const MINUTE_MS = 60 * 1000;

const kept = {
  read(name, otherwise) {
    try {
      const value = JSON.parse(localStorage.getItem(name));
      return value === null ? otherwise : value;
    } catch (error) {
      return otherwise;
    }
  },
  write(name, value) {
    try { localStorage.setItem(name, JSON.stringify(value)); } catch (error) { /* nowhere to keep it */ }
  },
};

let timers = kept.read('frameTimers', []);   // [{ id, minutes, ends }], `ends` in milliseconds since 1970
let alarms = kept.read('frameAlarms', []);   // [{ id, time: 'HH:MM', repeat, on, rang, snoozed }]
let pomodoro = Object.assign(
  // phase: idle, focus, short or long. `ends` while running; `left` (milliseconds) while paused.
  { focus: 25, short: 5, long: 15, phase: 'idle', round: 1, ends: 0, left: 0 },
  kept.read('framePomodoro', {}),
);
let customMinutes = 20;                      // the timer being set by hand
let draft = { hour: 7, minute: 0, repeat: 'daily' };  // the alarm being set
let ringing = [];                            // what is ringing, first come first: { title, detail, alarm }
let ringTimer = 0, ringSince = 0;

const newId = () => String(Date.now()) + Math.floor(Math.random() * 1000);
const twoDigits = (n) => ('0' + n).slice(-2);

/// Time left, as 4:59 or 1:04:59.
function countdown(ms) {
  const seconds = Math.max(0, Math.ceil(ms / 1000));
  const hours = Math.floor(seconds / 3600), minutes = Math.floor(seconds % 3600 / 60);
  return (hours ? hours + ':' + twoDigits(minutes) : minutes) + ':' + twoDigits(seconds % 60);
}
const minutesText = (minutes) => (minutes === 60 ? '1 hour' : minutes + ' min');

// --- ringing -------------------------------------------------------------------------

function ring(title, detail, alarm) {
  ringing.push({ title: title, detail: detail, alarm: alarm || null });
  if (ringing.length === 1) startRinging();
}

function startRinging() {
  const now = ringing[0];
  $('ring-what').textContent = now.title;
  $('ring-more').textContent = now.detail;
  $('ring-snooze').style.display = now.alarm ? '' : 'none';
  $('ring').classList.add('on');
  ringSince = Date.now();
  clearInterval(ringTimer);
  playChime();
  ringTimer = setInterval(() => {
    if (Date.now() - ringSince > RING_FOR_MS) stopRinging(false);  // nobody is there; leave it be
    else playChime();
  }, RING_EVERY_MS);
}

function stopRinging(snooze) {
  const was = ringing.shift();
  clearInterval(ringTimer);
  if (snooze && was && was.alarm) {
    const alarm = alarms.filter((a) => a.id === was.alarm)[0];
    if (alarm) {
      alarm.snoozed = Date.now() + SNOOZE_MINUTES * MINUTE_MS;
      kept.write('frameAlarms', alarms);
      drawAlarms();
    }
  }
  if (ringing.length) startRinging();
  else $('ring').classList.remove('on');
}

// --- timers --------------------------------------------------------------------------

function startTimer(minutes) {
  if (timers.length >= MOST_TIMERS) return;
  timers.push({ id: newId(), minutes: minutes, ends: Date.now() + minutes * MINUTE_MS });
  kept.write('frameTimers', timers);
  drawTimers();
}

function drawTimers() {
  $('timer-custom').textContent = minutesText(customMinutes);
  const list = $('timers');
  list.textContent = '';
  if (!timers.length) list.appendChild(element('p', '', 'No timer is running. Touch a length to start one; up to ' + MOST_TIMERS + ' can run at once.'));
  timers.forEach((timer) => {
    const entry = element('div', 'entry', '');
    entry.appendChild(element('span', 'big', '')).id = 'left-' + timer.id;
    entry.appendChild(element('span', 'about', minutesText(timer.minutes) + ' timer'));
    const cancel = element('button', 'press', 'Cancel');
    cancel.onclick = () => {
      timers = timers.filter((t) => t.id !== timer.id);
      kept.write('frameTimers', timers);
      drawTimers();
    };
    entry.appendChild(cancel);
    list.appendChild(entry);
  });
  everySecond();
}

// --- alarms --------------------------------------------------------------------------

/// Whether an alarm rings on a given day of the week.
function ringsOn(alarm, day) {
  const weekend = day.getDay() === 0 || day.getDay() === 6;
  return alarm.repeat === 'weekdays' ? !weekend : alarm.repeat === 'weekends' ? weekend : true;
}

/// When an alarm rings on a given day.
function onDay(alarm, day) {
  const parts = alarm.time.split(':').map(Number);
  return new Date(day.getFullYear(), day.getMonth(), day.getDate(), parts[0], parts[1]);
}

/// The next time an alarm will ring, or null if it is off.
function nextRing(alarm, now) {
  if (!alarm.on) return null;
  if (alarm.snoozed) return new Date(alarm.snoozed);
  for (let ahead = 0; ahead < 8; ahead++) {
    const at = onDay(alarm, dayAfter(startOfDay(now), ahead));
    if (at > now && ringsOn(alarm, at)) return at;
  }
  return null;
}

function drawAlarms() {
  $('alarm-hour').textContent = twoDigits(draft.hour);
  $('alarm-minute').textContent = twoDigits(draft.minute);
  choices($('alarm-repeats'), REPEATS, draft.repeat, (repeat) => { draft.repeat = repeat; drawAlarms(); });
  const list = $('alarms'), now = new Date();
  list.textContent = '';
  if (!alarms.length) list.appendChild(element('p', '', 'No alarms yet. An alarm rings even when the screen is off for the night.'));
  alarms.slice().sort((x, y) => (x.time < y.time ? -1 : 1)).forEach((alarm) => {
    const entry = element('div', 'entry' + (alarm.on ? '' : ' off'), '');
    entry.appendChild(element('span', 'big', alarm.time));
    const next = nextRing(alarm, now);
    const repeats = REPEATS.filter((r) => r[0] === alarm.repeat)[0][1];
    const about = !next ? 'Off' : alarm.snoozed ? 'Snoozed until ' + clockText(next)
      : 'Next: ' + dayText({ start: next }, now).toLowerCase();
    entry.appendChild(element('span', 'about', repeats + '  ·  ' + about));
    const toggle = element('button', 'press' + (alarm.on ? ' on' : ''), alarm.on ? 'On' : 'Off');
    toggle.onclick = () => {
      alarm.on = !alarm.on;
      alarm.snoozed = 0;
      kept.write('frameAlarms', alarms);
      drawAlarms();
    };
    const remove = element('button', 'press', 'Delete');
    remove.onclick = () => {
      alarms = alarms.filter((a) => a.id !== alarm.id);
      kept.write('frameAlarms', alarms);
      drawAlarms();
    };
    entry.appendChild(toggle);
    entry.appendChild(remove);
    list.appendChild(entry);
  });
}

/// Ring whichever alarms have come due.
function checkAlarms(now) {
  let changed = false;
  alarms.forEach((alarm) => {
    if (!alarm.on) return;
    const at = onDay(alarm, now), mark = dayKey(now) + ' ' + alarm.time;
    const due = ringsOn(alarm, now) && now >= at && now - at < ALARM_LATE_MS && alarm.rang !== mark;
    const snoozeOver = alarm.snoozed && now.getTime() >= alarm.snoozed;
    if (!due && !snoozeOver) return;
    alarm.rang = mark;
    alarm.snoozed = 0;
    // An alarm set for once has done its job; it stays in the list, off, to be used again.
    if (alarm.repeat === 'once' && due) alarm.on = false;
    changed = true;
    ring('Alarm', alarm.time, alarm.id);
  });
  if (changed) {
    kept.write('frameAlarms', alarms);
    drawAlarms();
  }
}

// --- the pomodoro clock ----------------------------------------------------------------

const PHASES = { idle: 'Ready', focus: 'Focus', short: 'Short break', long: 'Long break' };
const pomodoroRunning = () => pomodoro.phase !== 'idle' && !pomodoro.left;

function beginPhase(phase) {
  pomodoro.phase = phase;
  pomodoro.left = 0;
  pomodoro.ends = Date.now() + pomodoro[phase] * MINUTE_MS;
}

/// Move on to what follows the phase that has ended (or was skipped).
function nextPhase(told) {
  if (pomodoro.phase === 'focus') {
    beginPhase(pomodoro.round % ROUNDS === 0 ? 'long' : 'short');
  } else {
    pomodoro.round = pomodoro.phase === 'long' ? 1 : pomodoro.round + 1;
    beginPhase('focus');
  }
  kept.write('framePomodoro', pomodoro);
  drawPomodoro();
  if (!told) return;
  // A note and one chime, not an alarm: nothing has to be stopped.
  $('soon-when').textContent = 'Pomodoro';
  $('soon-what').textContent = pomodoro.phase === 'focus' ? 'Back to work: round ' + pomodoro.round : 'Time for a ' + PHASES[pomodoro.phase].toLowerCase();
  $('soon').classList.add('on');
  clearTimeout(soonTimer);
  soonTimer = setTimeout(() => $('soon').classList.remove('on'), 20 * 1000);
  playChime();
}

function drawPomodoro() {
  const running = pomodoroRunning();
  $('pomodoro').className = 'pomodoro ' + pomodoro.phase;
  $('pomo-phase').textContent = PHASES[pomodoro.phase] + (pomodoro.left ? '  ·  paused' : '');
  $('pomo-round').textContent = pomodoro.phase === 'idle' ? ROUNDS + ' rounds of focus, then a long break' : 'Round ' + pomodoro.round + ' of ' + ROUNDS;
  $('pomo-go').textContent = running ? 'Pause' : pomodoro.left ? 'Resume' : 'Start';
  const set = (name) => (minutes) => {
    pomodoro[name] = minutes;
    kept.write('framePomodoro', pomodoro);
    drawPomodoro();
  };
  choices($('pomo-focus'), FOCUS_MINUTES.map((m) => [m, m + ' min']), pomodoro.focus, set('focus'));
  choices($('pomo-short'), SHORT_MINUTES.map((m) => [m, m + ' min']), pomodoro.short, set('short'));
  choices($('pomo-long'), LONG_MINUTES.map((m) => [m, m + ' min']), pomodoro.long, set('long'));
  everySecond();
}

// --- keeping time ------------------------------------------------------------------------

function everySecond() {
  const now = Date.now();
  // Timers that have run out ring and leave the list.
  const done = timers.filter((timer) => now >= timer.ends);
  if (done.length) {
    timers = timers.filter((timer) => now < timer.ends);
    kept.write('frameTimers', timers);
    done.forEach((timer) => ring('Time is up', minutesText(timer.minutes) + ' timer'));
    drawTimers();
  }
  timers.forEach((timer) => {
    const left = $('left-' + timer.id);
    if (left) left.textContent = countdown(timer.ends - now);
  });
  if (pomodoroRunning() && now >= pomodoro.ends) nextPhase(true);
  const pomodoroLeft = pomodoro.phase === 'idle' ? pomodoro.focus * MINUTE_MS : pomodoro.left || pomodoro.ends - now;
  $('pomo-count').textContent = countdown(pomodoroLeft);
  checkAlarms(new Date(now));
  $('now-too').textContent = clockText(new Date(now));

  // Over the pictures: the timer that ends first, and the pomodoro if it is running.
  const soonest = timers.slice().sort((x, y) => x.ends - y.ends)[0];
  const showing = [];
  if (soonest) showing.push('⏱ ' + countdown(soonest.ends - now));
  if (pomodoroRunning()) showing.push(PHASES[pomodoro.phase] + ' ' + countdown(pomodoro.ends - now));
  const text = showing.join('   ·   ');
  if ($('running').textContent !== text) $('running').textContent = text;
}

// What this needs of the display, said to the server each time the page reports in: to
// stay on while something counts down or rings, and to be on when the next alarm is due.
aliveNeeds = () => {
  const now = Date.now();
  let busyUntil = ringing.length ? now + 5 * MINUTE_MS : 0;
  timers.forEach((timer) => { busyUntil = Math.max(busyUntil, timer.ends + MINUTE_MS); });
  if (pomodoroRunning()) busyUntil = Math.max(busyUntil, pomodoro.ends + 2 * MINUTE_MS);
  const rings = alarms.map((alarm) => nextRing(alarm, new Date(now))).filter(Boolean).map((at) => at.getTime());
  const asks = [];
  if (busyUntil > now) asks.push('busy=' + Math.ceil((busyUntil - now) / 1000));
  if (rings.length) asks.push('wake=' + Math.floor(Math.min.apply(null, rings) / 1000));
  return asks.length ? '?' + asks.join('&') : '';
};
// The tab is not left for Pictures while something is counting down on it.
staysOpen = () => timers.length > 0 || pomodoroRunning() || ringing.length > 0;

// --- wiring ------------------------------------------------------------------------------

choices($('timer-presets'), TIMER_MINUTES.map((m) => [m, minutesText(m)]), null, startTimer);
const steppers = document.querySelectorAll('[data-custom]');
for (let i = 0; i < steppers.length; i++) {
  steppers[i].onclick = () => {
    customMinutes = Math.min(600, Math.max(1, customMinutes + Number(steppers[i].dataset.custom)));
    drawTimers();
  };
}
$('timer-start').onclick = () => startTimer(customMinutes);

const turners = document.querySelectorAll('[data-alarm]');
for (let i = 0; i < turners.length; i++) {
  turners[i].onclick = () => {
    const by = Number(turners[i].dataset.by);
    if (turners[i].dataset.alarm === 'hour') draft.hour = (draft.hour + by + 24) % 24;
    else draft.minute = (draft.minute + by + 60) % 60;
    drawAlarms();
  };
}
$('alarm-add').onclick = () => {
  if (alarms.length >= MOST_ALARMS) return;
  alarms.push({ id: newId(), time: twoDigits(draft.hour) + ':' + twoDigits(draft.minute), repeat: draft.repeat, on: true, rang: '', snoozed: 0 });
  kept.write('frameAlarms', alarms);
  drawAlarms();
};

$('pomo-go').onclick = () => {
  if (pomodoro.phase === 'idle') {
    pomodoro.round = 1;
    beginPhase('focus');
  } else if (pomodoro.left) {
    pomodoro.ends = Date.now() + pomodoro.left;
    pomodoro.left = 0;
  } else {
    pomodoro.left = Math.max(1000, pomodoro.ends - Date.now());
  }
  kept.write('framePomodoro', pomodoro);
  drawPomodoro();
};
$('pomo-skip').onclick = () => { if (pomodoro.phase !== 'idle') nextPhase(false); };
$('pomo-reset').onclick = () => {
  pomodoro.phase = 'idle';
  pomodoro.round = 1;
  pomodoro.left = 0;
  kept.write('framePomodoro', pomodoro);
  drawPomodoro();
};

$('ring-stop').onclick = () => stopRinging(false);
$('ring-snooze').onclick = () => stopRinging(true);
$('tools').addEventListener('click', (event) => {
  if (event.target.dataset.tool) choose('tools', 'tool', event.target.dataset.tool);
});

choose('tools', 'tool', params.get('tool') || 'timers');
drawTimers();
drawAlarms();
drawPomodoro();
setInterval(everySecond, 500);
