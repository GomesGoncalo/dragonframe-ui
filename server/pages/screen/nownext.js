// Now and Next, a part of the Utilities tab of the frame's screen: what is happening now and
// what comes after it, as pictures, for a child who cannot read a clock or a calendar yet.
//
// The events are the calendar's own. Those a picture rule is about (words in the event's
// name, kept on the phone page) have a picture, and only those are shown here, so the
// grown-ups' events stay out of it. Each is a disc with its picture in the middle, the same
// disc as the timers': what is on now empties as it nears its end, and what comes next
// empties over its last hour, so "when the green is gone, it is time". With nothing next
// today, the first of tomorrow's is shown instead. Words underneath are for grown-ups.
//
// Glanced at rather than watched: under the clock on the pictures, a line shows the same two
// pictures, and a touch on it opens this.
//
// Loaded after screen.js, whose helpers and `events` it uses.

// What comes next empties over this long before it starts.
const NEXT_SHRINKS_MS = 60 * 60 * 1000;
const nowNextInView = () => tab === 'utilities' && document.body.dataset.tool === 'nownext';

/// The events with a picture that matter now: the one on now (the latest to have begun) and
/// when it ends, the next today, the first tomorrow when nothing is left today, and today's
/// whole-day ones. An event with no length of its own (as those added on the phone page are)
/// lasts until the next with a picture begins, or the day is over.
function nowAndNext(now) {
  const tomorrow = dayAfter(startOfDay(now), 1), after = dayAfter(tomorrow, 1);
  const pictured = events.filter((e) => e.picture).sort((x, y) => x.start - y.start);
  const timed = pictured.filter((e) => !e.allDay);
  const ends = (e) => {
    if (e.end > e.start) return e.end;
    const then = timed.filter((other) => other.start > e.start)[0];
    const midnight = dayAfter(startOfDay(e.start), 1);
    return then && then.start < midnight ? then.start : midnight;
  };
  const on = timed.filter((e) => e.start <= now && ends(e) > now);
  const next = timed.filter((e) => e.start > now && e.start < tomorrow)[0] || null;
  const current = on.length ? on[on.length - 1] : null;
  return {
    now: current,
    until: current ? ends(current) : null,
    next: next,
    tomorrow: next ? null : pictured.filter((e) => e.start >= tomorrow && e.start < after)[0] || null,
    today: pictured.filter((e) => e.allDay && e.start <= now && e.end > now),
  };
}

/// How a disc's share goes from 1 to 0 as the time does: by the length of `whole` milliseconds.
const shareLeft = (left, whole) => (whole > 0 ? Math.max(0, Math.min(1, left / whole)) : 0);
const discColour = (share) => (share > 0.25 ? 'plenty' : share > 0.1 ? 'nearly' : 'last');

/// What one panel shows: a heading, the disc, its name and a line for grown-ups.
function panelsOf(found, now) {
  const panels = [];
  if (found.now) {
    const e = found.now, until = found.until, share = shareLeft(until - now, until - e.start);
    const more = until.getHours() || until.getMinutes() ? 'until ' + clockText(until) : 'for the rest of today';
    panels.push({ kind: 'now', heading: 'Now', event: e, share: share, more: more });
  }
  if (found.next) {
    const e = found.next, left = e.start - now, share = shareLeft(left, NEXT_SHRINKS_MS);
    const minutes = Math.ceil(left / 60000);
    const away = minutes < 60 ? 'in ' + minutes + ' min' : 'in ' + listened(minutes);
    panels.push({ kind: 'next', heading: 'Next', event: e, share: share, more: 'at ' + clockText(e.start) + '  ·  ' + away });
  } else if (found.tomorrow) {
    const e = found.tomorrow;
    panels.push({ kind: 'tomorrow', heading: 'Tomorrow', event: e, share: 1, more: e.allDay ? 'all day' : 'at ' + clockText(e.start) });
  }
  return panels;
}

/// "1 h 05": a length of time in minutes, for grown-ups.
function listened(minutes) {
  return Math.floor(minutes / 60) + ' h' + (minutes % 60 ? ' ' + ('0' + minutes % 60).slice(-2) : '');
}

function drawNowNext() {
  const now = new Date(), found = nowAndNext(now), panels = panelsOf(found, now);
  $('nownext-today').textContent = found.today.length
    ? 'Today  ' + found.today.map((e) => e.picture).join(' ') : '';
  $('nownext-note').textContent = panels.length ? ''
    : 'Nothing with a picture is on the calendar today or tomorrow.';
  const box = $('nownext-panels');
  box.className = 'panels count-' + panels.length;
  // The panels are made afresh only when what they are about changes; the discs move on in place.
  const key = JSON.stringify(panels.map((p) => [p.kind, p.event.title, p.event.start.getTime(), p.event.picture, p.event.called]));
  if (key !== box.dataset.drawn) {
    box.dataset.drawn = key;
    box.textContent = '';
    panels.forEach((p, i) => {
      if (i) box.appendChild(element('div', 'then', '→'));
      const panel = box.appendChild(element('div', 'panel ' + p.kind, ''));
      panel.appendChild(element('div', 'heading', p.heading));
      panel.appendChild(element('div', 'face', ''));
      panel.appendChild(element('div', 'called', p.event.called));
      panel.appendChild(element('div', 'more', ''));
    });
  }
  panels.forEach((p, i) => {
    const panel = box.querySelectorAll('.panel')[i];
    const colour = p.kind === 'tomorrow' ? 'resting' : discColour(p.share);
    drawDisc(panel.querySelector('.face'), { share: p.share, colour: colour, picture: p.event.picture });
    if (panel.lastChild.textContent !== p.more) panel.lastChild.textContent = p.more;
  });
}

/// The line under the clock on the pictures: now's picture and next's, with next's time.
function drawNowNextLine() {
  const line = $('nownext-line');
  const found = pictureRules ? nowAndNext(new Date()) : { now: null, next: null };
  const text = (found.now ? 'Now ' + found.now.picture : '')
    + (found.now && found.next ? '   →   ' : '')
    + (found.next ? 'Next ' + found.next.picture + '  ' + clockText(found.next.start) : '');
  if (line.textContent !== text) line.textContent = text;
}

function showNowNext() {
  if (tab !== 'utilities') showTab('utilities');
  choose('tools', 'tool', 'nownext');
  drawNowNext();
}

/// Whether there is a Now and Next to go to: only once some events have a picture.
function nowNextNews() {
  document.body.classList.toggle('no-nownext', !pictureRules);
  if (!pictureRules && document.body.dataset.tool === 'nownext') choose('tools', 'tool', 'timers');
  drawNowNextLine();
  if (nowNextInView()) drawNowNext();
}

$('nownext-line').addEventListener('click', (touch) => {
  touch.stopPropagation();
  showNowNext();
  touched();
});
$('tools').addEventListener('click', () => {
  if (nowNextInView()) drawNowNext();
});
setInterval(() => {
  nowNextNews();
}, 5000);
