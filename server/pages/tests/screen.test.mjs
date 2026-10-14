// The frame's own screen (pages/screen), driven in Chrome at the frame's size against the
// real server. Run all page tests with: node --test server/pages/tests/

import assert from 'node:assert/strict';
import { after, before, describe, test } from 'node:test';

import { addPhotos, launchChrome, startServer, until } from './harness.mjs';

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

describe('loading', () => {
  test('the screen loads without a single error', async () => {
    await until('the page being ready', () => page.eval('document.readyState === "complete"'));
    assert.deepEqual(page.errors, []);
  });

  test('with no photos it says it is waiting for them', async () => {
    await until('the notice', () => page.eval('$("notice").textContent.includes("No photos")'));
  });
});

/// The cells drawn on the screen now: each one's place, and the shape of the photo in it.
const drawnCells = () => page.eval(`(() => {
  const shapes = {};
  photos.forEach((p) => { shapes[url('file', p[0])] = shape(p); });
  return Array.prototype.map.call(document.querySelectorAll('.slide.on .cell'), (cell) => {
    const r = cell.getBoundingClientRect();
    const photo = cell.style.backgroundImage.slice(5, -2);
    return { photo, shape: shapes[photo], x: r.left, y: r.top, w: r.width, h: r.height };
  });
})()`);

describe('pictures', () => {
  test('photos come up, each one whole, in its own shape, inside the screen', async () => {
    await addPhotos(page, server, [[1600, 900], [900, 1200], [900, 1200], [1200, 1200], [800, 1200], [1200, 800]]);
    await page.eval('refreshPhotos()');
    const size = await page.eval('[innerWidth, innerHeight]');
    // Several screens' worth, so photos sharing the screen are seen too.
    for (let screens = 0; screens < 5; screens++) {
      const cells = await until('photos on the screen', async () => {
        const found = await drawnCells();
        return found.length && found.every((c) => c.shape) && found;
      });
      for (const cell of cells) {
        assert.ok(Math.abs(cell.w / cell.h - cell.shape) < 0.01, `${cell.photo} is ${cell.w / cell.h}, not ${cell.shape}: cropped or stretched`);
        assert.ok(cell.x >= -0.5 && cell.y >= -0.5 && cell.x + cell.w <= size[0] + 0.5 && cell.y + cell.h <= size[1] + 0.5, `${cell.photo} is on the screen`);
      }
      const before = JSON.stringify(cells);
      await page.eval('step(1)');
      await until('the next screen', async () => JSON.stringify(await drawnCells()) !== before);
    }
    assert.deepEqual(page.errors, []);
  });
});

describe('stepping', () => {
  test('the arrows step while the tabs are up, and a touch at the side does not', async () => {
    const seen = () => page.eval('JSON.stringify(shown[at].cells.map((c) => c.photo[0]))');
    await page.eval('document.body.classList.add("bar")');
    const before = await seen();
    await page.eval('$("next").click()');
    await until('the next screen', async () => (await seen()) !== before);
    const middle = await seen();
    // A touch at the side with the tabs up puts them away instead of stepping.
    await page.eval('document.dispatchEvent(new MouseEvent("click", { clientX: 5, clientY: 5 }))');
    assert.equal(await page.eval('document.body.classList.contains("bar")'), false);
    assert.equal(await seen(), middle);
    // And the screen after the next is ready before it is asked for.
    await until('the next screen being ready', () => page.eval('!!prepared'));
    await page.eval('$("previous").click()');
    await until('the screen before', async () => (await seen()) !== middle);
    assert.deepEqual(page.errors, []);
  });
});

describe('told from elsewhere', () => {
  test('the remote control on the phone page changes the tab within moments', async () => {
    await server.api('/api/act', { do: 'show', tab: 'calendar' });
    await until('the calendar tab', () => page.eval('tab === "calendar" && document.body.dataset.tab === "calendar"'));
    // And the server hears where the screen now stands.
    await until('the server hearing of it', async () => (await server.api('/api/acting')).said.tab === 'calendar');
    await server.api('/api/act', { do: 'show', tab: 'pictures' });
    await until('the pictures tab', () => page.eval('tab === "pictures"'));
  });

  test('a note sent to the screen is shown', async () => {
    await server.api('/api/act', { do: 'note', say: 'Dinner is ready' });
    await until('the note', () => page.eval('$("soon").classList.contains("on") && $("soon-what").textContent === "Dinner is ready"'));
  });

  test('an event added on the phone page shows in Coming up', async () => {
    const tomorrow = await page.eval('dayKey(dayAfter(new Date(), 1)).split("-").map((n) => n.padStart(2, "0")).join("-")');
    await server.api('/api/events/add', { title: 'Parents evening', day: tomorrow, time: '09:00' });
    await page.eval('refreshCalendar()');
    await page.eval('showTab("calendar")');
    await until('the event in the list', () => page.eval('$("agenda").textContent.includes("Parents evening")'));
    assert.match(await page.eval('$("agenda").textContent'), /Tomorrow.*Parents evening.*09:00.*Added here/);
  });
});

describe('settings', () => {
  test('with a PIN, Settings is hidden until the PIN is typed on the keypad', async () => {
    await server.api('/api/pin', { pin: '2468' });
    await page.eval('sayAlive()');
    const press = (keys) => page.eval(`${JSON.stringify(keys)}.forEach((key) => {
      Array.prototype.find.call($('keys').children, (b) => b.textContent === key).click();
    })`);
    await page.eval('showTab("settings")');
    await until('the keypad', () => page.eval('document.body.classList.contains("locked")'));
    await press(['1', '1', '1', '1', 'OK']);
    await until('being refused', () => page.eval('$("pin-note").textContent.startsWith("Wrong PIN")'));
    assert.ok(await page.eval('document.body.classList.contains("locked")'));
    await press(['2', '4', '6', '8', 'OK']);
    await until('Settings opening', () => page.eval('!document.body.classList.contains("locked")'));
    // Leaving Settings gives the PIN up again.
    await page.eval('showTab("pictures")');
    await page.eval('showTab("settings")');
    await until('the keypad again', () => page.eval('document.body.classList.contains("locked")'));
    await page.eval('showTab("pictures")');
  });
});
