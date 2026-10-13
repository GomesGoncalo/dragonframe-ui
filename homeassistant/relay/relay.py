#!/usr/bin/env python3
"""Tell the photo frame what the house is doing.

This runs beside Home Assistant. It listens to Home Assistant for changes and tells
the frame what to show: the alarm's state, which doors are open, where there is
movement, whether the doorbell is ringing, and each camera's latest picture.

The frame is never given Home Assistant's token. It gets only what is sent here,
and this proves itself to the frame with a key the frame made (FRAME_KEY), which
opens nothing but that.

Settings, from the environment:
    HA_URL      where Home Assistant is (default http://homeassistant:8123)
    HA_TOKEN    a long-lived access token from Home Assistant
    FRAME_URL   where the frame's server is, such as http://192.168.4.23:8080
    FRAME_KEY   the key made on the frame's page (Frame tab, "Connect the house")

Only the standard library is used, so it runs in a stock Python image.
"""

import asyncio
import base64
import json
import os
import struct
import sys
import urllib.error
import urllib.parse
import urllib.request

REPORT_EVERY = 30  # seconds: the frame takes silence longer than a minute and a half for a lost house
SETTLE = 0.4       # seconds to let a burst of changes finish before telling the frame
MOST = {"alarms": 4, "sensors": 40, "cameras": 8}  # what the frame accepts
OPENINGS = ("door", "window", "opening", "garage_door")


def log(*words):
    print(*words, flush=True)


# --- what to tell the frame ---------------------------------------------------------


def named(state, *endings):
    """An entity's name for the screen, without the words Home Assistant adds to it."""
    name = state.get("attributes", {}).get("friendly_name") or state["entity_id"]
    for ending in endings:
        if name.lower().endswith(ending.lower()):
            name = name[: -len(ending)]
    return name.strip()[:60] or state["entity_id"][:60]


def report(states):
    """What the house is doing, from Home Assistant's states (entity id -> state)."""
    known = {entity: s for entity, s in states.items() if s.get("state") not in (None, "unavailable")}
    is_on = lambda entity: known.get(entity, {}).get("state") == "on"  # noqa: E731

    cameras, with_camera = [], set()
    for entity, state in sorted(known.items()):
        if entity.startswith("image.") and entity.endswith("_event_image"):
            camera = entity[len("image.") : -len("_event_image")]
            with_camera.add(camera)
            cameras.append({
                "id": camera,
                "name": named(state, " Event Image"),
                "person": is_on(f"binary_sensor.{camera}_person_detected"),
                "bell": f"binary_sensor.{camera}_ringing" in known,
            })

    alarms, sensors = [], []
    for entity, state in sorted(known.items()):
        kind = state.get("attributes", {}).get("device_class")
        if entity.startswith("alarm_control_panel.") and state["state"] != "unknown":
            alarms.append({"name": named(state), "state": state["state"]})
        elif not entity.startswith("binary_sensor.") or state["state"] not in ("on", "off"):
            continue
        elif kind in OPENINGS:
            name = named(state, " Sensor open", " open")
            window = kind == "window" or "window" in name.lower()
            sensors.append({"name": name, "kind": "window" if window else "door", "on": state["state"] == "on"})
        elif kind == "motion" and entity.endswith("_motion_detected"):
            # A camera's own motion is shown with the camera, not as a sensor of its own.
            if entity[len("binary_sensor.") : -len("_motion_detected")] not in with_camera:
                sensors.append({"name": named(state, " Motion detected"), "kind": "motion", "on": state["state"] == "on"})

    return {
        "alarms": alarms[: MOST["alarms"]],
        "sensors": sensors[: MOST["sensors"]],
        "cameras": cameras[: MOST["cameras"]],
        "ringing": any(e.startswith("binary_sensor.") and e.endswith("_ringing") and is_on(e) for e in known),
    }


def matters(entity):
    """Whether a change to this entity can change what the frame is told."""
    return entity.startswith(("alarm_control_panel.", "binary_sensor.", "image."))


# --- talking to the frame and to Home Assistant over http -------------------------------


def call(url, headers, body=None):
    """One http request; returns (status, body). Never raises for a refusal or a lost connection."""
    request = urllib.request.Request(url, data=body, headers=headers, method="POST" if body is not None else "GET")
    try:
        with urllib.request.urlopen(request, timeout=15) as reply:
            return reply.status, reply.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()
    except OSError as error:
        return 0, str(error).encode()


class Frame:
    def __init__(self, url, key):
        self.url, self.key = url.rstrip("/"), key
        self.told = None  # the last report the frame took

    def tell(self, what):
        """Report the house. Returns the cameras the frame wants a picture of, or None if it did not take the report."""
        status, body = call(f"{self.url}/api/home/report", {"X-Home-Key": self.key, "Content-Type": "application/json"}, json.dumps(what).encode())
        if status != 200:
            log(f"the frame did not take the report ({status or 'not reachable'}): {body[:200].decode(errors='replace')}")
            self.told = None
            return None
        if what != self.told:
            open_or_moving = [s["name"] for s in what["sensors"] if s["on"]]
            log("told the frame:", ", ".join(f"{a['name']} {a['state']}" for a in what["alarms"]) or "no alarm",
                "| open or moving:", ", ".join(open_or_moving) or "nothing", "| ringing" if what["ringing"] else "")
        self.told = what
        try:
            return list(json.loads(body).get("wanted") or [])
        except ValueError:
            return []

    def show(self, camera, picture):
        status, body = call(f"{self.url}/api/home/picture?camera={urllib.parse.quote(camera)}", {"X-Home-Key": self.key, "Content-Type": "application/octet-stream"}, picture)
        if status != 200:
            log(f"the frame did not take {camera}'s picture ({status or 'not reachable'}): {body[:200].decode(errors='replace')}")


# --- a small WebSocket client (text messages only), which is all Home Assistant needs ----


class Closed(Exception):
    pass


class WebSocket:
    def __init__(self, reader, writer):
        self.reader, self.writer = reader, writer

    @classmethod
    async def connect(cls, url):
        parts = urllib.parse.urlsplit(url)
        port = parts.port or (443 if parts.scheme in ("https", "wss") else 80)
        reader, writer = await asyncio.open_connection(parts.hostname, port, ssl=parts.scheme in ("https", "wss"), limit=2**16)
        key = base64.b64encode(os.urandom(16)).decode()
        writer.write((
            f"GET {parts.path or '/'} HTTP/1.1\r\nHost: {parts.hostname}:{port}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ).encode())
        await writer.drain()
        answer = (await reader.readuntil(b"\r\n\r\n")).split(b"\r\n", 1)[0]
        if b" 101 " not in answer:
            raise Closed("not a WebSocket: " + answer.decode(errors="replace"))
        return cls(reader, writer)

    async def _frame(self, opcode, payload):
        mask = os.urandom(4)
        size = len(payload)
        if size < 126:
            head = struct.pack("!BB", 0x80 | opcode, 0x80 | size)
        elif size < 2**16:
            head = struct.pack("!BBH", 0x80 | opcode, 0x80 | 126, size)
        else:
            head = struct.pack("!BBQ", 0x80 | opcode, 0x80 | 127, size)
        self.writer.write(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))
        await self.writer.drain()

    async def send(self, message):
        await self._frame(0x1, json.dumps(message).encode())

    async def receive(self):
        """The next message, as parsed JSON. Pings are answered on the way."""
        whole = b""
        while True:
            try:
                first, second = await self.reader.readexactly(2)
                size = second & 0x7F
                if size == 126:
                    (size,) = struct.unpack("!H", await self.reader.readexactly(2))
                elif size == 127:
                    (size,) = struct.unpack("!Q", await self.reader.readexactly(8))
                mask = await self.reader.readexactly(4) if second & 0x80 else None
                payload = await self.reader.readexactly(size)
            except (asyncio.IncompleteReadError, ConnectionError) as error:
                raise Closed(str(error)) from error
            if mask:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            opcode = first & 0x0F
            if opcode == 0x8:
                raise Closed("the other side closed the connection")
            if opcode == 0x9:
                await self._frame(0xA, payload)
            elif opcode in (0x0, 0x1, 0x2):
                whole += payload
                if first & 0x80:
                    return json.loads(whole)

    def close(self):
        self.writer.close()


# --- the relay ----------------------------------------------------------------------------


async def relay(ha_url, token, frame):
    """Follow Home Assistant until the connection is lost."""
    socket = await WebSocket.connect(ha_url.replace("http", "ws", 1).rstrip("/") + "/api/websocket")
    try:
        await socket.receive()  # "auth_required"
        await socket.send({"type": "auth", "access_token": token})
        if (await socket.receive()).get("type") != "auth_ok":
            raise SystemExit("Home Assistant refused the token in HA_TOKEN.")
        await socket.send({"id": 1, "type": "get_states"})
        await socket.send({"id": 2, "type": "subscribe_events", "event_type": "state_changed"})
        log("following Home Assistant")

        states, shown = {}, {}  # entity -> state; camera -> the picture state already sent
        headers = {"Authorization": f"Bearer {token}"}

        async def tell():
            what = report(states)
            wanted = await asyncio.to_thread(frame.tell, what)
            if wanted is None:
                return
            for camera in what["cameras"]:
                entity = f"image.{camera['id']}_event_image"
                stamp = states[entity]["state"]
                # Sent already, unless the frame says it has lost it (it forgets them when it restarts).
                if shown.get(camera["id"]) == stamp and camera["id"] not in wanted:
                    continue
                status, picture = await asyncio.to_thread(call, f"{ha_url.rstrip('/')}/api/image_proxy/{entity}", headers)
                if status == 200 and picture:
                    await asyncio.to_thread(frame.show, camera["id"], picture)
                    shown[camera["id"]] = stamp

        changed = False
        while True:
            try:
                # After a change, wait only a moment for more of them; otherwise until it is time to report anyway.
                message = await asyncio.wait_for(socket.receive(), SETTLE if changed else REPORT_EVERY)
            except asyncio.TimeoutError:
                await tell()
                changed = False
                continue
            if message.get("id") == 1 and message.get("type") == "result":
                states = {state["entity_id"]: state for state in message.get("result") or []}
                changed = True
            elif message.get("type") == "event":
                data = message["event"]["data"]
                entity, new = data["entity_id"], data.get("new_state")
                if new is None:
                    states.pop(entity, None)
                else:
                    states[entity] = new
                changed = changed or matters(entity)
    finally:
        socket.close()


async def main():
    try:
        token, frame = os.environ["HA_TOKEN"], Frame(os.environ["FRAME_URL"], os.environ["FRAME_KEY"])
    except KeyError as missing:
        sys.exit(f"{missing.args[0]} is not set; see relay.env")
    if not token or not frame.key:
        sys.exit("HA_TOKEN and FRAME_KEY must be filled in; see relay.env")
    ha_url = os.environ.get("HA_URL", "http://homeassistant:8123")
    while True:
        try:
            await relay(ha_url, token, frame)
        except (Closed, OSError, asyncio.TimeoutError) as error:
            log(f"lost Home Assistant ({error}); trying again in 10 seconds")
        await asyncio.sleep(10)


if __name__ == "__main__":
    asyncio.run(main())
