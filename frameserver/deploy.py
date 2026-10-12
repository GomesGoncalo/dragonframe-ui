#!/usr/bin/env python3
"""Build frameserver and install it on the frame (the only step that uses adb).

    ./deploy.py           # build, install, (re)start, and set it to start at boot
    ./deploy.py --remove  # stop it and remove it from the frame
    ./deploy.py --reset-pin  # forgot the PIN: remove it (photos and settings are kept)

Nothing is deployed unless formatting, clippy (warnings are errors) and the tests pass.

Needs: rustup target add armv7-unknown-linux-musleabihf, and cargo-zigbuild.
"""

import argparse
import hashlib
import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import frame_sync as fs  # noqa: E402

TARGET = "armv7-unknown-linux-musleabihf"
BINARY = HERE / "target" / TARGET / "release" / "frameserver"
INSTALL_DIR = "/data/local/frameserver"
# init reads its .rc files very early in boot. /vendor's overlay is mounted before that;
# /system's overlay lives on /cache, which is mounted too late for a file there to be seen.
RC_FILE = "/vendor/etc/init/frameserver.rc"
OLD_RC_FILE = "/system/etc/init/frameserver.rc"  # where earlier versions put it; never took effect
PORT = 8080
# Started by Android's init once the system is up, and restarted by it if it ever exits.
# init must launch the system shell rather than the binary itself: /data is mounted
# nosuid, and the kernel refuses to exec a program from there under a new SELinux label.
RC = f"""service frameserver /system/bin/sh -c "exec {INSTALL_DIR}/frameserver"
    class late_start
    user root
    group root sdcard_rw media_rw inet
    seclabel u:r:su:s0
    disabled

on property:sys.boot_completed=1
    start frameserver
"""


# Face-analysis models, run on the frame by the pure-Rust `tract` runtime. Pinned by SHA-256.
MODEL_DIR = HERE / "models"
MODELS = {
    # YuNet face detector, OpenCV Zoo, MIT licence.
    "yunet.onnx": (
        "https://media.githubusercontent.com/media/opencv/opencv_zoo/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
        "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
    ),
    # open-closed-eye-0001, Intel Open Model Zoo, Apache-2.0 licence.
    "eye.onnx": (
        "https://storage.openvinotoolkit.org/repositories/open_model_zoo/public/2022.1/open-closed-eye-0001/open_closed_eye.onnx",
        "4daa100034482525a26c9afb9297c16580a531189e66e3d2b2ac7d32becfd593",
    ),
    # FER+ emotion recognition, ONNX Model Zoo, MIT licence.
    "ferplus.onnx": (
        "https://media.githubusercontent.com/media/onnx/models/main/validated/vision/body_analysis/emotion_ferplus/model/emotion-ferplus-8.onnx",
        "a2a2ba6a335a3b29c21acb6272f962bd3d47f84952aaffa03b60986e04efa61c",
    ),
}


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch_models():
    """Download any model that is missing or does not match its pinned checksum."""
    MODEL_DIR.mkdir(exist_ok=True)
    for name, (url, expected) in MODELS.items():
        path = MODEL_DIR / name
        if path.exists() and sha256(path) == expected:
            continue
        print(f"==> downloading {name}")
        request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
        with urllib.request.urlopen(request, timeout=300) as response:
            path.write_bytes(response.read())
        if sha256(path) != expected:
            path.unlink()
            fs.die(f"{name} did not match its expected checksum; not installing it")


def install_models(frame):
    """Put the models on the frame, skipping ones that are already there with the right size."""
    remote_dir = f"{INSTALL_DIR}/models"
    frame.shell(f"mkdir -p {remote_dir}")
    for name in MODELS:
        local = MODEL_DIR / name
        size = frame.shell(f"stat -c %s {remote_dir}/{name} 2>/dev/null", check=False).strip()
        if size != str(local.stat().st_size):
            print(f"==> installing {name} on the frame")
            frame.adb("push", str(local), f"{remote_dir}/{name}")


CHECKS = [
    ["cargo", "fmt", "--check"],
    ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"],
    ["cargo", "test"],
]


def run_checks():
    for command in CHECKS:
        print("==>", " ".join(command))
        if subprocess.run(command, cwd=HERE).returncode != 0:
            fs.die(f"`{' '.join(command)}` failed; not deploying")


def reset_pin(frame):
    path = f"{INSTALL_DIR}/settings.json"
    try:
        settings = json.loads(frame.shell(f"cat {path}", check=False) or "{}")
    except ValueError:
        fs.die(f"could not read {path} on the frame")
    if "pin" not in settings:
        print("No PIN is set.")
        return
    settings.pop("pin", None)
    settings.pop("sessions", None)
    local = HERE / "target" / "settings.json"
    local.write_text(json.dumps(settings))
    frame.adb("push", str(local), path)
    local.unlink()
    print("PIN removed. Set a new one from the Status tab.")


def stop(frame):
    frame.shell("stop frameserver 2>/dev/null; pkill -x frameserver; true", check=False)


def remove_file(frame, path):
    """Delete a file from one of the read-only system overlays."""
    mount = "/" + path.split("/")[1]
    frame.shell(f"[ -e {path} ] && mount -o rw,remount {mount} && rm -f {path}; mount -o ro,remount {mount}", check=False)


def write_file(frame, path, content):
    """Write a file into a read-only system overlay, opening it just long enough to do so."""
    mount = "/" + path.split("/")[1]
    local = HERE / "target" / "frameserver.rc"
    local.write_text(content)
    frame.adb("push", str(local), f"{INSTALL_DIR}/frameserver.rc")
    frame.shell(f"mount -o rw,remount {mount} && cp {INSTALL_DIR}/frameserver.rc {path} && chmod 644 {path}; mount -o ro,remount {mount}")
    if frame.shell(f"cat {path}", check=False) != content:
        fs.die(f"could not install {path}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--host", default=fs.DEFAULT_HOST, help="frame address")
    p.add_argument("--remove", action="store_true", help="stop the server and remove it from the frame")
    p.add_argument("--no-boot", action="store_true", help="do not set it to start when the frame boots")
    p.add_argument("--reset-pin", action="store_true", help="remove a forgotten PIN")
    p.add_argument("--skip-checks", action="store_true", help="deploy without running fmt, clippy and tests")
    args = p.parse_args()
    frame = fs.Frame(args.host)

    if args.remove:
        frame.connect()
        stop(frame)
        remove_file(frame, RC_FILE)
        remove_file(frame, OLD_RC_FILE)
        frame.shell(f"rm -rf {INSTALL_DIR}")
        print("Removed frameserver from the frame. Photos were not touched.")
        return

    if args.reset_pin:
        frame.connect()
        reset_pin(frame)
        return

    fetch_models()
    if not args.skip_checks:
        run_checks()
    subprocess.run(["cargo", "zigbuild", "--release", "--target", TARGET], cwd=HERE, check=True)
    frame.connect()
    stop(frame)
    frame.shell(f"mkdir -p {INSTALL_DIR}")
    frame.adb("push", str(BINARY), f"{INSTALL_DIR}/frameserver.new")
    frame.shell(f"cd {INSTALL_DIR} && mv frameserver.new frameserver && chmod 755 frameserver")
    install_models(frame)
    if not args.no_boot:
        write_file(frame, RC_FILE, RC)
        remove_file(frame, OLD_RC_FILE)
    # Start it now, in the background and in its own session so it outlives this adb
    # connection. adb still does not return while it runs, so do not wait for it: drop the
    # adb client once the server answers. After a reboot, init starts it instead.
    starter = subprocess.Popen(
        ["adb", "-s", frame.host, "shell", f"cd {INSTALL_DIR} && setsid ./frameserver >/dev/null 2>&1 < /dev/null &"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    url = f"http://{frame.target[0]}:{PORT}/"
    for _ in range(20):
        time.sleep(0.5)
        try:
            urllib.request.urlopen(url + "api/state", timeout=3).read()
            starter.terminate()
            print(f"frameserver is running. Open {url}")
            return
        except OSError:
            pass
    starter.terminate()
    fs.die(f"installed, but {url} is not answering; see {INSTALL_DIR}/logs/ on the frame")


if __name__ == "__main__":
    main()
