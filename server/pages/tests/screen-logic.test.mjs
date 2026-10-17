// The screen's own reckoning, tested by calling the page's functions inside the page: how
// photos share the screen, how the calendar sorts days, when alarms ring and notes come up.
// Times are London's, as the frame's are.

import assert from 'node:assert/strict';
import { after, before, describe, test } from 'node:test';

import { launchChrome, startServer } from './harness.mjs';

let server, chrome, page;

before(async () => {
  server = await startServer();
  chrome = await launchChrome();
  page = await chrome.open(server.base + '/frame');
});

after(() => {
  if (page) page.close();
  if (chrome) chrome.stop();
  if (server) server.stop();
});

// --- photos sharing the screen ----------------------------------------------------------

/// What `arrange` makes of photos of these shapes (width / height) on a screen of `screen`,
/// with each cell's own shape worked out from where it was put.
async function arranged(shapes, screen) {
  return page.eval(`(() => {
    const photos = ${JSON.stringify(shapes)}.map((s, i) => ['p' + i + '.jpg', 0, s * 1000, 1000, false]);
    const box = arrange(photos[0], photos.slice(1), ${screen});
    return {
      total: box.total,
      cells: box.cells.map((c) => ({ name: c.photo[0], shape: shape(c.photo), x: c.x, y: c.y, w: c.w, h: c.h })),
    };
  })()`);
}

const near = (a, b) => Math.abs(a - b) < 1e-9;

/// Everything that must hold of any arrangement: every photo whole and in its own shape,
/// nothing outside the box, nothing on top of anything else.
function assertWhole(box) {
  for (const cell of box.cells) {
    // A cell is a share of the box's width and height; the box is `total` wide for 1 high.
    assert.ok(near((cell.w * box.total) / cell.h, cell.shape), `${cell.name} keeps its shape (never cropped)`);
    assert.ok(cell.x >= -1e-9 && cell.y >= -1e-9 && cell.x + cell.w <= 1 + 1e-9 && cell.y + cell.h <= 1 + 1e-9, `${cell.name} is inside`);
  }
  for (const a of box.cells) {
    for (const b of box.cells) {
      if (a === b) continue;
      const apart = a.x + a.w <= b.x + 1e-9 || b.x + b.w <= a.x + 1e-9 || a.y + a.h <= b.y + 1e-9 || b.y + b.h <= a.y + 1e-9;
      assert.ok(apart, `${a.name} and ${b.name} do not overlap`);
    }
  }
  const area = box.cells.reduce((sum, c) => sum + c.w * c.h, 0);
  assert.ok(near(area, 1), 'together they fill the box');
}

describe('photos sharing the screen', () => {
  const WIDE = 1706.67 / 960;

  test('a photo the shape of the screen is shown alone', async () => {
    const box = await arranged([WIDE, 3 / 4, 3 / 4, 3 / 4], WIDE);
    assert.deepEqual(box.cells.map((c) => c.name), ['p0.jpg']);
    assertWhole(box);
  });

  test('portraits share a wide screen, whole, and fill it better than one alone', async () => {
    const box = await arranged([3 / 4, 3 / 4, 3 / 4, 3 / 4, 3 / 4, 3 / 4], WIDE);
    assert.ok(box.cells.length > 1 && box.cells.length <= 4, `${box.cells.length} photos`);
    assert.ok(box.cells.some((c) => c.name === 'p0.jpg'), 'the photo at the head of the line is one of them');
    assert.ok(Math.abs(Math.log(box.total / WIDE)) < Math.abs(Math.log(3 / 4 / WIDE)), 'less of the screen is empty');
    assertWhole(box);
  });

  test('on a frame standing tall, landscapes go one above the other', async () => {
    const box = await arranged([4 / 3, 4 / 3, 4 / 3], 1 / WIDE);
    assert.ok(box.cells.length > 1);
    assert.ok(box.cells.every((c) => near(c.x, 0) && near(c.w, 1)), 'one column');
    assertWhole(box);
  });

  test('any mixture of shapes comes out whole', async () => {
    const mixtures = [
      [3 / 4, 16 / 9, 1, 2 / 3, 3 / 2],
      [9 / 16, 9 / 16, 4 / 3, 1, 9 / 16, 2],
      [2.5, 0.5, 0.5, 0.5],
      [1],
    ];
    for (const shapes of mixtures) {
      for (const screen of [WIDE, 1 / WIDE]) {
        const box = await arranged(shapes, screen);
        assert.ok(box.cells.length >= 1 && box.cells.length <= 4);
        assertWhole(box);
      }
    }
  });

  test('every way of grouping photos into columns is tried, each once', async () => {
    // Which column comes first leaves as much empty either way (and is shuffled after), so
    // the ways are the groupings: 1, 2, 5 and 15 of them for 1 to 4 photos.
    const ways = await page.eval(`[1, 2, 3, 4].map((n) => intoColumns(Array.from({ length: n }, (_, i) => i))
      .map((columns) => columns.map((column) => column.slice().sort().join('')).sort().join('|')))`);
    assert.deepEqual(ways.map((list) => list.length), [1, 2, 5, 15]);
    ways.forEach((list) => assert.equal(new Set(list).size, list.length, 'no grouping twice'));
  });
});

// --- the calendar -------------------------------------------------------------------------

describe('the order photos come in', () => {
  /// How many times each photo comes round in one round of `order`, for photos taken this
  /// many days ago (with `fav` making one a favourite).
  async function turns(order, photos) {
    return page.eval(`(() => {
      const now = Date.UTC(2026, 9, 17);
      const photos = ${JSON.stringify(photos)}.map((p) => [p.name, (now - p.days * DAY_MS) / 1000, 1, 1, !!p.fav]);
      const counts = {};
      round(photos, '${order}', now).forEach((p) => { counts[p[0]] = (counts[p[0]] || 0) + 1; });
      return counts;
    })()`);
  }
  const photos = [
    { name: 'week', days: 7 }, { name: 'season', days: 100 }, { name: 'old', days: 900 },
    { name: 'old-fav', days: 900, fav: true }, { name: 'week-fav', days: 7, fav: true },
  ];

  test('shuffled, each photo comes once and a favourite twice', async () => {
    assert.deepEqual(await turns('shuffled', photos), { week: 1, season: 1, old: 1, 'old-fav': 2, 'week-fav': 2 });
  });

  test('recent more often: three times for the last month, twice for the last year, a favourite once more', async () => {
    assert.deepEqual(await turns('recent', photos), { week: 3, season: 2, old: 1, 'old-fav': 2, 'week-fav': 4 });
  });

  test('newest first, each photo once, newest at the front', async () => {
    const order = await page.eval(`round([['a', 1], ['b', 3], ['c', 2]], 'newest', 0).map((p) => p[0])`);
    assert.deepEqual(order, ['b', 'c', 'a']);
  });
});

describe('the calendar', () => {
  test('times from the server: a number is a moment, text is a day or a time here', async () => {
    const read = await page.eval(`[
      when(1797148800).toISOString(),
      when('2026-10-14').toString(),
      when('2026-10-14T09:30:00').toString(),
    ]`);
    assert.equal(read[0], '2026-12-13T08:00:00.000Z');
    assert.match(read[1], /^Wed Oct 14 2026 00:00:00 GMT\+0100/);
    assert.match(read[2], /^Wed Oct 14 2026 09:30:00 GMT\+0100/);
  });

  test('weeks start on Monday, and the calendar is fetched from the 1st or that week\'s Monday, whichever is first', async () => {
    const days = await page.eval(`[
      weekStart(new Date(2026, 9, 14, 15)).toDateString(),
      weekStart(new Date(2026, 9, 18, 23)).toDateString(),
      weekStart(new Date(2026, 9, 19)).toDateString(),
      viewStart(new Date(2026, 9, 14)).toDateString(),
      viewStart(new Date(2026, 5, 1)).toDateString(),
    ]`);
    // The month grid leaves the days before the 1st blank, so October starts on the 1st.
    assert.deepEqual(days, ['Mon Oct 12 2026', 'Mon Oct 12 2026', 'Mon Oct 19 2026', 'Thu Oct 01 2026', 'Mon Jun 01 2026']);
  });

  test('a whole-day event is on every day it covers, and a timed one on its own day', async () => {
    const days = await page.eval(`(() => {
      const trip = { title: 'Trip', allDay: true, start: new Date(2026, 9, 24), end: new Date(2026, 9, 27) };
      // Late on the Saturday the clocks go back, and on past midnight.
      const party = { title: 'Party', allDay: false, start: new Date(2026, 9, 24, 22), end: new Date(2026, 9, 25, 2) };
      const byDay = eventsByDay([trip, party]);
      return Object.keys(byDay).sort().map((day) => day + ' ' + byDay[day].map((e) => e.title).join('+'));
    })()`);
    assert.deepEqual(days, ['2026-10-24 Trip+Party', '2026-10-25 Trip', '2026-10-26 Trip']);
  });

  test('whole-day events come first, then the rest by time', async () => {
    const order = await page.eval(`inOrder([
      { title: 'late', allDay: false, start: new Date(2026, 9, 14, 18) },
      { title: 'all day', allDay: true, start: new Date(2026, 9, 14) },
      { title: 'early', allDay: false, start: new Date(2026, 9, 14, 8) },
    ]).map((e) => e.title)`);
    assert.deepEqual(order, ['all day', 'early', 'late']);
    assert.deepEqual(await page.eval('inOrder(undefined)'), []);
  });

  test('events are listed under Today, Tomorrow, or their date', async () => {
    const said = await page.eval(`(() => {
      const now = new Date(2026, 9, 14, 12);
      const at = (d, h) => ({ start: new Date(2026, 9, d, h), allDay: false });
      return [dayText(at(14, 18), now), dayText(at(15, 9), now), dayText(at(13, 9), now), dayText(at(17, 9), now),
        timeText(at(14, 18), now), timeText({ start: new Date(2026, 9, 14), allDay: true }, now),
        timeText(at(13, 20), new Date(2026, 9, 14))];
    })()`);
    // Something that began yesterday and is still on is today's, and lasts all of it.
    assert.deepEqual(said, ['Today', 'Tomorrow', 'Today', 'Saturday 17 October', '18:00', 'All day', 'All day']);
  });
});

// --- alarms and timers ---------------------------------------------------------------------

describe('alarms and timers', () => {
  const next = (alarm, now) => page.eval(`(() => {
    const at = nextRing(${JSON.stringify(alarm)}, new Date(${now}));
    return at && at.toString().slice(0, 21);
  })()`);
  // Wednesday 14 October 2026 at 08:00, London.
  const WEDNESDAY = 'new Date(2026, 9, 14, 8)';
  const FRIDAY_EVENING = 'new Date(2026, 9, 16, 20)';

  test('an alarm rings next on a day it is for', async () => {
    const alarm = { on: true, time: '07:30', repeat: 'daily' };
    assert.equal(await next(alarm, WEDNESDAY), 'Thu Oct 15 2026 07:30');
    assert.equal(await next({ ...alarm, time: '09:00' }, WEDNESDAY), 'Wed Oct 14 2026 09:00');
    assert.equal(await next({ ...alarm, repeat: 'weekdays' }, FRIDAY_EVENING), 'Mon Oct 19 2026 07:30');
    assert.equal(await next({ ...alarm, repeat: 'weekends' }, WEDNESDAY), 'Sat Oct 17 2026 07:30');
  });

  test('an alarm that is off never rings, and a snoozed one rings when the snooze ends', async () => {
    assert.equal(await next({ on: false, time: '07:30', repeat: 'daily' }, WEDNESDAY), null);
    const snoozed = new Date(2026, 9, 14, 8, 9).getTime();
    assert.equal(await next({ on: true, time: '07:30', repeat: 'daily', snoozed }, WEDNESDAY), 'Wed Oct 14 2026 08:09');
  });

  test('an alarm keeps its time on the clock when the clocks go back', async () => {
    // The clocks go back at 02:00 on Sunday 25 October 2026.
    assert.equal(await next({ on: true, time: '07:00', repeat: 'daily' }, 'new Date(2026, 9, 24, 23)'), 'Sun Oct 25 2026 07:00');
  });

  test('time left reads as minutes and seconds, with hours when there are any', async () => {
    const read = await page.eval('[countdown(299001), countdown(60000), countdown(3725000), countdown(-5), minutesText(60), minutesText(25)]');
    assert.deepEqual(read, ['5:00', '1:00', '1:02:05', '0:00', '1 hour', '25 min']);
  });
});

// --- reminders ------------------------------------------------------------------------------

describe('reminders', () => {
  /// The notes up at `now` for these events (in the page's shape), the page's own events put back after.
  const notesAt = (eventsSource, now) => page.eval(`(() => {
    const kept = events;
    events = ${eventsSource};
    try {
      return remindersAt(${now}).map((r) => r.what + ' | ' + r.when);
    } finally {
      events = kept;
    }
  })()`);
  const DENTIST = `[{
    title: 'Dentist', place: '', allDay: false, trip: null,
    start: new Date(2026, 9, 14, 15), end: new Date(2026, 9, 14, 16),
    rules: [{ words: 'dentist', when: { minutes: 60 }, say: 'Leave for the dentist' }],
  }]`;

  test('a note comes up when its rule says, and stays until the event is over', async () => {
    assert.deepEqual(await notesAt(DENTIST, 'new Date(2026, 9, 14, 13, 59).getTime()'), []);
    assert.deepEqual(await notesAt(DENTIST, 'new Date(2026, 9, 14, 14).getTime()'), ['Leave for the dentist | Dentist  ·  Today  ·  15:00']);
    assert.equal((await notesAt(DENTIST, 'new Date(2026, 9, 14, 15, 59).getTime()')).length, 1);
    assert.deepEqual(await notesAt(DENTIST, 'new Date(2026, 9, 14, 16).getTime()'), []);
  });

  test('a rule can be for a time of day some days before', async () => {
    const bins = `[{
      title: 'Bin day', place: '', allDay: true, trip: null,
      start: new Date(2026, 9, 16), end: new Date(2026, 9, 17),
      rules: [{ words: 'bin', when: { days: 1, at: '19:00' } }],
    }]`;
    assert.deepEqual(await notesAt(bins, 'new Date(2026, 9, 15, 18, 59).getTime()'), []);
    assert.deepEqual(await notesAt(bins, 'new Date(2026, 9, 15, 19).getTime()'), ['Bin day | Tomorrow']);
  });

  test('two rules about one event give one note', async () => {
    const twice = DENTIST.replace("rules: [", "rules: [{ words: 'dent', when: { minutes: 90 } }, ");
    assert.deepEqual(await notesAt(twice, 'new Date(2026, 9, 14, 14, 30).getTime()'), ['Dentist | Today  ·  15:00']);
  });
});
