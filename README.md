# Dragon Touch frame

Software for a Dragon Touch calendar and photo frame (model TM15, Android 10): a server
that runs on the frame itself, a new screen for the frame, and a page for managing it
from a phone or computer.

The frame's original software is never removed or changed. Everything here sits beside
it and can be switched off or removed again (see [Going back](#going-back)).

## What is here

```
server/                  the server that runs on the frame (Rust)
  src/                   Rust only; see "How it is put together"
  pages/                 what the server serves, as plain html, css and js
    screen/              the frame's own screen: pictures, calendar, settings
    manage/              the page for a phone or computer
    simulator.html       the screen in a frame-sized box
android/                 the small app that shows the screen full screen on the frame
tools/
  deploy.py              builds everything and installs it on the frame; also reverts
  simulate.py            runs it all on this computer, without touching the frame
  frame_sync.py          reaching the frame over adb (used by deploy.py), and the
  frame_app.py           earlier tools that managed photos from a computer
```

Not in git: `.tools/` (downloaded build tools), `.sim/` (the simulator's photos and
data), `android/build/`, `server/target/` and `server/models/`.

## The frame's screen

Four tabs, each with an icon. On Pictures the tab bar is hidden until you tap the middle
of the screen; on the other two it stays visible. The frame can stand either way up: the
screen turns with it and lays itself out for a wide or a tall display. Calendar and
Settings go back to Pictures when left alone (5 minutes unless set otherwise).

- **Pictures.** A slideshow with a clock and the weather. No photo is ever cropped. A
  photo the shape of the screen is shown alone. Others are laid out with photos from the
  next few in line, side by side and one above the other, in whichever arrangement
  leaves the least of the screen empty. Each photo carries the date it was taken in its
  corner, for the first five seconds of a screen and whenever the tab bar is up. Photos
  taken on today's date in earlier years come first, marked "On this day" with the year.
  In the morning (06:00 to 10:00) today's events are listed under the clock. Tap the
  left or right quarter to step back or forward. The tab bar has a Pause button.
- **Calendar.** A Month view (dots on days with events, and "Coming up" grouped by day
  with each event's time and calendar) and a Week view (seven days with their events
  and the forecast). Each subscribed calendar has its own colour; events added by hand
  are grey. The arrows, or a swipe, move to other months and weeks; "Today" comes back.
  A touch on a day lists what is on it, past or future (from the week view this opens
  the month with that day picked).
- **Utilities.** Timers (up to four at once, from preset lengths or one set by hand),
  alarms (once, every day, weekdays or weekends, with a five-minute snooze) and a
  pomodoro clock (focus and break lengths can be chosen; a long break after four
  rounds). When a timer or alarm goes off, the frame chimes repeatedly and shows a
  notice over everything until it is stopped. A running timer or pomodoro shows in the
  corner of Pictures, keeps the screen on through night mode, and keeps its tab open.
  An alarm switches the screen on if night mode has it off. These are kept on the frame
  itself and need no PIN.
- **Settings**, in five parts. If a PIN is set, a keypad asks for it each time Settings
  is opened, and nothing is shown until it is right:
  - Pictures: how often they change, the order, how long before the screen goes back to
    Pictures, and whether to show memories first, the weather, and the morning list.
  - Night: switch the screen off between two times, and optionally whenever the room
    has been dark for two minutes. See [Night mode](#night-mode).
  - Sound: a chime for calendar events, at the time or 5 to 30 minutes before, with a
    button to hear it.
  - Wi-Fi: the network and signal, a "Choose a network" button, and the address of the
    management page with a QR code. Android 10 offers no way for the server to join a
    network, so the button opens Android's own list of networks. The frame has no back
    button, so the server brings the screen back one minute after the last touch (after
    15 minutes at the latest).
  - Frame: the place the weather is for, and the way back to the original screen (two
    taps).

When an event with a time is about to start, a note appears at the top of whichever tab
is showing, with the chime if one is set.

### Night mode

The server switches the screen off and on with Android's sleep and wake keys.

- It is off between the two times, and, if asked, whenever the room has been dark
  (under 4 lux) for two minutes; it counts as lit again above 12 lux.
- A frame touched in the last five minutes is left on.
- Only a screen that night mode switched off is switched back on by it.
- The frame's light sensor stops measuring when the screen is off, so the screen app
  keeps listening to it; that is how a dark room being lit again is noticed.
- The server holds a wake lock so the frame's processor keeps running with the screen
  off; otherwise nothing could switch it back on.
- To use the frame while it is off, press its power button.

## The management page

Open `http://<frame address>:8080` from a phone or computer on the same Wi-Fi.

- **Photos, Duplicates, Google Photos sync, Bin**: see, add and delete photos, find
  look-alikes, and sync shared Google Photos albums.
- **Calendar**:
  - Calendars the frame follows: one link per line. For a public Google calendar its
    ordinary sharing or embedding link will do (it is turned into the calendar's public
    feed); for a private one use "Secret address in iCal format" from the calendar's
    settings. Any address ending in `.ics` or starting with `webcal://` works; in Apple
    Calendar, share the calendar publicly and copy the link. The frame fetches each one
    every 15 minutes and keeps the last copy, so the calendar survives restarts and
    network drops. Calendars are only read.
  - Some calendar servers only offer an old style of encryption that the server's usual
    TLS library refuses. Those are fetched with a second, more tolerant client (OpenSSL)
    that is only tried when the usual one cannot connect.
  - Add an event: title, date, optional time, and once, weekly, monthly or yearly.
    These are stored on the frame.
- **Frame**: everything the frame's own Settings tab has, plus what needs a keyboard:
  which screen the frame shows, night mode, the chime, the place for the weather (type
  a town; the forecast comes from Open-Meteo, which needs no account), and the
  slideshow options.
- **Status**: storage, sync and the PIN.

A PIN (Status tab) protects changes made over the network. Viewing needs no PIN.
Calendar links are treated as secrets: they are never written to the log and are only
shown to a browser that has entered the PIN. The frame's own Settings tab asks for the
PIN too, and gives it up again when Settings is left, so the next person is asked again.

## Trying it on this computer

```sh
tools/simulate.py
```

- `http://127.0.0.1:8090/simulator` is the frame's screen in a box of exactly the
  frame's size (1920x1080).
- `http://127.0.0.1:8090/` is the management page.

The first run copies 150 random photos from the frame into `.sim/` (read
only; nothing is sent to the frame). Wi-Fi is a made-up network and screen switches are
only logged. Calendars, events and settings changed here stay on this computer.

The simulator answers to the whole local network, so a phone can open the address and
QR code shown in Settings. `tools/simulate.py --local` keeps it to this computer.

Useful for looking at one thing: `/frame?tab=calendar&view=week`,
`/frame?tab=settings&part=night`, `/frame?every=5`, `/frame?order=newest`, and
`/simulator?portrait` for the frame standing tall. Keys in the simulator: arrows step,
space pauses, `1` `2` `3` choose a tab. When night mode switches the simulated screen
off, the page goes black and says so.

## Installing on the frame

```sh
tools/deploy.py
```

This runs the checks (`cargo fmt --check`, `cargo clippy` with warnings as errors,
`cargo test`), builds the server for the frame, builds the screen app, installs both,
sets the server to start when the frame boots, and puts the new screen on the frame.
Nothing is deployed if a check fails.

| Command | What it does |
| --- | --- |
| `tools/deploy.py` | Build, install, and show the new screen |
| `tools/deploy.py --keep-screen` | Build and install, but leave whichever screen the frame shows |
| `tools/deploy.py --new-ui` | Show the new screen (no build) |
| `tools/deploy.py --original-ui` | Go back to the screen the frame came with (no build) |
| `tools/deploy.py --previous` | Put back the server that the last deploy replaced |
| `tools/deploy.py --reset-pin` | Remove a forgotten PIN |
| `tools/deploy.py --remove` | Remove the server and the app from the frame |

### What it needs

- `adb`, Rust with `rustup target add armv7-unknown-linux-musleabihf`, and
  `cargo-zigbuild`.
- For the screen app: a Java kit and Google's Android SDK tools. `android/build.py`
  downloads these into `.tools/` on first use (about 800 MB) and nothing is
  installed system-wide. Fetching the SDK parts accepts Google's Android SDK licence.
  Deleting `.tools` removes all of it.
- The app is signed with a key created on first build and kept in
  `.tools/frame-screen.keystore`. If that key is lost, uninstall the app from the frame
  (`adb uninstall frame.screen`) before installing one signed with a new key.
- `deploy.py` and `simulate.py` run with the system Python (`/usr/bin/python3`): macOS
  blocks Homebrew's Python and `adb` from the local network. `frame_sync.py` relays adb
  through Python when adb cannot reach the frame directly.

## Going back

The frame's own software is its home screen throughout. The new screen is an ordinary
app shown on top of it.

1. **From the frame**: Settings, "Use the original screen".
2. **From a phone or computer**: management page, Frame, "Show the original screen".
3. **By itself**: the new screen tells the server it is running every 30 seconds. If
   that stops for 3 minutes, the server puts the original screen back and says so on
   the Status tab. The choice of screen is kept, so a restart tries the new one again.
4. **From this computer**: `tools/deploy.py --original-ui`. This works through adb alone
   and does not depend on the server or the page.
5. **Completely**: `tools/deploy.py --remove` removes the server and the app. Photos are
   not touched.

To return to the new screen: Frame, "Show the new screen", or `tools/deploy.py --new-ui`.

## How it is put together

The server is async Rust (tokio and axum). Work that blocks, such as decoding images
and running Android commands, lives in plain functions run off the async threads.
Requests are parsed into types where they arrive (`types.rs`), so a handler never holds
a value it would still have to check.

| In `server/src/` | What it does |
| --- | --- |
| `main.rs` | Start-up: settings from the environment, logging, listening |
| `app.rs` | The state every part shares |
| `routes/mod.rs` | The table of routes, the PIN guard, how refusals are answered |
| `routes/photos.rs` | Listing, serving, adding and deleting photos; the bin; look-alikes; album syncs |
| `routes/calendars.rs` | What is on the calendar, subscribed calendars, added events |
| `routes/frame.rs` | The frame's Wi-Fi, screen, slideshow, night, chime and weather |
| `routes/access.rs` | Unlocking with the PIN, locking, changing it |
| `routes/pages.rs` | Handing out the pages in `pages/` |
| `tasks.rs` | Background work: daily syncs, face checks, the bin, night mode, watching the screen |
| `store.rs` | Photos on disk, thumbnails, settings, Android commands for photos |
| `index.rs` | What is known about each photo: date, size, fingerprint, quality |
| `gphotos.rs` | Syncing Google Photos shared albums |
| `faces.rs` | Face checks used to rank look-alike photos |
| `ical.rs` | Reading iCalendar text and working out when events happen, repeats and time zones included |
| `calendar.rs` | Subscribed calendars (fetching, keeping copies) and events added by hand |
| `device.rs` | The frame itself: Wi-Fi status, which screen is in front, room light, screen on and off. Has a stand-in for the simulator |
| `night.rs` | Deciding when the screen should be off |
| `weather.rs` | Finding a place by name and fetching its forecast from Open-Meteo |
| `plain.rs` | Listening for plain http, and resetting https attempts |
| `auth.rs`, `dns.rs` | The PIN and sessions; name lookups that work in a static binary on Android |
| `types.rs` | Values checked once, where they enter |
| `tests/` | Tests of the routes, one file per area; each module also has its own tests |

Each page in `server/pages/` is an ordinary `index.html` with its stylesheets and scripts
beside it (the screen's Utilities tab has its own pair, `utilities.css` and
`utilities.js`). The server sends a page as one document with those put inside it, so a
page arrives in one request and is never half old and half new. Changing a page means
editing those files and rebuilding.

To add something: a new setting goes in `types.rs` (its type), `store.rs` (kept in
`settings.json`), a handler in the matching `routes/` file and a line in the route
table, then the two pages. A new background job is a function in `tasks.rs` and a line
in `tasks::start`.

Checks, run from `server/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings`, `cargo test`. `tools/deploy.py` refuses to deploy unless all three pass.

Routes added for the screen and calendar:

| Route | What it does |
| --- | --- |
| `GET /frame` | The frame's screen |
| `GET /simulator` | The screen in a frame-sized box |
| `GET /api/slides` | Photos as `[path, taken, width, height]` |
| `GET /api/calendar?from=&to=` | Events in a window (seconds since 1970), feed status, added events |
| `POST /api/calendars` | Set the subscribed calendar links |
| `POST /api/events/add`, `/api/events/delete` | Events added by hand |
| `GET /api/device` | Wi-Fi, chosen screen, slideshow, night and chime settings, room light, management address |
| `GET /api/weather` | The weather now and day by day, for the chosen place |
| `POST /api/weather/place`, `/api/weather/forget` | Choose the place by name, or stop showing the weather |
| `POST /api/night`, `/api/chime` | Night mode and the event chime |
| `GET /api/address.svg` | QR code of the management address |
| `GET /api/alive` | The screen saying it is running; `?busy=<seconds>` keeps the display on, `?wake=<seconds since 1970>` has it on then |
| `POST /api/screen`, `/api/slideshow`, `/api/wifi/settings` | Choose the screen, set the slideshow, open the Wi-Fi list |

On the frame, data lives in `/data/local/frameserver`: `settings.json`, `events.json`,
`calendars/` (kept copies of subscribed calendars), `index.json`, `thumbs/`, `logs/`.

Environment variables: `FRAME_CLOUD_DIR` (photo folder), `FRAME_DATA_DIR`,
`FRAME_PORT`, `FRAME_BIND` (listen address), `FRAME_DEVICE=simulated`, `FRAME_TZ` (the
simulated frame's time zone), `FRAME_LOG` (log level).

## Things worth knowing about the frame

- The WebView is Chromium 74. The screen page avoids newer features: optional chaining,
  `??`, flex `gap`, `inset`, `aspect-ratio`, `clamp()`, `backdrop-filter`.
- The display is 1920x1080 at Android density 180, which is 1706.67x960 CSS pixels.
- There is no on-screen back or home button, and no shell command to join a Wi-Fi
  network.
- It has a light sensor, an accelerometer, a speaker, a microphone, an infrared
  receiver and Bluetooth; no camera and no battery.
- Its language is set to US English, so the screen page picks the date format from the
  time zone instead (day first outside the Americas).
- iOS Safari tries https first on links opened from the Camera, and does not always
  fall back to http on a home address; typing `http://` in front of the address works.
- adb is open on port 5555 with root and no authentication. Anyone on the Wi-Fi can use
  it; the PIN does not cover that.
- Sending the BACK key while the original home screen is in front makes it restart in a
  loop. Recover with `am force-stop com.fujia.calendar`, then
  `am start -a android.intent.action.MAIN -c android.intent.category.HOME`.
- The built-in browser (`org.chromium.webview_shell`) can show the screen page too, but
  with a title strip and address bar. The server falls back to it only when the app is
  not installed.

## Not yet tried on the frame

These work in the tests and the simulator but have not been exercised on the device:
opening the Wi-Fi list and coming back from it, switching to the original screen from
the Settings tab, the automatic fallback, the new screen coming up by itself after a
reboot, a whole night of night mode (including waking when a dark room is lit), the
chime through the frame's speaker, and an alarm ringing while the screen is off.
