//! What the frame itself can tell us and be asked to do, beyond holding photos:
//! how its Wi-Fi is doing, and which screen is in front.
//!
//! The frame runs Android 10 and has no on-screen back or home button, so
//! whatever this module puts in front it must also be able to take away again.
//! On a computer there is no Android to ask; [`Device::Simulated`] stands in so
//! the frame's screen can be tried there.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::info;

use crate::store::run;
use crate::types::Screen;

/// The app that shows the new screen edge to edge (`app/` in this project).
const APP: &str = "frame.screen";
const APP_ACTIVITY: &str = "frame.screen/.MainActivity";
/// The browser that comes with the frame, used when the app is not installed. It shows
/// the same page, under its own title and address bars.
const BROWSER: &str = "org.chromium.webview_shell";
const BROWSER_ACTIVITY: &str = "org.chromium.webview_shell/.WebViewBrowserActivity";
/// The app the frame came with. It stays running behind the new screen and now and then
/// puts its own slideshow in front by itself.
const OWN_APP: &str = "com.fujia.calendar";
/// The new screen must be heard from this often, or the frame goes back to its own.
pub const QUIET_LIMIT: Duration = Duration::from_mins(3);

#[derive(Clone, Copy, Debug)]
pub enum Device {
    Android,
    /// A stand-in on a computer: reports a made-up network and only logs what it is asked to do.
    Simulated(&'static StandIn),
}

/// What the stand-in frame remembers: whether its screen is on, and which screen is in front.
/// Each stand-in has its own, so tests running side by side do not see each other's.
#[derive(Debug)]
pub struct StandIn {
    screen_on: AtomicBool,
    /// Whether the frame's own app has put itself in front.
    own_in_front: AtomicBool,
}

#[cfg(test)]
impl StandIn {
    /// Have the frame's own app put itself in front, as the real one does now and then.
    pub fn own_app_comes_forward(&self) {
        self.own_in_front.store(true, Ordering::Relaxed);
    }
}

/// How bright the room is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    /// In lux: a lit room is in the hundreds, a dark one near zero.
    pub lux: f32,
    /// The sensor's own clock when it measured, in seconds. A reading with the same time as
    /// the one before is the same reading, not news.
    pub at: u64,
}

/// How the frame's Wi-Fi is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Wifi {
    pub on: bool,
    /// The network it is connected to, if any.
    pub network: Option<String>,
    /// Signal strength in dBm (about -50 is strong, -80 is weak).
    pub signal: Option<i32>,
    /// The frame's address on that network.
    pub address: Option<String>,
}

impl Device {
    /// `FRAME_DEVICE=simulated` asks for the stand-in.
    pub fn from_env() -> Device {
        match std::env::var("FRAME_DEVICE").as_deref() {
            Ok("simulated") => Device::simulated(),
            _ => Device::Android,
        }
    }

    /// A new stand-in frame, with its screen on and the new screen in front. It lives as long
    /// as the program: a server has one, and a test one each.
    pub fn simulated() -> Device {
        let stand_in = StandIn { screen_on: AtomicBool::new(true), own_in_front: AtomicBool::new(false) };
        Device::Simulated(Box::leak(Box::new(stand_in)))
    }

    pub fn is_simulated(self) -> bool {
        matches!(self, Device::Simulated(_))
    }

    /// Whether the frame has finished starting up, so that a screen can be put in front. Blocking.
    pub fn started(self) -> bool {
        match self {
            Device::Android => run("getprop", &["sys.boot_completed"]).is_some_and(|out| out.trim() == "1"),
            Device::Simulated(_) => true,
        }
    }

    /// How the Wi-Fi is doing, or None if the frame will not say. Blocking.
    pub fn wifi(self) -> Option<Wifi> {
        match self {
            Device::Android => {
                let state = run("dumpsys", &["wifi"])?;
                let addresses = run("ip", &["-4", "-o", "addr", "show", "wlan0"]).unwrap_or_default();
                Some(parse_wifi(&state, &addresses))
            }
            Device::Simulated(_) => Some(Wifi {
                on: true,
                network: Some("Simulated network".into()),
                signal: Some(-52),
                address: Some(own_address().unwrap_or_else(|| "127.0.0.1".into())),
            }),
        }
    }

    /// Put Android's own Wi-Fi settings in front, the only place a network can be chosen. Blocking.
    pub fn open_wifi_settings(self) -> bool {
        match self {
            Device::Android => run("am", &["start", "-a", "android.settings.WIFI_SETTINGS"]).is_some(),
            Device::Simulated(_) => {
                info!("simulated: Android's Wi-Fi settings would open now");
                true
            }
        }
    }

    /// How long ago the frame was last touched. Blocking.
    pub fn untouched_for(self) -> Option<Duration> {
        self.power().1
    }

    /// How bright the room is, or None if the frame will not say. The frame's light sensor
    /// only measures while something is listening to it: Android does while the screen is on,
    /// and the screen app does all the time. Blocking.
    pub fn room_light(self) -> Option<Light> {
        match self {
            Device::Android => run("dumpsys", &["sensorservice"]).as_deref().and_then(parse_light),
            Device::Simulated(_) => {
                let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
                Some(Light { lux: 110.0, at })
            }
        }
    }

    /// Whether the screen is on, and how long ago the frame was touched, from one look at
    /// the frame's power state. Either is None if the frame will not say. Blocking.
    pub fn power(self) -> (Option<bool>, Option<Duration>) {
        match self {
            Device::Android => {
                let Some(power) = run("dumpsys", &["power"]) else { return (None, None) };
                let state = power.lines().find_map(|line| line.trim().strip_prefix("mWakefulness="));
                (state.map(|state| state == "Awake"), parse_untouched(&power))
            }
            // Nobody is touching a frame that is not there.
            Device::Simulated(stand_in) => (Some(stand_in.screen_on.load(Ordering::Relaxed)), Some(Duration::MAX)),
        }
    }

    /// Switch the screen off (it stays the same screen, only dark) or on again. Blocking.
    pub fn switch_screen(self, on: bool) -> bool {
        match self {
            // The "sleep" and "wake up" keys, which unlike the power key say which way to go.
            Device::Android => run("input", &["keyevent", if on { "224" } else { "223" }]).is_some(),
            Device::Simulated(stand_in) => {
                info!(on, "simulated: the screen would be switched now");
                stand_in.screen_on.store(on, Ordering::Relaxed);
                true
            }
        }
    }

    /// The time zone the frame keeps its clock in, such as `Europe/London`. Blocking.
    pub fn time_zone(self) -> Option<String> {
        let zone = match self {
            Device::Android => run("getprop", &["persist.sys.timezone"])?,
            Device::Simulated(_) => std::env::var("FRAME_TZ").ok()?,
        };
        Some(zone.trim().to_string()).filter(|zone| !zone.is_empty())
    }

    /// Keep the frame's processor running while the screen is off, so this server can
    /// switch the screen back on in the morning. Blocking.
    pub fn stay_running(self) {
        if matches!(self, Device::Android) {
            if let Err(e) = std::fs::write("/sys/power/wake_lock", "frameserver") {
                tracing::warn!(error = %e, "could not ask the frame to keep running with its screen off");
            }
        }
    }

    /// Whether the app the frame came with has put itself in front. Blocking.
    pub fn own_screen_in_front(self) -> bool {
        match self {
            Device::Android => run("dumpsys", &["window"]).as_deref().and_then(focused_app) == Some(OWN_APP),
            Device::Simulated(stand_in) => stand_in.own_in_front.load(Ordering::Relaxed),
        }
    }

    /// Put a screen in front: the new one, served on `port`, or the one the frame came with. Blocking.
    pub fn show(self, screen: Screen, port: u16) -> bool {
        match (self, screen) {
            (Device::Android, Screen::New) => {
                let address = format!("http://127.0.0.1:{port}/frame");
                let view = ["-a", "android.intent.action.VIEW", "-d", address.as_str()];
                if run("pm", &["path", APP]).is_some_and(|path| path.contains("package:")) {
                    return run("am", &[&["start", "-n", APP_ACTIVITY], &view[..]].concat()).is_some();
                }
                // Android's status bar stays hidden while the browser is in front (it applies to the browser only).
                run("settings", &["put", "global", "policy_control", &format!("immersive.full={BROWSER}")]);
                run("am", &[&["start", "-n", BROWSER_ACTIVITY], &view[..]].concat()).is_some()
            }
            (Device::Android, Screen::Legacy) => {
                // Closing whatever shows the new screen uncovers the frame's own; asking for home makes sure of it.
                run("am", &["force-stop", APP]);
                run("am", &["force-stop", BROWSER]);
                let home = ["start", "-a", "android.intent.action.MAIN", "-c", "android.intent.category.HOME"];
                run("am", &home).is_some()
            }
            (Device::Simulated(stand_in), screen) => {
                info!(?screen, "simulated: this screen would be put in front now");
                stand_in.own_in_front.store(screen == Screen::Legacy, Ordering::Relaxed);
                true
            }
        }
    }
}

/// The address this computer has on its network, which is what a phone would have to open.
/// Found by asking which of our addresses a packet to the outside would leave from; nothing is sent.
fn own_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_unspecified() && !address.is_loopback()).then(|| address.to_string())
}

/// Whether the new screen, in front for `shown_for` and last heard from `since_heard` ago, has
/// gone quiet: it is not running, and the frame should go back to the screen it came with.
pub fn gone_quiet(shown_for: Duration, since_heard: Duration) -> bool {
    shown_for >= QUIET_LIMIT && since_heard >= QUIET_LIMIT
}

/// Read Wi-Fi state out of `dumpsys wifi` and the address out of `ip -4 -o addr show wlan0`.
fn parse_wifi(state: &str, addresses: &str) -> Wifi {
    let on = state.lines().any(|line| line.trim() == "Wi-Fi is enabled");
    let info = state.lines().find(|line| line.trim_start().starts_with("mWifiInfo")).unwrap_or("");
    let field = |name: &str| {
        let rest = &info[info.find(name)? + name.len()..];
        Some(rest.split(", ").next().unwrap_or(rest).trim())
    };
    let connected = on && field("Supplicant state: ") == Some("COMPLETED");
    // The name may itself hold a comma, so it runs up to the field that always follows it.
    let name = info
        .find("SSID: ")
        .map(|at| &info[at + 6..])
        .map(|rest| rest.split(", BSSID: ").next().unwrap_or(rest).trim().trim_matches('"'))
        .filter(|name| !name.is_empty() && *name != "<unknown ssid>");
    let address = addresses
        .split_whitespace()
        .skip_while(|word| *word != "inet")
        .nth(1)
        .and_then(|with_mask| with_mask.split('/').next())
        .map(str::to_string);
    Wifi {
        on,
        network: name.filter(|_| connected).map(str::to_string),
        signal: field("RSSI: ").and_then(|dbm| dbm.parse().ok()).filter(|dbm| connected && *dbm > -127),
        address: address.filter(|_| connected),
    }
}

/// Read the newest light reading out of `dumpsys sensorservice`, which lists each
/// sensor's recent events as `N (ts=<seconds>, wall=<time>) <lux>, 0.00, 0.00,`.
fn parse_light(sensors: &str) -> Option<Light> {
    let events = sensors.lines().skip_while(|line| !line.contains("light sensor: last")).skip(1);
    let newest = events.take_while(|line| line.contains("(ts=")).last()?;
    let (stamp, values) = newest.split_once("(ts=")?.1.split_once(')')?;
    let seconds: f64 = stamp.split(',').next()?.trim().parse().ok()?;
    let lux: f32 = values.split(',').next()?.trim().parse().ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a clock reading, checked to be in range"
    )]
    (seconds.is_finite() && seconds >= 0.0 && lux.is_finite() && lux >= 0.0)
        .then_some(Light { lux, at: seconds as u64 })
}

/// Read which app has the window in front out of `dumpsys window`.
fn focused_app(windows: &str) -> Option<&str> {
    // mCurrentFocus=Window{1451474 u0 com.fujia.calendar/com.waophoto.smartframe.ui.SlideActivity}
    let focus = windows.lines().find_map(|line| line.trim().strip_prefix("mCurrentFocus="))?;
    let (app, _activity) = focus.split_whitespace().last()?.split_once('/')?;
    Some(app)
}

/// Read how long ago the frame was touched out of `dumpsys power`.
fn parse_untouched(power: &str) -> Option<Duration> {
    let line = power.lines().find(|line| line.trim_start().starts_with("mLastUserActivityTime="))?;
    let ago = line.split_once('(')?.1.split_whitespace().next()?;
    ago.parse().ok().map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTED: &str = "Wi-Fi is enabled\n  curState=ConnectedState\nmWifiInfo SSID: Home, Sweet Home, BSSID: 00:11:22:33:44:55, \
        MAC: 02:00:00:00:00:00, Supplicant state: COMPLETED, RSSI: -49, Link speed: 180Mbps, Frequency: 5260MHz, Net ID: 0\n";
    const ADDRESS: &str =
        "11: wlan0    inet 192.168.4.23/22 brd 192.168.7.255 scope global wlan0\\       valid_lft forever\n";

    #[test]
    fn wifi_state_is_read_from_what_android_prints() {
        assert_eq!(
            parse_wifi(CONNECTED, ADDRESS),
            Wifi {
                on: true,
                network: Some("Home, Sweet Home".into()),
                signal: Some(-49),
                address: Some("192.168.4.23".into())
            }
        );
        let quoted = CONNECTED.replace("Home, Sweet Home", "\"Cafe\"");
        assert_eq!(parse_wifi(&quoted, ADDRESS).network.as_deref(), Some("Cafe"));
    }

    #[test]
    fn a_frame_that_is_not_connected_reports_no_network() {
        let searching = "Wi-Fi is enabled\nmWifiInfo SSID: <unknown ssid>, BSSID: <none>, MAC: 02:00:00:00:00:00, \
            Supplicant state: SCANNING, RSSI: -127, Link speed: -1Mbps\n";
        assert_eq!(parse_wifi(searching, ""), Wifi { on: true, ..Wifi::default() });
        let stale = CONNECTED.replace("COMPLETED", "DISCONNECTED");
        assert_eq!(
            parse_wifi(&stale, ADDRESS),
            Wifi { on: true, ..Wifi::default() },
            "an old name is not a connection"
        );
        let off = CONNECTED.replace("Wi-Fi is enabled", "Wi-Fi is disabled");
        assert_eq!(parse_wifi(&off, ADDRESS), Wifi::default());
        assert_eq!(parse_wifi("", ""), Wifi::default());
        assert_eq!(parse_wifi("mWifiInfo", "inet"), Wifi::default());
    }

    #[test]
    fn the_app_in_front_is_read_from_what_android_prints() {
        let windows = "WINDOW MANAGER WINDOWS (dumpsys window windows)\n  \
            mCurrentFocus=Window{1451474 u0 com.fujia.calendar/com.waophoto.smartframe.ui.SlideActivity}\n  \
            mFocusedApp=AppWindowToken{8a1 token=Token{3c ActivityRecord{1e u0 com.fujia.calendar/.ui.MainActivity t14}}}\n";
        assert_eq!(focused_app(windows), Some(OWN_APP));
        let ours = windows.replace("com.fujia.calendar/com.waophoto.smartframe.ui.SlideActivity", APP_ACTIVITY);
        assert_eq!(focused_app(&ours), Some(APP));
        for unreadable in ["", "  mCurrentFocus=null", "  mCurrentFocus=Window{9 u0 StatusBar}"] {
            assert_eq!(focused_app(unreadable), None, "{unreadable:?}");
        }
    }

    #[test]
    fn the_time_since_the_last_touch_is_read_from_what_android_prints() {
        let power = "  mWakefulness=Awake\n  mLastUserActivityTime=39885086 (4150617 ms ago)\n  \
            mLastUserActivityTimeNoChangeLights=994055 (43041648 ms ago)\n";
        assert_eq!(parse_untouched(power), Some(Duration::from_millis(4_150_617)));
        for unreadable in ["", "mLastUserActivityTime=5", "mLastUserActivityTime=5 (soon)"] {
            assert_eq!(parse_untouched(unreadable), None, "{unreadable:?}");
        }
    }

    #[test]
    fn the_rooms_light_is_read_from_what_android_prints() {
        let sensors = "Mi3da 3-axis Accelerometer: last 50 events\n\t 1 (ts=48080.1, wall=11:08:32.2) 0.50, 9.70, 0.20, \n\
            pt3r850 light sensor: last 10 events\n\t 1 (ts=48081.405328000, wall=11:08:33.298) 110.00, 0.00, 0.00, \n\
            \t 2 (ts=48082.418660000, wall=11:08:34.312) 3.00, 0.00, 0.00, \nSensor fusion: off\n\t 3 (ts=99999.0, wall=x) 7.0,\n";
        assert_eq!(
            parse_light(sensors),
            Some(Light { lux: 3.0, at: 48_082 }),
            "the newest of the light sensor's own events"
        );
        for unreadable in [
            "",
            "pt3r850 light sensor: last 10 events\n",
            "light sensor: last 2 events\n\t 1 (ts=soon, wall=x) 5.0,\n",
            "light sensor: last 2 events\n\t 1 (ts=5.0, wall=x) bright,\n",
        ] {
            assert_eq!(parse_light(unreadable), None, "{unreadable:?}");
        }
    }

    #[test]
    fn the_stand_in_screen_can_be_switched() {
        let device = Device::simulated();
        assert!(device.switch_screen(false));
        assert_eq!(device.power().0, Some(false));
        assert!(device.switch_screen(true));
        assert_eq!(device.power().0, Some(true));
        assert_eq!(device.room_light().map(|light| light.lux), Some(110.0));
        device.stay_running();
    }

    #[test]
    fn the_new_screen_has_gone_quiet_only_after_a_fair_chance_to_be_heard() {
        let (short, long) = (Duration::from_secs(30), QUIET_LIMIT + Duration::from_secs(1));
        assert!(gone_quiet(long, long));
        assert!(!gone_quiet(short, long), "it was only just put in front");
        assert!(!gone_quiet(long, short), "it was heard from a moment ago");
    }

    #[test]
    fn the_stand_in_answers_without_an_android_to_ask() {
        let device = Device::simulated();
        assert!(device.is_simulated() && device.started());
        let wifi = device.wifi().unwrap();
        assert_eq!(wifi.network.as_deref(), Some("Simulated network"));
        let address: std::net::IpAddr = wifi.address.unwrap().parse().expect("an address a phone could open");
        assert!(!address.is_unspecified());
        assert!(device.open_wifi_settings() && device.show(Screen::New, 8080) && device.show(Screen::Legacy, 8080));
        assert_eq!(device.untouched_for(), Some(Duration::MAX));
        assert!(!Device::Android.is_simulated());
    }
}
