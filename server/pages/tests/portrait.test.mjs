// The frame's own screen standing up (portrait), in its own Chrome and server so that
// nothing else running on the screen meddles. Run all page tests with: node --test server/pages/tests/

import assert from 'node:assert/strict';
import { after, before, describe, test } from 'node:test';

import { launchChrome, startServer, until } from './harness.mjs';

let server, chrome, page;

before(async () => {
  server = await startServer();
  chrome = await launchChrome();
  page = await chrome.open(server.base + '/frame?tab=utilities', { width: 960, height: 1707, scale: 1.125 });
});

after(() => {
  if (page) page.close();
  if (chrome) chrome.stop();
  if (server) server.stop();
});

describe('standing up', () => {
  test('every button in a row of choices can be reached, however many there are', async () => {
    await until('the page being ready', () => page.eval('document.readyState === "complete"'));
    // As with the house connected and Yoto set up: the most buttons Utilities has.
    const cut = await page.eval(`(() => {
      document.body.classList.remove('no-home', 'no-yoto');
      return Array.prototype.filter.call(document.querySelectorAll('.switch button'), (button) => {
        const r = button.getBoundingClientRect();
        if (!r.width) return false;
        const at = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        return r.right > innerWidth || !button.contains(at);
      }).map((button) => button.textContent);
    })()`);
    assert.deepEqual(cut, []);
  });
});
