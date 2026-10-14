// What the page tests stand on: the real server, run with a stand-in frame against folders of
// its own, and a headless Chrome driven over the DevTools protocol. No packages: Node's own
// test runner, fetch and WebSocket (Node 22, or Node 20 with --experimental-websocket).
//
// The pages are classic scripts sharing one global scope, so a test can call the page's own
// functions inside it (`page.eval('arrange(...)')`) as well as touch it like a person would.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const SERVER_DIR = fileURLToPath(new URL('../..', import.meta.url));
// The frame's screen: 1920x1080 pixels at a density that makes it 1706.67x960 in CSS pixels.
export const FRAME = { width: 1707, height: 960, scale: 1.125 };
const ZONE = 'Europe/London';

if (typeof WebSocket === 'undefined') {
  throw new Error('This Node has no WebSocket: use Node 22, or Node 20 with --experimental-websocket');
}

/// A Chrome or Chromium to drive: $CHROME, or one in the usual places.
export function findChrome() {
  const home = process.env.HOME || '';
  const playwright = join(home, 'Library', 'Caches', 'ms-playwright');
  const candidates = [
    process.env.CHROME,
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ];
  if (existsSync(playwright)) {
    for (const name of readdirSync(playwright).filter((n) => n.startsWith('chromium-')).sort().reverse()) {
      candidates.push(join(playwright, name, 'chrome-mac', 'Chromium.app', 'Contents', 'MacOS', 'Chromium'));
      candidates.push(join(playwright, name, 'chrome-linux', 'chrome'));
    }
  }
  return candidates.find((path) => path && existsSync(path)) || null;
}

const freePort = () => new Promise((resolve, reject) => {
  const probe = createServer();
  probe.once('error', reject);
  probe.listen(0, '127.0.0.1', () => {
    const { port } = probe.address();
    probe.close(() => resolve(port));
  });
});

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/// Wait until `check` gives something truthy, trying every 50 ms for up to `ms`.
export async function until(what, check, ms = 5000) {
  const end = Date.now() + ms;
  for (;;) {
    let last;
    try {
      last = await check();
      if (last) return last;
    } catch (error) {
      last = error;
    }
    if (Date.now() > end) throw new Error(`${what} never happened (last: ${last instanceof Error ? last.message : JSON.stringify(last)})`);
    await pause(50);
  }
}

/// The server, built if it needs to be, running with a stand-in frame against empty folders.
export async function startServer() {
  const build = spawnSync('cargo', ['build', '--quiet'], { cwd: SERVER_DIR, stdio: 'inherit' });
  if (build.status !== 0) throw new Error('the server did not build');
  const root = mkdtempSync(join(tmpdir(), 'frame-pages-'));
  const photos = join(root, 'photos');
  mkdirSync(join(photos, 'frame-sync'), { recursive: true });
  const port = await freePort();
  const child = spawn(join(SERVER_DIR, 'target', 'debug', 'frameserver'), [], {
    env: {
      ...process.env,
      FRAME_CLOUD_DIR: photos,
      FRAME_DATA_DIR: join(root, 'data'),
      FRAME_PORT: String(port),
      FRAME_BIND: '127.0.0.1',
      FRAME_DEVICE: 'simulated',
      FRAME_TZ: ZONE,
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let said = '';
  child.stderr.on('data', (chunk) => { said += chunk; });
  const base = `http://127.0.0.1:${port}`;
  await until('the server answering', async () => (await fetch(base + '/api/device')).ok, 15000).catch((error) => {
    child.kill();
    throw new Error(`${error.message}\n${said}`);
  });
  return {
    base,
    photos,
    /// Ask the server something as the page does.
    async api(path, body) {
      const init = body === undefined ? {} : {
        method: 'POST',
        headers: { 'X-Frame-App': '1', 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      };
      const reply = await fetch(base + path, init);
      const text = await reply.text();
      if (!reply.ok) throw new Error(`${path}: ${reply.status} ${text}`);
      return text ? JSON.parse(text) : null;
    },
    stop() {
      child.kill();
      rmSync(root, { recursive: true, force: true });
    },
  };
}

/// A headless Chrome, and pages opened in it.
export async function launchChrome() {
  const path = findChrome();
  if (!path) throw new Error('no Chrome found; set CHROME to one');
  const profile = mkdtempSync(join(tmpdir(), 'frame-chrome-'));
  const child = spawn(path, [
    '--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`,
    '--no-first-run', '--no-default-browser-check', '--mute-audio', '--disable-gpu',
    '--autoplay-policy=no-user-gesture-required', 'about:blank',
  ], { stdio: 'ignore', env: { ...process.env, TZ: ZONE } });
  const portFile = join(profile, 'DevToolsActivePort');
  const port = await until('Chrome starting', () => existsSync(portFile) && readFileSync(portFile, 'utf8').split('\n')[0], 15000);
  const devtools = `http://127.0.0.1:${port}`;
  return {
    /// A new tab the size of the frame's screen (or `size`), showing `url`.
    async open(url, size = FRAME) {
      const target = await (await fetch(`${devtools}/json/new?about:blank`, { method: 'PUT' })).json();
      const page = await Page.connect(target.webSocketDebuggerUrl);
      page.closer = () => fetch(`${devtools}/json/close/${target.id}`);
      await page.send('Emulation.setDeviceMetricsOverride', {
        width: size.width, height: size.height, deviceScaleFactor: size.scale, mobile: false,
      });
      await page.send('Emulation.setTimezoneOverride', { timezoneId: ZONE });
      await page.goto(url);
      return page;
    },
    stop() {
      child.kill();
      // Chrome may still be writing its profile as it goes.
      setTimeout(() => rmSync(profile, { recursive: true, force: true }), 500).unref();
    },
  };
}

/// One tab, spoken to over the DevTools protocol. It keeps every error the page meets.
class Page {
  constructor(socket) {
    this.socket = socket;
    this.next = 1;
    this.waiting = new Map();
    this.listeners = [];
    this.errors = [];
  }

  static async connect(address) {
    const socket = new WebSocket(address);
    await new Promise((resolve, reject) => {
      socket.onopen = resolve;
      socket.onerror = () => reject(new Error('could not reach the tab'));
    });
    const page = new Page(socket);
    socket.onmessage = (message) => page.heard(JSON.parse(message.data));
    await page.send('Runtime.enable');
    await page.send('Log.enable');
    await page.send('Page.enable');
    return page;
  }

  heard(message) {
    if (message.id) {
      const { resolve, reject } = this.waiting.get(message.id);
      this.waiting.delete(message.id);
      if (message.error) reject(new Error(message.error.message));
      else resolve(message.result);
      return;
    }
    const { method, params } = message;
    if (method === 'Runtime.exceptionThrown') {
      const details = params.exceptionDetails;
      this.errors.push((details.exception && details.exception.description) || details.text);
    } else if (method === 'Runtime.consoleAPICalled' && params.type === 'error') {
      this.errors.push(params.args.map((arg) => arg.value || arg.description).join(' '));
    } else if (method === 'Log.entryAdded' && params.entry.level === 'error') {
      this.errors.push(`${params.entry.text} ${params.entry.url || ''}`.trim());
    }
    this.listeners.forEach((listener) => listener(method, params));
  }

  send(method, params = {}) {
    const id = this.next++;
    this.socket.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.waiting.set(id, { resolve, reject }));
  }

  async goto(url) {
    const loaded = new Promise((resolve) => {
      const listener = (method) => {
        if (method !== 'Page.loadEventFired') return;
        this.listeners = this.listeners.filter((l) => l !== listener);
        resolve();
      };
      this.listeners.push(listener);
    });
    await this.send('Page.navigate', { url });
    await loaded;
  }

  /// The value of `expression` in the page, awaited if it is a promise. It must be JSON.
  async eval(expression) {
    const { result, exceptionDetails } = await this.send('Runtime.evaluate', {
      expression, awaitPromise: true, returnByValue: true,
    });
    if (exceptionDetails) {
      const thrown = exceptionDetails.exception && exceptionDetails.exception.description;
      throw new Error(`${thrown || exceptionDetails.text}\n  in: ${expression}`);
    }
    return result.value;
  }

  /// Touch the page at a point, as a finger would.
  async tap(x, y) {
    const point = { x, y, radiusX: 1, radiusY: 1, force: 1, id: 0 };
    await this.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] });
    await this.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  }

  /// Click the middle of the element `selector` finds.
  async click(selector) {
    const box = await this.eval(`(() => {
      const node = document.querySelector(${JSON.stringify(selector)});
      if (!node) return null;
      node.scrollIntoView({ block: 'center' });
      const r = node.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2, seen: r.width > 0 && r.height > 0 };
    })()`);
    if (!box || !box.seen) throw new Error(`nothing to click at ${selector}`);
    for (const type of ['mousePressed', 'mouseReleased']) {
      await this.send('Input.dispatchMouseEvent', { type, x: box.x, y: box.y, button: 'left', clickCount: 1 });
    }
  }

  /// Close the tab itself, not only the line to it: an open tab goes on polling the server,
  /// and Chrome allows a host only six connections at a time.
  async close() {
    this.socket.close();
    await this.closer();
  }
}

/// Photos of the given shapes ([width, height] each), made as JPEGs by the browser and put
/// where the frame keeps synced photos. Returns their paths as the server names them.
export async function addPhotos(page, server, shapes) {
  const made = await page.eval(`Promise.all(${JSON.stringify(shapes)}.map(([w, h], i) => {
    const canvas = document.createElement('canvas');
    canvas.width = w;
    canvas.height = h;
    const pen = canvas.getContext('2d');
    pen.fillStyle = 'hsl(' + (i * 47 % 360) + ', 60%, 50%)';
    pen.fillRect(0, 0, w, h);
    return canvas.toDataURL('image/jpeg', 0.6).split(',')[1];
  }))`);
  return made.map((base64, i) => {
    const name = `fs_${(0xabc000 + i).toString(16).padStart(16, '0')}.jpg`;
    writeFileSync(join(server.photos, 'frame-sync', name), Buffer.from(base64, 'base64'));
    return `frame-sync/${name}`;
  });
}
