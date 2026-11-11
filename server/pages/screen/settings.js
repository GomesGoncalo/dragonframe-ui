// The frame's Settings, and the PIN. Part of the screen; see index.html.
'use strict';

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

  choices($('chime-loudness'), CHIME_LOUDNESS, chimeLoudness, (loud) => {
    chimeLoudness = loud;
    remember('frameChimeLoudness', loud);
    drawSettings();
    playChime();
  });
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
    stationsNews(reply.stations || []);
    // A place chosen a moment ago, or a fresh forecast, shows without waiting for the next turn.
    if ((reply.weather || null) !== (weather ? weather.fetched : null)) refreshWeather();
    // Home, in Utilities, is there only while the house is connected.
    document.body.classList.toggle('no-home', !reply.home);
    if (!reply.home && document.body.dataset.tool === 'home') choose('tools', 'tool', 'timers');
    // Yoto likewise, while Lullimit can be read.
    const yotoBefore = !document.body.classList.contains('no-yoto');
    document.body.classList.toggle('no-yoto', !reply.yoto);
    if (!reply.yoto && document.body.dataset.tool === 'yoto') choose('tools', 'tool', 'timers');
    if (reply.yoto && !yotoBefore) refreshYoto();
    asleep = !!reply.asleep;
    document.body.classList.toggle('asleep', asleep);
    schedule();
  } catch (error) {
    // The server will hear from us at the next turn.
  }
  setTimeout(sayAlive, ALIVE_MS);
}
