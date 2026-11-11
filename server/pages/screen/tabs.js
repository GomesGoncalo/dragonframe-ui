// The tabs, and the start of the screen. Part of the screen; see index.html.
'use strict';

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
  standingChanged();
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
  // With the tabs up, the photos are there to be touched (to show one alone, say) and the
  // arrows step; a touch anywhere else puts the tabs away.
  if (document.body.classList.contains('bar')) document.body.classList.remove('bar');
  else if (x < 0.25) step(-1, true);
  else if (x > 0.75) step(1, true);
  else document.body.classList.add('bar');
  touched();
});
// The arrows that are there while the tabs are up.
for (const [id, by] of [['previous', -1], ['next', 1]]) {
  $(id).addEventListener('click', (event) => {
    event.stopPropagation();
    step(by, true);
    touched();
  });
}
// (Keys, from a keyboard or a remote control, are in actions.js.)

// Every tab gets its drawing.
const withIcons = document.querySelectorAll('[data-icon]');
for (let i = 0; i < withIcons.length; i++) label(withIcons[i], withIcons[i].dataset.icon, withIcons[i].textContent);

chimeLoudness = Number(remembered('frameChimeLoudness')) || 0.3;
if (!CHIME_LOUDNESS.some((one) => one[0] === chimeLoudness)) chimeLoudness = 0.3;
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
