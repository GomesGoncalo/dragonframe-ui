// The frame's calendar, and the events that are about to start. Part of the screen; see index.html.
'use strict';

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
  // Now and Next, in Utilities, is there only once some events have a picture.
  pictureRules = (found.event_pictures || []).length;
  return (found.events || []).map((e) => ({
    title: e.title, place: e.place, allDay: e.all_day, start: when(e.start), end: when(e.end),
    // Its picture, if a picture rule is about it, and what to call it beside that (see nownext.js).
    picture: e.picture ? e.picture.picture : '', called: e.picture && e.picture.say ? e.picture.say : e.title,
    colour: typeof e.feed === 'number' ? COLOURS[e.feed % COLOURS.length] : ADDED_COLOUR,
    // Which calendar it is on, to say so beside it.
    from: e.added ? 'Added here' : e.calendar || '',
    // The reminder rules that are about it (see reminders.js).
    rules: (e.remind || []).map((rule) => (found.reminders || [])[rule]).filter(Boolean),
    // (A rule may also say what the frame is to do when its note comes up: `then`.)
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
    nowNextNews();
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
  node.appendChild(element('div', 'what', (event.picture ? event.picture + '  ' : '') + event.title));
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
let chimeLoudness = 0.3;
/// Two soft notes, the second lower. Made here rather than played from a file. They are as
/// loud as the chime is set to be, whatever the music's volume, and the music makes way.
function playChime() {
  quietMusic(CHIME_MS);
  try {
    audio = audio || new (window.AudioContext || window.webkitAudioContext)();
    if (audio.state === 'suspended') audio.resume();
    [[659.25, 0], [523.25, 0.45]].forEach((note) => {
      const tone = audio.createOscillator(), loudness = audio.createGain();
      const start = audio.currentTime + note[1];
      tone.type = 'sine';
      tone.frequency.value = note[0];
      loudness.gain.setValueAtTime(0.0001, start);
      loudness.gain.exponentialRampToValueAtTime(chimeLoudness, start + 0.03);
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
