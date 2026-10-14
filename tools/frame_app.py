#!/usr/bin/env python3
"""Photo manager for the Dragon Touch frame: see what is on it, delete photos, add more.

    ./frame_app.py            # starts the app and opens it in your browser

Shows every photo on the frame (synced, added here, or sent from the phone
app), lets you delete any of them, and add photos from this computer by
picking or dropping files. A Sync tab runs frame_sync.py for folders and
Google Photos albums. It only listens on this computer (127.0.0.1).
"""

import argparse
import hashlib
import json
import re
import secrets
import threading
import webbrowser
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlparse

import frame_sync as fs

CLOUD_DIR = "/storage/emulated/0/CloudAlbum/cloud"  # everything the frame's photo app shows lives here
SYNC_SUBDIR = fs.REMOTE_DIR[len(CLOUD_DIR) + 1 :]
ADDED_PREFIX = "up_"  # photos added through this app; frame_sync.py only manages fs_ files, so a sync never removes these
SETTINGS_FILE = fs.CONFIG_DIR / "app.json"
SAFE_REL = re.compile(r"\w[\w./-]*\.(?:jpe?g|png|webp)", re.IGNORECASE)
MIME = {"jpg": "image/jpeg", "jpeg": "image/jpeg", "webp": "image/webp", "png": "image/png"}
MAX_UPLOAD = 200 * 1024 * 1024


class ApiError(Exception):
    def __init__(self, message, status=400):
        super().__init__(message)
        self.status = status


def safe_rel(rel):
    return isinstance(rel, str) and bool(SAFE_REL.fullmatch(rel)) and ".." not in rel


def kind_of(rel):
    """Where a photo came from: 'synced' (frame_sync), 'added' (this app) or 'phone' (the vendor's phone app)."""
    folder, _, name = rel.rpartition("/")
    if folder == SYNC_SUBDIR:
        return "synced" if fs.SYNCED_NAME.fullmatch(name) else "added"
    return "phone"


class App:
    def __init__(self, host):
        self.frame = fs.Frame(host)
        self.token = secrets.token_urlsafe(24)
        self.reads = threading.BoundedSemaphore(6)  # parallel photo reads over adb
        self.job_lock = threading.Lock()
        self.job = {"running": False, "log": deque(maxlen=300), "progress": None, "result": None}
        fs.listeners.append(self.on_sync_event)

    # --- frame access -------------------------------------------------------

    def call(self, fn, *args):
        """Run a frame operation, reconnecting once; frame_sync reports failures via SystemExit."""
        for attempt in (1, 2):
            try:
                return fn(*args)
            except SystemExit as e:
                if attempt == 2:
                    raise ApiError(str(e.code).removeprefix("error: "), 502)
                try:
                    self.frame.connect()
                except SystemExit as e2:
                    raise ApiError(str(e2.code).removeprefix("error: "), 502)

    def list_photos(self):
        """Every photo on the frame, newest first, as [relative path, kind]."""
        out = self.frame.shell(
            f"cd {CLOUD_DIR} && find . -type f \\( -name '*.jpg' -o -name '*.jpeg' -o -name '*.png' -o -name '*.webp' \\)"
            " -not -path './.downloading/*' -exec stat -c '%Y %n' {} +"
        )
        found = []
        for line in out.splitlines():
            mtime, _, path = line.partition(" ")
            rel = path.removeprefix("./")
            if mtime.isdigit() and safe_rel(rel):
                found.append((int(mtime), rel))
        found.sort(reverse=True)
        return [[rel, kind_of(rel)] for _, rel in found]

    def now_showing(self):
        path = self.frame.current_photo()
        if not path or not path.startswith(CLOUD_DIR + "/"):
            return None
        rel = path[len(CLOUD_DIR) + 1 :]
        return {"rel": rel, "kind": kind_of(rel)} if safe_rel(rel) else None

    def state(self, full):
        out = {"job": self.job_view(), "excluded": sorted(fs.load_excluded())}
        try:
            out["now"] = self.call(self.now_showing)
            if full:
                out["photos"] = self.call(self.list_photos)
                out["free"] = self.call(self.frame.free_space)
                out["sources"] = self.settings().get("sources", [])
            out["reachable"] = True
        except ApiError as e:
            out.update(reachable=False, error=str(e))
        return out

    def photo(self, rel):
        if not safe_rel(rel):
            raise ApiError("not found", 404)
        with self.reads:
            data = self.call(self.frame.read_file, f"{CLOUD_DIR}/{rel}")
        if not data or data.startswith(b"cat: "):
            raise ApiError("that photo is no longer on the frame", 404)
        return data

    # --- actions ------------------------------------------------------------

    def delete(self, rels):
        if not isinstance(rels, list) or not rels or not all(safe_rel(r) for r in rels):
            raise ApiError("expected a list of photo paths")
        self.call(self._delete, rels)
        # A synced photo would come back at the next sync unless it is also remembered as excluded.
        synced = {r.rpartition("/")[2] for r in rels if kind_of(r) == "synced"}
        if synced:
            fs.save_excluded(fs.load_excluded() | synced)
        fs.log.info("app: deleted %d photos: %s", len(rels), " ".join(rels))
        return {"deleted": len(rels)}

    def _delete(self, rels):
        for i in range(0, len(rels), 40):
            paths = [f"{CLOUD_DIR}/{r}" for r in rels[i : i + 40]]
            # Phone-app photos have an .md5 file next to them.
            self.frame.shell("rm -f " + " ".join(f"'{p}' '{p}.md5'" for p in paths))
            where = "_data IN (" + ",".join(f"'{p}'" for p in paths) + ")"
            self.frame.shell(f'content delete --uri {fs.MEDIA_URI} --where "{where}"', check=False)

    def add(self, filename, data):
        """Convert one uploaded image and put it on the frame."""
        ext = Path(filename).suffix.lower()
        if ext not in fs.IMAGE_EXTS:
            raise ApiError(f"{filename}: not a supported image type")
        name = f"{ADDED_PREFIX}{hashlib.sha1(data).hexdigest()[:16]}.jpg"
        fs.CACHE_DIR.mkdir(parents=True, exist_ok=True)
        src = fs.CACHE_DIR / f"incoming-{secrets.token_hex(8)}{ext}"
        dest = fs.CACHE_DIR / name
        try:
            src.write_bytes(data)
            try:
                fs.convert(src, dest)
            except RuntimeError as e:
                raise ApiError(f"{filename}: {str(e).split(': ', 1)[-1]}")
            remote = f"{fs.REMOTE_DIR}/{name}"
            self.call(self.frame.shell, f"mkdir -p {fs.REMOTE_DIR}")
            self.call(self.frame.adb, "push", str(dest), remote)
            self.call(self.frame.shell, f"am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d file://{remote}")
        finally:
            src.unlink(missing_ok=True)
            dest.unlink(missing_ok=True)
        fs.log.info("app: added %s as %s", filename, name)
        return {"rel": f"{SYNC_SUBDIR}/{name}"}

    def restore(self, names):
        excluded = fs.load_excluded()
        if names != "all" and not (isinstance(names, list) and all(isinstance(n, str) for n in names)):
            raise ApiError("expected a list of names")
        names = excluded if names == "all" else set(names) & excluded
        fs.save_excluded(excluded - names)
        fs.log.info("app: re-included %d photos", len(names))
        return {"restored": len(names)}

    def settings(self):
        try:
            return json.loads(SETTINGS_FILE.read_text())
        except (FileNotFoundError, ValueError):
            return {}

    # --- sync job -----------------------------------------------------------

    def on_sync_event(self, kind, payload):
        if kind == "log":
            self.job["log"].append(payload)
        elif kind == "progress":
            self.job["progress"] = payload

    def job_view(self):
        j = self.job
        return {"running": j["running"], "log": list(j["log"]), "progress": j["progress"], "result": j["result"]}

    def start_sync(self, body):
        sources = [s.strip() for s in body.get("sources", []) if isinstance(s, str) and s.strip()]
        if not sources:
            raise ApiError("add at least one folder or Google Photos shared-album link")
        limit = body.get("limit")
        if limit is not None and (not isinstance(limit, int) or limit < 0):
            raise ApiError("limit must be a positive number")
        with self.job_lock:
            if self.job["running"]:
                raise ApiError("a sync is already running", 409)
            self.job.update(running=True, progress=None, result=None)
            self.job["log"].clear()
        SETTINGS_FILE.parent.mkdir(parents=True, exist_ok=True)
        SETTINGS_FILE.write_text(json.dumps({"sources": sources}))
        args = argparse.Namespace(
            sources=sources, delete=bool(body.get("delete")), limit=limit, dry_run=False, refresh=bool(body.get("refresh"))
        )
        threading.Thread(target=self.run_sync, args=(args,), daemon=True).start()
        return {"started": True}

    def run_sync(self, args):
        fs.log.info("--- app sync %s", " ".join(args.sources))
        try:
            fs.cmd_sync(self.frame, args)
            result = {"ok": True}
        except SystemExit as e:  # frame_sync exits with a message on errors, or 1 when some photos failed
            failed_some = e.code == 1
            result = {"ok": failed_some, "error": None if failed_some else str(e.code).removeprefix("error: ")}
        except Exception as e:
            fs.log.exception("app sync crashed")
            result = {"ok": False, "error": f"{type(e).__name__}: {e}"}
        self.job.update(running=False, result=result)


class Handler(BaseHTTPRequestHandler):
    app = None  # set in main()

    def log_message(self, *_):
        pass

    def send(self, status, body, ctype="application/json", cache=False):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "private, max-age=31536000, immutable" if cache else "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        self.wfile.write(data)

    def authorised(self, query):
        """Every request needs the per-run token, so other websites cannot drive the app."""
        port = self.server.server_address[1]
        if self.headers.get("Host") not in (f"127.0.0.1:{port}", f"localhost:{port}"):
            return False
        supplied = self.headers.get("X-Token") or query.get("t", [""])[0]
        return secrets.compare_digest(supplied, self.app.token)

    def read_body(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_UPLOAD:
            raise ApiError("that file is too large", 413)
        return self.rfile.read(length)

    def handle_request(self, method):
        url = urlparse(self.path)
        query = parse_qs(url.query)
        app = self.app
        try:
            if method == "GET" and url.path == "/":
                if not self.authorised(query):
                    return self.send(403, b"Open the link printed by frame_app.py.", "text/plain")
                return self.send(200, PAGE.replace("__TOKEN__", app.token).encode(), "text/html; charset=utf-8")
            if not self.authorised(query):
                raise ApiError("forbidden", 403)
            if method == "GET" and url.path == "/api/state":
                return self.send(200, app.state(full="full" in query))
            if method == "GET" and url.path == "/file":
                rel = query.get("p", [""])[0]
                data = app.photo(rel)
                return self.send(200, data, MIME.get(rel.rsplit(".", 1)[-1].lower(), "application/octet-stream"), cache=True)
            if method == "POST" and url.path == "/api/add":
                return self.send(200, app.add(unquote(self.headers.get("X-Filename", "")), self.read_body()))
            if method == "POST":
                body = json.loads(self.read_body() or b"{}")
                if url.path == "/api/delete":
                    return self.send(200, app.delete(body.get("paths")))
                if url.path == "/api/restore":
                    return self.send(200, app.restore(body.get("names")))
                if url.path == "/api/sync":
                    return self.send(200, app.start_sync(body))
                if url.path == "/api/refresh-slideshow":
                    app.call(app.frame.refresh_slideshow)
                    return self.send(200, {"ok": True})
            raise ApiError("not found", 404)
        except ApiError as e:
            self.send(e.status, {"error": str(e)})
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as e:
            fs.log.exception("app request failed")
            self.send(500, {"error": f"{type(e).__name__}: {e}"})

    def do_GET(self):
        self.handle_request("GET")

    def do_POST(self):
        self.handle_request("POST")


PAGE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Photo Frame</title>
<style>
:root {
  --bg: #f6f5f2; --panel: #ffffff; --ink: #1d1f23; --muted: #6b7078; --line: #e3e1dc;
  --accent: #0f766e; --accent-ink: #ffffff; --danger: #b42318; --ok: #15803d; --tile: #e9e7e2;
}
@media (prefers-color-scheme: dark) {
  :root { --bg: #15171a; --panel: #1e2125; --ink: #eceef1; --muted: #9aa0a8; --line: #2e3238;
          --accent: #2dd4bf; --accent-ink: #062b28; --danger: #f97066; --ok: #4ade80; --tile: #272b30; }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--ink); font: 15px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
header { display: flex; flex-wrap: wrap; align-items: center; gap: 12px 20px; padding: 14px 20px; background: var(--panel); border-bottom: 1px solid var(--line); position: sticky; top: 0; z-index: 5; }
h1 { font-size: 18px; margin: 0; letter-spacing: -0.01em; }
.status { color: var(--muted); font-size: 14px; display: flex; align-items: center; gap: 8px; }
.dot { width: 9px; height: 9px; border-radius: 50%; background: var(--muted); flex: none; }
.dot.on { background: var(--ok); } .dot.off { background: var(--danger); }
nav { display: flex; gap: 4px; margin-left: auto; }
nav button { background: none; border: 0; color: var(--muted); padding: 7px 14px; border-radius: 8px; font: inherit; cursor: pointer; }
nav button.active { background: var(--bg); color: var(--ink); font-weight: 600; }
main { max-width: 1280px; margin: 0 auto; padding: 20px; }
button.btn { font: inherit; border: 1px solid var(--line); background: var(--panel); color: var(--ink); padding: 7px 14px; border-radius: 8px; cursor: pointer; }
button.btn:hover { border-color: var(--muted); }
button.btn.primary { background: var(--accent); color: var(--accent-ink); border-color: transparent; font-weight: 600; }
button.btn.danger { color: var(--danger); }
button:disabled { opacity: .45; cursor: not-allowed; }
.now { display: flex; gap: 16px; align-items: center; background: var(--panel); border: 1px solid var(--line); border-radius: 12px; padding: 12px; margin-bottom: 18px; }
.now img { width: 132px; height: 88px; object-fit: cover; border-radius: 8px; background: var(--tile); }
.label { font-size: 12px; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); }
.now .grow { flex: 1; min-width: 0; }
.bar { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-bottom: 14px; min-height: 36px; }
.bar .spacer { flex: 1; }
.muted { color: var(--muted); }
.chips { display: flex; flex-wrap: wrap; gap: 6px; }
.chips button { font: inherit; font-size: 14px; border: 1px solid var(--line); background: none; color: var(--muted); padding: 5px 12px; border-radius: 999px; cursor: pointer; }
.chips button.active { background: var(--ink); color: var(--bg); border-color: var(--ink); }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 10px; }
.tile { position: relative; aspect-ratio: 4 / 3; border-radius: 10px; overflow: hidden; background: var(--tile); cursor: zoom-in; }
.tile img { width: 100%; height: 100%; object-fit: cover; display: block; }
.tile.sel { outline: 3px solid var(--accent); outline-offset: -3px; }
.tile .pick { position: absolute; top: 6px; left: 6px; width: 26px; height: 26px; border-radius: 50%; border: 2px solid #fff; background: rgba(0,0,0,.35); color: transparent; cursor: pointer; font-size: 14px; line-height: 1; padding: 0; }
.tile.sel .pick { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); }
.tile .tag { position: absolute; left: 6px; bottom: 6px; font-size: 11px; padding: 2px 7px; border-radius: 999px; background: rgba(0,0,0,.6); color: #fff; }
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; padding: 18px; max-width: 760px; }
.card h2 { font-size: 16px; margin: 0 0 4px; }
label.row { display: flex; gap: 8px; align-items: flex-start; margin: 10px 0; }
textarea, input[type=number] { font: inherit; color: var(--ink); background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; }
textarea { width: 100%; min-height: 84px; resize: vertical; font-family: ui-monospace, Menlo, monospace; font-size: 13px; }
input[type=number] { width: 110px; }
.progress { height: 10px; background: var(--tile); border-radius: 6px; overflow: hidden; margin: 14px 0 6px; }
.progress div { height: 100%; width: 0; background: var(--accent); transition: width .4s; }
pre.log { background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 10px; max-height: 260px; overflow: auto; font: 12px/1.5 ui-monospace, Menlo, monospace; white-space: pre-wrap; margin: 12px 0 0; }
ul.plain { list-style: none; padding: 0; margin: 12px 0 0; }
ul.plain li { display: flex; align-items: center; gap: 12px; padding: 8px 0; border-top: 1px solid var(--line); font-family: ui-monospace, Menlo, monospace; font-size: 13px; }
ul.plain li span { flex: 1; overflow-wrap: anywhere; }
.adding { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; padding: 12px 14px; margin-bottom: 18px; }
.adding .progress { margin: 8px 0 0; }
.drop { position: fixed; inset: 0; z-index: 40; display: none; align-items: center; justify-content: center; background: rgba(15,118,110,.88); color: #fff; font-size: 24px; font-weight: 600; pointer-events: none; }
.drop.on { display: flex; }
.lightbox { position: fixed; inset: 0; background: rgba(0,0,0,.88); display: none; flex-direction: column; z-index: 20; }
.lightbox.open { display: flex; }
.lightbox .stage { flex: 1; min-height: 0; display: flex; align-items: center; justify-content: center; padding: 16px; }
.lightbox img { max-width: 100%; max-height: 100%; object-fit: contain; border-radius: 6px; }
.lightbox .controls { display: flex; flex-wrap: wrap; justify-content: center; gap: 10px; padding: 12px 16px 18px; color: #ddd; align-items: center; }
.toast { position: fixed; bottom: 18px; left: 50%; transform: translateX(-50%); background: var(--ink); color: var(--bg); padding: 9px 16px; border-radius: 10px; display: none; z-index: 30; max-width: 90vw; }
.hidden { display: none !important; }
@media (max-width: 600px) { nav { margin-left: 0; width: 100%; } main { padding: 16px; } .grid { grid-template-columns: repeat(auto-fill, minmax(120px, 1fr)); } }
</style>
</head>
<body>
<header>
  <h1>Photo Frame</h1>
  <div class="status"><span class="dot" id="dot"></span><span id="status">Connecting…</span></div>
  <nav>
    <button data-tab="photos" class="active">Photos on the frame</button>
    <button data-tab="sync">Sync an album</button>
    <button data-tab="deleted">Deleted <span id="exCount"></span></button>
  </nav>
</header>
<main>
  <section id="tab-photos">
    <div class="adding hidden" id="offline"><b>The frame is not reachable.</b> <span id="offlineText"></span><div class="muted">Check that it is switched on and on the same Wi-Fi. This page keeps retrying.</div></div>
    <div class="adding hidden" id="adding"><span id="addText"></span><div class="progress"><div id="addFill"></div></div></div>
    <div class="now hidden" id="now">
      <img id="nowImg" alt="Photo on the frame right now">
      <div class="grow"><div class="label">Showing on the frame now</div><div id="nowText" class="muted"></div></div>
      <button class="btn danger" id="nowDelete">Delete this photo</button>
    </div>
    <div class="bar">
      <button class="btn primary" id="addBtn">Add photos…</button>
      <input type="file" id="addInput" accept="image/*,.heic,.heif" multiple class="hidden">
      <span class="muted">or drop them anywhere on this page</span>
      <span class="spacer"></span>
      <button class="btn" id="reloadShow" title="Closes the slideshow on the frame so it restarts with the current photos">Refresh frame slideshow</button>
    </div>
    <div class="bar">
      <div class="chips" id="chips"></div>
      <span class="spacer"></span>
      <span id="selInfo" class="hidden"><b id="selCount"></b> selected</span>
      <button class="btn hidden" id="selClear">Clear</button>
      <button class="btn danger hidden" id="selDelete">Delete selected</button>
      <button class="btn" id="prev">‹ Newer</button>
      <span id="pageInfo" class="muted"></span>
      <button class="btn" id="next">Older ›</button>
    </div>
    <div class="grid" id="grid"></div>
    <p class="muted" id="empty"></p>
  </section>

  <section id="tab-sync" class="hidden">
    <div class="card">
      <h2>Sync an album or folder to the frame</h2>
      <div class="muted">One source per line: a folder on this computer, or a Google Photos shared-album link. Only photos that are not on the frame yet are uploaded.</div>
      <label class="row"><textarea id="sources" placeholder="https://photos.app.goo.gl/…&#10;/home/me/Pictures/Frame"></textarea></label>
      <label class="row"><input type="checkbox" id="optDelete"> <span>Also remove synced photos that are no longer in these sources</span></label>
      <label class="row"><input type="checkbox" id="optRefresh" checked> <span>Refresh the frame slideshow when done</span></label>
      <label class="row" style="align-items:center"><span>Upload at most</span> <input type="number" id="optLimit" min="1" placeholder="no limit"> <span>new photos this run</span></label>
      <button class="btn primary" id="syncStart">Start sync</button>
      <div id="syncLive" class="hidden">
        <div class="progress"><div id="progFill"></div></div>
        <div id="progText" class="muted"></div>
        <pre class="log" id="log"></pre>
      </div>
    </div>
  </section>

  <section id="tab-deleted" class="hidden">
    <div class="card">
      <h2>Deleted synced photos</h2>
      <div class="muted">Synced photos you deleted are skipped by every later sync, so they stay off the frame. Restore one to let the next sync put it back.</div>
      <div class="bar" style="margin-top:12px"><button class="btn" id="restoreAll">Restore all</button></div>
      <ul class="plain" id="exList"></ul>
    </div>
  </section>
</main>

<div class="drop" id="drop">Drop photos to add them to the frame</div>
<div class="lightbox" id="lightbox">
  <div class="stage"><img id="lbImg" alt=""></div>
  <div class="controls">
    <button class="btn" id="lbPrev">‹</button>
    <span id="lbInfo"></span>
    <button class="btn" id="lbNext">›</button>
    <button class="btn danger" id="lbDelete">Delete from frame</button>
    <button class="btn" id="lbClose">Close</button>
  </div>
</div>
<div class="toast" id="toast"></div>

<script>
const TOKEN = "__TOKEN__", PAGE_SIZE = 60;
const KINDS = { all: "All", synced: "Synced", added: "Added here", phone: "From phone app" };
const $ = id => document.getElementById(id);
let photos = [], filter = "all", page = 0, selected = new Set(), excluded = [], lb = -1, jobWasRunning = false, nowRel = null, adding = false, needFull = false;

async function api(path, body) {
  const r = await fetch(path, { method: body ? "POST" : "GET", headers: { "X-Token": TOKEN, "Content-Type": "application/json" }, body: body ? JSON.stringify(body) : undefined });
  const j = await r.json();
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}
const src = rel => `/file?t=${TOKEN}&p=${encodeURIComponent(rel)}`;
const kindOf = rel => (photos.find(p => p[0] === rel) || [])[1];
const shown = () => filter === "all" ? photos : photos.filter(p => p[1] === filter);
function toast(msg) { const t = $("toast"); t.textContent = msg; t.style.display = "block"; clearTimeout(toast.h); toast.h = setTimeout(() => t.style.display = "none", 5000); }

function renderChips() {
  const chips = $("chips"); chips.textContent = "";
  for (const [k, label] of Object.entries(KINDS)) {
    const n = k === "all" ? photos.length : photos.filter(p => p[1] === k).length;
    if (k !== "all" && !n) continue;
    const b = document.createElement("button"); b.textContent = `${label} ${n.toLocaleString()}`; b.className = k === filter ? "active" : "";
    b.onclick = () => { filter = k; page = 0; render(); };
    chips.append(b);
  }
}
function render() {
  renderChips();
  const list = shown(), pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
  page = Math.min(page, pages - 1);
  const grid = $("grid"); grid.textContent = "";
  list.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE).forEach(([rel, kind], i) => {
    const tile = document.createElement("div"); tile.className = "tile" + (selected.has(rel) ? " sel" : "");
    const img = document.createElement("img"); img.loading = "lazy"; img.decoding = "async"; img.src = src(rel); img.alt = "";
    const pick = document.createElement("button"); pick.className = "pick"; pick.textContent = "✓"; pick.title = "Select";
    pick.onclick = e => { e.stopPropagation(); selected.has(rel) ? selected.delete(rel) : selected.add(rel); tile.classList.toggle("sel"); renderSelection(); };
    tile.onclick = () => openLightbox(page * PAGE_SIZE + i);
    tile.append(img, pick);
    if (kind !== "synced") { const tag = document.createElement("span"); tag.className = "tag"; tag.textContent = kind === "phone" ? "Phone app" : "Added here"; tile.append(tag); }
    grid.append(tile);
  });
  $("empty").textContent = photos.length ? "" : "There are no photos on the frame yet. Add some with the button above.";
  $("pageInfo").textContent = `Page ${page + 1} of ${pages}`;
  $("prev").disabled = page === 0; $("next").disabled = page >= pages - 1;
  renderSelection();
}
function renderSelection() {
  const n = selected.size;
  for (const id of ["selInfo", "selClear", "selDelete"]) $(id).classList.toggle("hidden", n === 0);
  $("selCount").textContent = n;
}
function renderExcluded() {
  $("exCount").textContent = excluded.length ? `(${excluded.length})` : "";
  $("restoreAll").disabled = !excluded.length;
  const list = $("exList"); list.textContent = "";
  if (!excluded.length) { const li = document.createElement("li"); li.textContent = "Nothing here."; li.className = "muted"; list.append(li); }
  for (const name of excluded) {
    const li = document.createElement("li"), span = document.createElement("span"), b = document.createElement("button");
    span.textContent = name; b.className = "btn"; b.textContent = "Restore";
    b.onclick = () => restore([name]);
    li.append(span, b); list.append(li);
  }
}
function renderNow(now) {
  $("now").classList.toggle("hidden", !now);
  if (!now) { nowRel = null; return; }
  if (now.rel !== nowRel) { nowRel = now.rel; $("nowImg").src = src(now.rel); }
  $("nowText").textContent = { synced: "Synced photo", added: "Added from this computer", phone: "Sent from the phone app" }[now.kind];
}
function renderJob(job) {
  $("syncStart").disabled = job.running;
  $("syncStart").textContent = job.running ? "Sync running…" : "Start sync";
  $("syncLive").classList.toggle("hidden", !(job.running || job.log.length));
  const p = job.progress;
  $("progFill").style.width = p && p.total ? `${Math.round(100 * p.done / p.total)}%` : (job.result && job.result.ok ? "100%" : "0%");
  let text = p ? `${p.done.toLocaleString()} of ${p.total.toLocaleString()}${p.rate ? ` · ${p.rate}/s ${p.eta}` : ""} · ${p.detail}` : (job.running ? "Preparing…" : "");
  if (!job.running && job.result) text = job.result.error ? `Failed: ${job.result.error}` : "Finished.";
  $("progText").textContent = text;
  const log = $("log"), atEnd = log.scrollTop + log.clientHeight >= log.scrollHeight - 8;
  log.classList.toggle("hidden", !job.log.length);
  log.textContent = job.log.join("\n"); if (atEnd) log.scrollTop = log.scrollHeight;
  if (jobWasRunning && !job.running) { load(true); toast(job.result && job.result.error ? "Sync failed" : "Sync finished"); }
  jobWasRunning = job.running;
}

async function load(full) {
  let s;
  try { s = await api("/api/state" + (full ? "?full=1" : "")); } catch (e) { $("dot").className = "dot off"; $("status").textContent = "App not responding"; return; }
  excluded = s.excluded; renderExcluded(); renderJob(s.job);
  $("dot").className = "dot " + (s.reachable ? "on" : "off");
  $("offline").classList.toggle("hidden", s.reachable);
  if (!s.reachable) { $("status").textContent = "Not connected"; $("offlineText").textContent = s.error; renderNow(null); needFull = true; return; }
  renderNow(s.now);
  if (full) {
    needFull = false;
    photos = s.photos;
    const have = new Set(photos.map(p => p[0])); selected = new Set([...selected].filter(r => have.has(r)));
    $("status").textContent = `Connected · ${photos.length.toLocaleString()} photos · ${s.free} free`;
    if (!$("sources").value && s.sources.length) $("sources").value = s.sources.join("\n");
    render();
  }
}

async function del(rels) {
  const phone = rels.filter(r => kindOf(r) === "phone").length;
  let q = rels.length === 1 ? "Delete this photo from the frame?" : `Delete ${rels.length} photos from the frame?`;
  if (phone) q += `\n\n${phone === rels.length ? "These were" : phone + " of them were"} sent from the phone app. They will be deleted from the frame only, and will still be listed in the phone app.`;
  if (!confirm(q)) return false;
  try { await api("/api/delete", { paths: rels }); } catch (e) { toast(e.message); return false; }
  const gone = new Set(rels); photos = photos.filter(p => !gone.has(p[0])); rels.forEach(r => selected.delete(r));
  $("status").textContent = $("status").textContent.replace(/[\d,.]+ photos/, `${photos.length.toLocaleString()} photos`);
  render(); load(false);
  toast(rels.length === 1 ? "Deleted from the frame." : `Deleted ${rels.length} photos from the frame.`);
  return true;
}
async function restore(names) {
  try { await api("/api/restore", { names }); } catch (e) { return toast(e.message); }
  await load(false); toast("Restored. Run a sync to put them back on the frame.");
}

async function addFiles(files) {
  files = [...files].filter(f => f.size);
  if (!files.length) return;
  if (adding) return toast("Still adding the previous photos.");
  adding = true; $("adding").classList.remove("hidden");
  let done = 0, failed = [];
  const step = () => { $("addText").textContent = `Adding photos to the frame: ${done} of ${files.length}`; $("addFill").style.width = `${Math.round(100 * done / files.length)}%`; };
  step();
  const queue = files.slice();
  const worker = async () => {
    for (let f; (f = queue.shift()); ) {
      try {
        const r = await fetch("/api/add", { method: "POST", headers: { "X-Token": TOKEN, "X-Filename": encodeURIComponent(f.name) }, body: f });
        if (!r.ok) throw new Error((await r.json()).error || r.statusText);
      } catch (e) { failed.push(e.message); }
      done++; step();
    }
  };
  await Promise.all([worker(), worker(), worker()]);
  adding = false; $("adding").classList.add("hidden");
  filter = "all"; page = 0; await load(true);
  const ok = files.length - failed.length;
  toast(failed.length ? `Added ${ok}, ${failed.length} failed: ${failed[0]}` : `Added ${ok} photo${ok === 1 ? "" : "s"} to the frame.`);
}

function openLightbox(i) { const list = shown(); lb = i; $("lbImg").src = src(list[i][0]); $("lbInfo").textContent = `${i + 1} / ${list.length}`; $("lightbox").classList.add("open"); }
function closeLightbox() { lb = -1; $("lightbox").classList.remove("open"); $("lbImg").removeAttribute("src"); }
function stepLightbox(d) { const n = shown().length; if (lb < 0 || !n) return; openLightbox((lb + d + n) % n); }

const TABS = ["photos", "sync", "deleted"];
function showTab(tab) {
  if (!TABS.includes(tab)) tab = "photos";
  document.querySelectorAll("nav button").forEach(b => b.classList.toggle("active", b.dataset.tab === tab));
  for (const t of TABS) $("tab-" + t).classList.toggle("hidden", t !== tab);
}
document.querySelectorAll("nav button").forEach(b => b.onclick = () => { location.hash = b.dataset.tab; });
window.onhashchange = () => showTab(location.hash.slice(1));
showTab(location.hash.slice(1));

$("prev").onclick = () => { page--; render(); scrollTo(0, 0); };
$("next").onclick = () => { page++; render(); scrollTo(0, 0); };
$("selClear").onclick = () => { selected.clear(); render(); };
$("selDelete").onclick = () => del([...selected]);
$("nowDelete").onclick = () => { if (nowRel) del([nowRel]); };
$("addBtn").onclick = () => $("addInput").click();
$("addInput").onchange = e => { addFiles(e.target.files); e.target.value = ""; };
$("reloadShow").onclick = () => api("/api/refresh-slideshow", {}).then(() => toast("The slideshow will restart with the current photos."), e => toast(e.message));
$("restoreAll").onclick = () => { if (confirm(`Restore all ${excluded.length} deleted photos?`)) api("/api/restore", { names: "all" }).then(() => load(false), e => toast(e.message)); };
$("lbClose").onclick = closeLightbox; $("lbPrev").onclick = () => stepLightbox(-1); $("lbNext").onclick = () => stepLightbox(1);
$("lbDelete").onclick = async () => { const i = lb; if (await del([shown()[i][0]])) { const n = shown().length; n ? openLightbox(Math.min(i, n - 1)) : closeLightbox(); } };
$("lightbox").onclick = e => { if (e.target.classList.contains("stage") || e.target === $("lightbox")) closeLightbox(); };
document.onkeydown = e => { if (lb < 0) return; if (e.key === "Escape") closeLightbox(); if (e.key === "ArrowLeft") stepLightbox(-1); if (e.key === "ArrowRight") stepLightbox(1); };
$("syncStart").onclick = async () => {
  const limit = parseInt($("optLimit").value, 10);
  const body = { sources: $("sources").value.split("\n"), delete: $("optDelete").checked, refresh: $("optRefresh").checked, limit: Number.isFinite(limit) ? limit : null };
  if (body.delete && !confirm("Synced photos that are not in these sources will be removed from the frame. Continue?")) return;
  try { await api("/api/sync", body); jobWasRunning = true; load(false); } catch (e) { toast(e.message); }
};

let dragDepth = 0;
const hasFiles = e => e.dataTransfer && [...e.dataTransfer.types].includes("Files");
window.addEventListener("dragenter", e => { if (hasFiles(e)) { dragDepth++; $("drop").classList.add("on"); } });
window.addEventListener("dragleave", e => { if (hasFiles(e) && --dragDepth <= 0) { dragDepth = 0; $("drop").classList.remove("on"); } });
window.addEventListener("dragover", e => { if (hasFiles(e)) e.preventDefault(); });
window.addEventListener("drop", e => { if (!hasFiles(e)) return; e.preventDefault(); dragDepth = 0; $("drop").classList.remove("on"); addFiles(e.dataTransfer.files); });

load(true);
setInterval(() => load(needFull), 4000);
</script>
</body>
</html>
"""


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--host", help=fs.HOST_HELP)
    p.add_argument("--port", type=int, default=8765, help="local port for the app (default 8765)")
    p.add_argument("--no-browser", action="store_true", help="do not open the browser automatically")
    args = p.parse_args()

    fs.setup_logging()
    app = App(args.host or fs.frame_host())
    Handler.app = app
    try:
        server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    except OSError as e:
        fs.die(f"cannot listen on port {args.port} ({e.strerror}); try --port")
    server.daemon_threads = True
    url = f"http://127.0.0.1:{args.port}/?t={app.token}"
    print(f"Photo Frame manager running at {url}\nPress Ctrl-C to stop.")
    try:
        app.frame.connect()
    except SystemExit as e:
        print(f"warning: {e.code} (the app will keep retrying)")
    if not args.no_browser:
        webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopped.")


if __name__ == "__main__":
    main()
