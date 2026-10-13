#!/usr/bin/python3
"""Try the frame's own screen on this computer, without changing anything on the frame.

    tools/simulate.py

This builds frameserver for this computer and runs it against a folder here
(.sim), with a stand-in for the frame's Android (a made-up Wi-Fi network, and
screen changes that are only logged). What you see is the real server and pages:

    http://127.0.0.1:8090/simulator   the frame's screen, in a box of the frame's size
    http://127.0.0.1:8090/            the page for managing photos and the calendar

Like the frame, it answers to the whole local network, so a phone on the same Wi-Fi
can open the address (and QR code) that the Settings tab shows. --local keeps it to
this computer.

The first run copies a random sample of photos from the frame to try the slideshow
with. They are only read from the frame (plain GET requests); nothing is sent to it.
Calendars, events and settings you change belong to this copy and never reach the frame.

Run it with the system Python (the shebang): macOS blocks Homebrew's Python from the
local network.
"""

import argparse
import json
import os
import random
import subprocess
import sys
import urllib.parse
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
HERE = REPO / "server"  # the Rust crate
SIM = REPO / ".sim"


def copy_sample(frame, count):
    """Copy `count` random photos from the frame into the simulator's photo folder."""
    photos = SIM / "photos"
    if any(photos.rglob("*.*")):
        return
    print(f"Copying {count} photos from the frame at {frame} to try the slideshow with...")
    try:
        with urllib.request.urlopen(f"http://{frame}/api/state?full=1", timeout=30) as reply:
            listed = [(photo[0], photo[2]) for photo in json.load(reply)["photos"]]
    except OSError as error:
        print(f"  The frame did not answer ({error}). Put some photos in {photos / 'frame-sync'} yourself.")
        return
    for done, (rel, taken) in enumerate(random.sample(listed, min(count, len(listed))), 1):
        target = photos / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        try:
            with urllib.request.urlopen(f"http://{frame}/file?p={urllib.parse.quote(rel, safe='')}", timeout=60) as reply:
                target.write_bytes(reply.read())
            # The copy's date is all this server has to go on, so give it the date the photo was taken.
            os.utime(target, (taken, taken))
        except OSError as error:
            print(f"  skipped {rel}: {error}")
        print(f"\r  {done} copied", end="", flush=True)
    print()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--frame", default="192.168.4.23:8080", help="frameserver on the frame, to copy sample photos from")
    parser.add_argument("--photos", type=int, default=150, help="how many photos to copy on the first run")
    parser.add_argument("--port", type=int, default=8090, help="port to serve on")
    parser.add_argument("--local", action="store_true", help="answer only to this computer, not to phones on the network")
    args = parser.parse_args()

    (SIM / "photos" / "frame-sync").mkdir(parents=True, exist_ok=True)
    (SIM / "data").mkdir(parents=True, exist_ok=True)
    copy_sample(args.frame, args.photos)

    print("Building frameserver for this computer...")
    if subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=HERE).returncode != 0:
        sys.exit("The build failed.")
    env = dict(
        os.environ,
        FRAME_CLOUD_DIR=str(SIM / "photos"),
        FRAME_DATA_DIR=str(SIM / "data"),
        FRAME_PORT=str(args.port),
        FRAME_DEVICE="simulated",
        # The stand-in frame keeps its clock where this computer does.
        FRAME_TZ=os.environ.get("TZ") or os.path.realpath("/etc/localtime").split("zoneinfo/")[-1],
        FRAME_BIND="127.0.0.1" if args.local else "0.0.0.0",
    )
    print(f"\nThe frame's screen:        http://127.0.0.1:{args.port}/simulator")
    print(f"Photos and the calendar:   http://127.0.0.1:{args.port}/\n")
    try:
        subprocess.run([str(HERE / "target" / "release" / "frameserver")], env=env)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
