// The Home tab of the frame's screen: the house's alarm, door and motion sensors, and
// cameras, and the notice that someone is at the door.
//
// None of it comes from those devices directly. A relay on another machine tells the
// frame's server what the house is doing, and this asks the server every couple of
// seconds. The tab only exists once the house is connected (see `body.no-home`).
//
// Loaded after screen.js, whose helpers it uses.

const HOUSE_EVERY_MS = 2000;
// A ring older than this when it is first seen (the page was not running) is not announced.
const RING_STALE_SECONDS = 60;
// The notice that someone is at the door goes away by itself after this long. The doorbell's
// picture of the visitor can take a minute to arrive, so the notice outlasts that.
const BELL_FOR_MS = 100 * 1000;
// A picture this much older than the ring (in seconds) was taken before it, of someone else.
const BEFORE_THE_RING = 10;
const ALARM_STATES = {
  disarmed: 'Disarmed', armed_home: 'Armed, at home', armed_away: 'Armed, away', armed_night: 'Armed for the night',
  armed_vacation: 'Armed, on holiday', armed_custom_bypass: 'Armed', arming: 'Arming…', pending: 'About to sound…',
  triggered: 'Alarm sounding', unknown: 'Not known',
};

let house = null;       // what the server last said: { connected, alarms, sensors, cameras, ringing, rang }
let lastRing = null;    // the ring already seen, so each is announced once
let houseDrawn = '';
let looking = null;     // the camera whose picture is shown large
let bellTimer = 0;

const pictureOf = (camera) => '/api/home/picture?camera=' + camera.id + '&at=' + camera.picture;
const timeOf = (seconds) => clockText(new Date(seconds * 1000));

async function refreshHouse() {
  if (document.body.classList.contains('no-home')) return;
  let said;
  try {
    said = await (await fetch('/api/home', { cache: 'no-store' })).json();
  } catch (error) {
    return;
  }
  const first = house === null;
  house = said;
  if (first) {
    lastRing = said.rang;  // whatever rang before this page was up is old news
  } else if (said.rang && said.rang !== lastRing) {
    lastRing = said.rang;
    if (Date.now() / 1000 - said.rang < RING_STALE_SECONDS) showBell();
  }
  drawHouse();
}

function drawHouse() {
  const key = JSON.stringify(house);
  if (key === houseDrawn) return;
  houseDrawn = key;
  $('house-stale').textContent = house.connected ? '' : 'The house is out of touch. This is how things were when it was last heard from.';

  const alarms = $('alarms');
  alarms.textContent = '';
  house.alarms.forEach((alarm) => {
    const kind = alarm.state === 'triggered' || alarm.state === 'pending' ? 'triggered' : alarm.state.indexOf('arm') === 0 ? 'armed' : '';
    const box = element('div', 'alarm ' + kind, '');
    box.appendChild(element('div', 'name', alarm.name));
    box.appendChild(element('div', 'state', ALARM_STATES[alarm.state] || ALARM_STATES.unknown));
    alarms.appendChild(box);
  });

  // Whatever is open or moving comes first.
  const sensors = $('sensors');
  sensors.textContent = '';
  house.sensors.slice().sort((x, y) => (y.on - x.on) || (x.name < y.name ? -1 : 1)).forEach((sensor) => {
    const row = element('div', 'sensor' + (sensor.on ? ' on' : ''), sensor.name);
    const is = sensor.kind === 'motion' ? (sensor.on ? 'Movement' : 'Still') : (sensor.on ? 'Open' : 'Closed');
    row.appendChild(element('span', 'is', is));
    sensors.appendChild(row);
  });

  const cameras = $('cameras');
  cameras.textContent = '';
  house.cameras.forEach((camera) => {
    const tile = element('div', 'camera' + (camera.person ? ' person' : ''), '');
    if (camera.picture) tile.appendChild(element('img', '', '')).src = pictureOf(camera);
    else tile.appendChild(element('div', 'none', 'No picture yet'));
    const name = element('div', 'name', camera.name);
    name.appendChild(element('span', '', camera.person ? 'Someone there' : camera.picture ? timeOf(camera.picture) : ''));
    tile.appendChild(name);
    tile.onclick = () => { if (camera.picture) look(camera.id); };
    cameras.appendChild(tile);
  });

  // The pictures shown large follow their cameras.
  if (looking) look(looking);
  if ($('bell').classList.contains('on')) drawBell();
}

/// Show a camera's picture large.
function look(id) {
  const camera = house.cameras.filter((c) => c.id === id)[0];
  if (!camera || !camera.picture) return closeLook();
  looking = id;
  $('look-said').textContent = camera.name + '  ·  ' + timeOf(camera.picture);
  const src = pictureOf(camera);
  if ($('look-picture').getAttribute('src') !== src) $('look-picture').src = src;
  $('look').classList.add('on');
}
function closeLook() {
  looking = null;
  $('look').classList.remove('on');
}

// --- someone is at the door --------------------------------------------------------------

/// The doorbell's own sound: two notes, higher then lower, twice.
function playBell() {
  try {
    audio = audio || new (window.AudioContext || window.webkitAudioContext)();
    if (audio.state === 'suspended') audio.resume();
    [[783.99, 0], [622.25, 0.5], [783.99, 1.5], [622.25, 2]].forEach((note) => {
      const tone = audio.createOscillator(), loudness = audio.createGain();
      const start = audio.currentTime + note[1];
      tone.type = 'triangle';
      tone.frequency.value = note[0];
      loudness.gain.setValueAtTime(0.0001, start);
      loudness.gain.exponentialRampToValueAtTime(0.4, start + 0.02);
      loudness.gain.exponentialRampToValueAtTime(0.0001, start + 1.1);
      tone.connect(loudness);
      loudness.connect(audio.destination);
      tone.start(start);
      tone.stop(start + 1.2);
    });
  } catch (error) {
    // No sound on this browser; the notice still shows.
  }
}

function drawBell() {
  // The doorbell's own camera, if the house says which that is.
  const camera = house.cameras.filter((c) => c.bell)[0];
  const picture = $('bell-picture');
  picture.style.display = camera && camera.picture ? '' : 'none';
  if (camera && camera.picture && picture.getAttribute('src') !== pictureOf(camera)) picture.src = pictureOf(camera);
  // Until the picture of this ring arrives, say that what is shown is from before it.
  const earlier = camera && camera.picture && camera.picture < house.rang - BEFORE_THE_RING;
  $('bell-note').textContent = earlier ? 'Earlier picture, from ' + timeOf(camera.picture) + '. The new one is on its way.'
    : !camera || !camera.picture ? 'The picture is on its way.' : '';
}

function showBell() {
  $('bell-when').textContent = timeOf(house.rang);
  drawBell();
  $('bell').classList.add('on');
  playBell();
  clearTimeout(bellTimer);
  bellTimer = setTimeout(closeBell, BELL_FOR_MS);
}
function closeBell() {
  clearTimeout(bellTimer);
  $('bell').classList.remove('on');
}

$('look-close').onclick = closeLook;
$('bell-close').onclick = closeBell;
setInterval(refreshHouse, HOUSE_EVERY_MS);
refreshHouse();
