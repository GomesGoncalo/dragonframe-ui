#!/usr/bin/env python3
"""Sync photos to a Dragon Touch calendar / photo frame (model TM15) over ADB.

The frame runs Android 10 with ADB open on port 5555. Its photo app shows every
image indexed in MediaStore under /sdcard/CloudAlbum/cloud/, so syncing is:
resize -> adb push into a dedicated folder -> ask Android to rescan that folder.

    frame_sync.py sync ~/Pictures/Frame              # add new photos
    frame_sync.py sync ~/Pictures/Frame --delete     # mirror (also remove)
    frame_sync.py sync https://photos.app.goo.gl/... # Google Photos shared album
    frame_sync.py status
    frame_sync.py clear
    frame_sync.py exclude                            # drop the photo on screen now
    frame_sync.py include fs_0123456789abcdef.jpg    # undo an exclude

Every run is logged to ~/Library/Logs/frame-sync.log.

Only files this tool created (fs_*.jpg in its own folder) are ever touched;
photos sent through the official app are left alone.

Runs on macOS and Linux. Needs `adb` on PATH; local folders are converted with
`sips` on macOS and ImageMagick on Linux. For a graphical manager run frame_app.py.
"""

import argparse
import atexit
import hashlib
import json
import logging
import os
import re
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

# The frame's address differs from one network to the next, so it is not kept in this
# repository: each developer gives it in $FRAME_HOST or in the file .frame at the top of
# the repository, which git ignores. See frame_host().
HOST_FILE = Path(__file__).resolve().parent.parent / ".frame"
HOST_HELP = "frame address, such as 192.168.1.20 (default: $FRAME_HOST, or the file .frame)"
REMOTE_DIR = "/storage/emulated/0/CloudAlbum/cloud/frame-sync"
PREFIX = "fs_"
if sys.platform == "darwin":
    CACHE_DIR = Path.home() / "Library" / "Caches" / "frame-sync"
    LOG_FILE = Path.home() / "Library" / "Logs" / "frame-sync.log"
    CONFIG_DIR = Path.home() / "Library" / "Application Support" / "frame-sync"
else:
    CACHE_DIR = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "frame-sync"
    LOG_FILE = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state") / "frame-sync" / "frame-sync.log"
    CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "frame-sync"
EXCLUDED_FILE = CONFIG_DIR / "excluded.txt"
SYNCED_NAME = re.compile(r"fs_[0-9a-f]{16}\.jpg")
APP_PREFS = "/data/data/com.fujia.calendar/shared_prefs/spUtils.xml"
IMAGE_EXTS = {".jpg", ".jpeg", ".png", ".heic", ".heif", ".webp", ".gif", ".tif", ".tiff", ".bmp"}
MEDIA_URI = "content://media/external/images/media"
BATCH = 100  # photos prepared locally at a time
USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/126.0 Safari/537.36"
)
GPHOTOS_RPC = "snAcKc"  # Google Photos web UI call that lists one page of an album
GPHOTOS_RPC_URL = "https://photos.google.com/_/PhotosUi/data/batchexecute"
GPHOTOS_SHARE = re.compile(r"/share/([\w-]+)\?(?:.*&)?key=([\w-]+)")

log = logging.getLogger("frame-sync")
log.addHandler(logging.NullHandler())


listeners = []  # callables (kind, payload); the web app uses these to follow a running sync


def emit(kind, payload):
    for listener in listeners:
        listener(kind, payload)


def say(msg):
    """Print a line for the user and record it in the log file."""
    print(msg)
    log.info(msg.strip())
    emit("log", msg.strip())


def frame_host():
    """The frame's address as the developer gave it: $FRAME_HOST, or the first line of .frame."""
    host = os.environ.get("FRAME_HOST", "").strip()
    if not host and HOST_FILE.is_file():
        lines = (line.split("#")[0].strip() for line in HOST_FILE.read_text().splitlines())
        host = next((line for line in lines if line), "")
    if not host:
        die(
            "Which frame? Give its address on your network, once, in a file at the top of the repository:\n"
            f"    echo 192.168.1.20 > {HOST_FILE}\n"
            "or as FRAME_HOST=192.168.1.20 in the environment, or with --host. "
            "The frame shows its address in Settings, under Wi-Fi."
        )
    return host


def frame_name(host=None):
    """The frame's address without a port, for reaching its web server rather than adb."""
    host = host or frame_host()
    return host.rpartition(":")[0] if host.count(":") == 1 else host


def die(msg):
    log.error(msg)
    sys.exit(f"error: {msg}")


def duration(seconds):
    m, sec = divmod(int(seconds), 60)
    h, m = divmod(m, 60)
    return f"{h}h{m:02d}m" if h else f"{m}m{sec:02d}s" if m else f"{sec}s"


class Progress:
    """A one-line progress bar on a terminal; plain periodic lines when output is piped."""

    def __init__(self, total, unit="photos"):
        self.total, self.unit = total, unit
        self.tty = sys.stdout.isatty()
        self.start = self.last_plain = time.time()
        self.done = 0

    def update(self, done, detail=""):
        self.done = done
        elapsed = time.time() - self.start
        rate = done / elapsed if elapsed > 0 else 0
        eta = f"ETA {duration((self.total - done) / rate)}" if rate and done < self.total else ""
        pct = done / self.total if self.total else 1
        stats = f"{pct:4.0%}  {done}/{self.total} {self.unit}  {rate:.1f}/s  {eta}  {detail}".rstrip()
        emit("progress", {"done": done, "total": self.total, "rate": round(rate, 1), "eta": eta, "detail": detail})
        if self.tty:
            width = shutil.get_terminal_size().columns
            bar_w = max(10, min(30, width - len(stats) - 4))
            filled = int(bar_w * pct)
            line = f"[{'█' * filled}{'░' * (bar_w - filled)}] {stats}"
            print("\r" + line[: width - 1].ljust(width - 1), end="", flush=True)
        elif time.time() - self.last_plain >= 30 or done >= self.total:
            self.last_plain = time.time()
            print(stats, flush=True)

    def note(self, msg):
        """Print a message without leaving a broken bar behind."""
        if self.tty:
            print("\r" + " " * (shutil.get_terminal_size().columns - 1) + "\r", end="")
        say(msg)

    def finish(self):
        if self.tty:
            print()


class Frame:
    def __init__(self, host):
        self.host = host if ":" in host else f"{host}:5555"
        name, _, port = self.host.rpartition(":")
        self.target = (name, int(port))  # the frame's real address; self.host may become a local relay
        self.relay_host = None

    def adb(self, *args, check=True):
        r = subprocess.run(["adb", "-s", self.host, *args], capture_output=True, text=True)
        if check and r.returncode != 0:
            die(f"adb {' '.join(args[:2])} failed: {(r.stderr or r.stdout).strip()}")
        return r.stdout

    def shell(self, cmd, check=True):
        return self.adb("shell", cmd, check=check)

    def connect(self):
        """Connect adb to the frame, directly or, if adb itself is blocked, through this process."""
        if self._adb_connect(self.host):
            return
        try:
            socket.create_connection(self.target, 5).close()
        except OSError:
            die(f"cannot reach the frame at {self.target[0]}:{self.target[1]} (is it on, and on this network?)")
        # The frame answers us but not adb: macOS can deny adb local-network access on its own.
        # Forward a local port to the frame from this process and point adb at that instead.
        if self.relay_host is None:
            self.relay_host = f"127.0.0.1:{self._start_relay()}"
            atexit.register(subprocess.run, ["adb", "disconnect", self.relay_host], capture_output=True)
            log.info("adb cannot reach the frame directly; relaying through %s", self.relay_host)
        subprocess.run(["adb", "disconnect", self.relay_host], capture_output=True)
        if not self._adb_connect(self.relay_host):
            die("the frame is reachable, but adb could not connect to it")
        self.host = self.relay_host

    @staticmethod
    def _adb_connect(host):
        try:
            subprocess.run(["adb", "connect", host], capture_output=True, text=True, timeout=15)
        except FileNotFoundError:
            hint = "brew install --cask android-platform-tools" if sys.platform == "darwin" else "your package manager (e.g. apt install adb)"
            die(f"adb not found. Install it with: {hint}")
        except subprocess.TimeoutExpired:
            return False
        r = subprocess.run(["adb", "-s", host, "get-state"], capture_output=True, text=True)
        return r.stdout.strip() == "device"

    def _start_relay(self):
        """Listen on a free local port and forward every connection to the frame; returns the port."""
        server = socket.socket()
        server.bind(("127.0.0.1", 0))
        server.listen()

        def pipe(src, dst):
            try:
                while data := src.recv(65536):
                    dst.sendall(data)
            except OSError:
                pass
            finally:
                for sock in (src, dst):
                    try:
                        sock.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass

        def serve():
            while True:
                client, _ = server.accept()
                try:
                    upstream = socket.create_connection(self.target, 10)
                except OSError:
                    client.close()
                    continue
                threading.Thread(target=pipe, args=(client, upstream), daemon=True).start()
                threading.Thread(target=pipe, args=(upstream, client), daemon=True).start()

        threading.Thread(target=serve, daemon=True).start()
        return server.getsockname()[1]

    def remote_files(self):
        return set(self.remote_files_newest_first())

    def remote_files_newest_first(self):
        out = self.shell(f"ls -1t {REMOTE_DIR} 2>/dev/null", check=False)
        return [n for n in out.split() if SYNCED_NAME.fullmatch(n)]

    def read_file(self, path):
        """Return the bytes of a file on the frame."""
        r = subprocess.run(["adb", "-s", self.host, "exec-out", f"cat '{path}'"], capture_output=True)
        if r.returncode != 0:
            die(f"could not read {path} from the frame")
        return r.stdout

    def free_space(self):
        return self.shell("df -h /data | tail -1").split()[3]

    def indexed_count(self):
        out = self.shell(
            f"content query --uri {MEDIA_URI} --projection _id --where \"_data LIKE '{REMOTE_DIR}/{PREFIX}%'\"",
            check=False,
        )
        return sum(1 for line in out.splitlines() if line.startswith("Row:"))

    def rescan(self, expected):
        """Ask MediaStore to rescan our folder and wait until it has caught up."""
        self.shell(f"am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d file://{REMOTE_DIR}")
        deadline = time.time() + 30 + expected * 0.2
        tty = sys.stdout.isatty()
        while True:
            time.sleep(2)
            n = self.indexed_count()
            if tty:
                print(f"\rWaiting for the frame to index photos: {n}/{expected} ", end="", flush=True)
            if n == expected or time.time() > deadline:
                if tty:
                    print()
                if n != expected:
                    say(f"warning: frame has indexed {n} of {expected} photos so far; it may still be scanning")
                log.info("frame index: %d of %d", n, expected)
                return n == expected

    def current_photo(self):
        """Path of the photo the slideshow is showing, or None if no slideshow is running."""
        if "SlideActivity" not in self.shell("dumpsys window | grep mCurrentFocus", check=False):
            return None
        m = re.search(r'name="slide_playing_data">([^<]+)<', self.shell(f"cat {APP_PREFS}", check=False))
        return m.group(1) if m else None

    def remove(self, names):
        """Delete synced photos from the frame and from its media index."""
        names = list(names)
        for i in range(0, len(names), 50):
            paths = [f"{REMOTE_DIR}/{n}" for n in names[i : i + 50]]
            self.shell("rm -f " + " ".join(paths))
            where = "_data IN (" + ",".join(f"'{p}'" for p in paths) + ")"
            self.shell(f'content delete --uri {MEDIA_URI} --where "{where}"', check=False)

    def refresh_slideshow(self):
        """Leave a running slideshow so the photo list reloads next time it starts.

        Only ever sends BACK while the slideshow is in front: BACK on the home
        screen sends the launcher into a restart loop.
        """
        if "SlideActivity" in self.shell("dumpsys window | grep mCurrentFocus", check=False):
            self.shell("input keyevent BACK")
            say("Closed the running slideshow; it will include the new photos when it next starts.")


def load_excluded():
    try:
        return {line.strip() for line in EXCLUDED_FILE.read_text().splitlines() if line.strip()}
    except FileNotFoundError:
        return set()


def save_excluded(names):
    EXCLUDED_FILE.parent.mkdir(parents=True, exist_ok=True)
    EXCLUDED_FILE.write_text("".join(f"{n}\n" for n in sorted(names)))


def key_for(*parts):
    return hashlib.sha1("\0".join(str(p) for p in parts).encode()).hexdigest()[:16]


def local_items(folder):
    """Yield (remote_name, make) for each image under folder; make(dest) writes the frame-ready JPEG."""
    root = Path(folder).expanduser().resolve()
    if not root.is_dir():
        die(f"not a folder: {folder}")
    for path in sorted(root.rglob("*")):
        if path.suffix.lower() not in IMAGE_EXTS or any(p.startswith(".") for p in path.relative_to(root).parts):
            continue
        st = path.stat()
        name = f"{PREFIX}{key_for(path, st.st_size, st.st_mtime_ns)}.jpg"
        yield name, lambda dest, src=path: convert(src, dest)


def convert(src, dest, max_px=1920):
    """Write a frame-ready JPEG: sips on macOS, ImageMagick elsewhere."""
    tmp = dest.with_suffix(".tmp.jpg")
    if sys.platform == "darwin":
        cmd = ["sips", "-s", "format", "jpeg", "-s", "formatOptions", "85", "-Z", str(max_px), str(src), "--out", str(tmp)]
    else:
        magick = shutil.which("magick") or shutil.which("convert")
        if not magick:
            raise RuntimeError("ImageMagick is needed to convert local photos (e.g. apt install imagemagick)")
        cmd = [magick, str(src), "-auto-orient", "-resize", f"{max_px}x{max_px}>", "-quality", "85", str(tmp)]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0 or not tmp.exists():
        tmp.unlink(missing_ok=True)
        reason = (r.stderr.strip().splitlines() or ["unknown error"])[0]
        raise RuntimeError(f"could not convert {src}: {reason}")
    tmp.replace(dest)


def fetch(url, data=None, user_agent=USER_AGENT, timeout=60):
    req = urllib.request.Request(url, data=data, headers={"User-Agent": user_agent})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.geturl(), resp.read()


def gphotos_page(album, auth_key, token):
    """Return (items, next_token, album_total) for one page of a shared album."""
    f_req = json.dumps([[[GPHOTOS_RPC, json.dumps([album, token, None, auth_key]), None, "generic"]]])
    _, body = fetch(f"{GPHOTOS_RPC_URL}?rpcids={GPHOTOS_RPC}", urllib.parse.urlencode({"f.req": f_req}).encode())
    try:
        rows = json.loads(body.decode().split("\n", 1)[1])
        payload = next(r[2] for r in rows if r[0] == "wrb.fr" and r[1] == GPHOTOS_RPC)
        data = json.loads(payload)
        return data[1] or [], data[2] or None, data[3][21]
    except (IndexError, KeyError, StopIteration, TypeError, ValueError):
        die("Google Photos returned an unexpected response; the album may be private or Google changed its format")


def gphotos_items(album_url):
    """Return (items, complete) for a Google Photos *shared album link* (Sharing -> Create link).

    Google's API no longer allows reading a user's library, so this uses the
    same unauthenticated call the public album web page makes, page by page.
    """
    # Short links only redirect for non-browser user agents; browsers get a JS page.
    final_url, _ = fetch(album_url, user_agent="curl/8.0")
    m = GPHOTOS_SHARE.search(final_url)
    if not m:
        die("not a Google Photos shared-album link (expected photos.app.goo.gl/... or photos.google.com/share/...?key=...)")
    album, auth_key = m.groups()

    items, seen, token, total = {}, set(), None, None
    tty = sys.stdout.isatty()
    while True:
        page, token, total = gphotos_page(album, auth_key, token)
        listed = len(seen) + len(page)
        emit("progress", {"done": listed, "total": total or listed, "rate": 0, "eta": "", "detail": "reading the album from Google Photos"})
        if tty:
            print(f"\rListing album: {listed}/{total} items ", end="", flush=True)
        for entry in page:
            media_key, base_url = entry[0], entry[1][0]
            seen.add(media_key)
            if isinstance(entry[-1], dict) and "76647426" in entry[-1]:  # video
                continue
            name = f"{PREFIX}{key_for('gphotos', media_key)}.jpg"
            items[name] = lambda dest, u=base_url: download(u + "=w1920-h1920", dest)
        if not token or not page:
            break
    if tty:
        print()
    complete = total is None or len(seen) >= total
    if not complete:
        say(f"warning: album reports {total} items but only {len(seen)} were listed")
    if len(seen) > len(items):
        say(f"  skipping {len(seen) - len(items)} videos")
    return items, complete


def download(url, dest):
    _, data = fetch(url)
    tmp = dest.with_suffix(".tmp.jpg")
    tmp.write_bytes(data)
    tmp.replace(dest)


def cmd_sync(frame, args):
    wanted, complete = {}, True
    for source in args.sources:
        if source.startswith(("http://", "https://")):
            items, ok = gphotos_items(source)
            complete = complete and ok
        else:
            items = dict(local_items(source))
        before = len(wanted)
        wanted.update(items)
        say(f"{source}: {len(wanted) - before} photos")
    excluded = load_excluded() & set(wanted)
    if excluded:
        say(f"Leaving out {len(excluded)} excluded photos (see: frame_sync.py excluded)")
        for name in excluded:
            del wanted[name]
    if args.delete and not complete:
        say("warning: not removing anything from the frame because an album listing was incomplete")
        args.delete = False

    frame.connect()
    remote = frame.remote_files()
    to_add = sorted(set(wanted) - remote)
    to_remove = sorted(remote - set(wanted)) if args.delete else []
    say(f"On frame: {len(remote)}   to upload: {len(to_add)}   to remove: {len(to_remove)}")
    if args.limit is not None and len(to_add) > args.limit:
        to_add = to_add[: args.limit]
        say(f"Limiting this run to {len(to_add)} uploads; run again for the rest.")
    if args.dry_run:
        return
    if not (to_add or to_remove):
        # An interrupted run can leave uploaded files the frame has not indexed yet.
        if frame.indexed_count() != len(remote):
            frame.rescan(len(remote))
        return

    # Work in small batches (prepare -> upload -> delete) so local disk use stays
    # at one batch, however large the sync is.
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    for stale in CACHE_DIR.glob(f"{PREFIX}*.jpg"):
        stale.unlink()
    frame.shell(f"mkdir -p {REMOTE_DIR}")
    uploaded = failed = 0
    progress = Progress(len(to_add))
    batches = -(-len(to_add) // BATCH)

    def prepare(name):
        dest = CACHE_DIR / name
        wanted[name](dest)
        return dest

    with ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as pool:
        for b, start in enumerate(range(0, len(to_add), BATCH), 1):
            names = to_add[start : start + BATCH]
            futures = {pool.submit(prepare, n): n for n in names}
            ready = []
            for fut in as_completed(futures):
                try:
                    ready.append(fut.result())
                except Exception as e:
                    failed += 1
                    progress.note(f"  skipped {futures[fut]}: {e}")
                progress.update(uploaded + failed, f"{failed} failed  " * bool(failed) + f"batch {b}/{batches}: preparing {len(ready)}/{len(names)}")
            if ready:
                progress.update(uploaded + failed, f"{failed} failed  " * bool(failed) + f"batch {b}/{batches}: uploading {len(ready)} to frame")
                frame.adb("push", *map(str, ready), REMOTE_DIR + "/")
                uploaded += len(ready)
            for path in ready:
                path.unlink(missing_ok=True)
            progress.update(uploaded + failed, f"{failed} failed  " * bool(failed) + f"batch {b}/{batches} done")
            log.info("batch %d/%d: uploaded %d, total %d/%d, failed %d", b, batches, len(ready), uploaded, len(to_add), failed)
    if to_add:
        progress.finish()

    for i in range(0, len(to_remove), BATCH):
        frame.shell("rm -f " + " ".join(f"{REMOTE_DIR}/{n}" for n in to_remove[i : i + BATCH]))

    total = len(frame.remote_files())
    frame.rescan(total)
    if args.refresh:
        frame.refresh_slideshow()
    say(
        f"Done in {duration(time.time() - progress.start)}: uploaded {uploaded}, removed {len(to_remove)}, "
        f"failed {failed}. {total} synced photos on the frame."
    )
    if failed:
        sys.exit(1)


def cmd_status(frame, _args):
    frame.connect()
    files = len(frame.remote_files())
    size = frame.shell(f"du -sh {REMOTE_DIR} 2>/dev/null | cut -f1", check=False).strip() or "0"
    free = frame.free_space()
    print(f"Synced photos on frame: {files} ({size}), indexed by the frame: {frame.indexed_count()}")
    print(f"Free space: {free}")


def cmd_exclude(frame, args):
    frame.connect()
    names = args.names
    if not names:
        current = frame.current_photo()
        if not current:
            die("no slideshow is running on the frame; start it, or pass a photo name (fs_....jpg)")
        if not current.startswith(REMOTE_DIR + "/"):
            die("the photo on screen was sent from the phone app, not synced by this tool; delete it in the app")
        names = [current.rsplit("/", 1)[1]]
    bad = [n for n in names if not SYNCED_NAME.fullmatch(n)]
    if bad:
        die(f"not a synced photo name: {', '.join(bad)} (expected fs_<16 hex digits>.jpg)")
    frame.remove(names)
    save_excluded(load_excluded() | set(names))
    for name in names:
        say(f"Removed {name} from the frame; it will not be synced again. Undo: frame_sync.py include {name}")


def cmd_include(_frame, args):
    excluded = load_excluded()
    names = excluded if args.all else set(args.names)
    if not names:
        die("pass photo names, or --all")
    missing = names - excluded
    if missing:
        die(f"not in the excluded list: {', '.join(sorted(missing))}")
    save_excluded(excluded - names)
    say(f"Re-included {len(names)} photos; the next sync will upload them again.")


def cmd_excluded(_frame, _args):
    excluded = sorted(load_excluded())
    print("\n".join(excluded) if excluded else "No excluded photos.")


def cmd_clear(frame, args):
    frame.connect()
    n = len(frame.remote_files())
    if not n:
        print("Nothing to remove.")
        return
    if not args.yes and input(f"Remove all {n} synced photos from the frame? [y/N] ").strip().lower() != "y":
        return
    frame.shell(f"rm -f {REMOTE_DIR}/{PREFIX}*.jpg")
    frame.rescan(0)
    say(f"Removed {n} photos.")


def setup_logging():
    try:
        LOG_FILE.parent.mkdir(parents=True, exist_ok=True)
        handler = logging.FileHandler(LOG_FILE)
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s", "%Y-%m-%d %H:%M:%S"))
        log.addHandler(handler)
        log.setLevel(logging.INFO)
    except OSError:
        pass  # logging is a convenience; never block a sync on it


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--host", help=HOST_HELP)
    sub = p.add_subparsers(dest="command", required=True)

    s = sub.add_parser("sync", help="upload photos from folders and/or Google Photos shared-album links")
    s.add_argument("sources", nargs="+", help="local folder or shared-album URL")
    s.add_argument("--delete", action="store_true", help="also remove synced photos that are no longer in the sources")
    s.add_argument("--limit", type=int, metavar="N", help="upload at most N new photos in this run")
    s.add_argument("--dry-run", action="store_true", help="show what would change without touching the frame")
    s.add_argument("--refresh", action="store_true", help="close a running slideshow so it reloads the photo list")
    s.set_defaults(func=cmd_sync)

    sub.add_parser("status", help="show what is on the frame").set_defaults(func=cmd_status)

    e = sub.add_parser("exclude", help="remove a photo from the frame and never sync it again")
    e.add_argument("names", nargs="*", help="photo name on the frame; default: the photo on screen right now")
    e.set_defaults(func=cmd_exclude)

    i = sub.add_parser("include", help="undo an exclude so the next sync uploads the photo again")
    i.add_argument("names", nargs="*", help="excluded photo names")
    i.add_argument("--all", action="store_true", help="re-include every excluded photo")
    i.set_defaults(func=cmd_include)

    sub.add_parser("excluded", help="list excluded photos").set_defaults(func=cmd_excluded)

    c = sub.add_parser("clear", help="remove every photo this tool uploaded")
    c.add_argument("-y", "--yes", action="store_true", help="do not ask for confirmation")
    c.set_defaults(func=cmd_clear)

    args = p.parse_args()
    setup_logging()
    log.info("--- %s", " ".join(sys.argv[1:]))
    try:
        args.func(Frame(args.host or frame_host()), args)
    except KeyboardInterrupt:
        log.warning("interrupted")
        sys.exit("\ninterrupted; run the same command again to continue")


if __name__ == "__main__":
    main()
