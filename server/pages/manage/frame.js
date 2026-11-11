// The remote control, the radio, and the frame's own settings. Part of the page for managing the frame.
"use strict";

// --- the remote control, and the radio stations ----------------------------------------------------------
// Whatever the frame's screen can be told to do is told the same way: an action such as
// { do: "next" } or { do: "play", station: "Jazz" }, sent to /api/act.
let stations = [], acting = null;
const TAB_NAMES = { pictures: "Pictures", calendar: "the Calendar", todo: "To do", utilities: "Utilities", settings: "Settings" };
/// What a reminder rule has the frame do, in words.
const thenText = act => act.do === "play" ? (act.station ? `then plays ${act.station}` : "then plays the radio") : act.do === "stop" ? "then stops the music" : act.do === "show" ? `then shows ${TAB_NAMES[act.tab] || act.tab}` : `then does “${act.do}”`;
async function loadRemote() {
  let a;
  try { a = await api("/api/acting"); } catch (e) { return; }
  acting = a; stations = a.stations || [];
  const said = a.said || {};
  setText($("rcSaid"), !a.listening ? "The frame's new screen is not running just now (the frame may be showing its original screen), so it would not hear these."
    : `The frame is on ${TAB_NAMES[said.tab] || "its screen"}${said.paused ? ", with the photos paused" : ""}. ${said.playing ? `Playing ${said.playing}, at volume ${said.volume}.` : "No music is playing."}`);
  setText($("rcPause"), said.paused ? "Resume the photos" : "Pause the photos");
  const sp = a.spotify || {}, track = sp.track;
  setText($("spSaid"), !sp.installed ? "Spotify's player is not installed on the frame (tools/deploy.py puts it there)."
    : !sp.on ? "Spotify cannot play on the frame at the moment."
    : sp.stage === "off" ? "Starting…"
    : sp.stage === "waiting" ? `Ready: choose “${sp.name}” as the speaker in the Spotify app.`
    : sp.stage === "idle" || !track ? "Someone has chosen the frame in Spotify; nothing is playing."
    : `${sp.stage === "playing" ? "Playing" : "Paused"}: ${track.title}${track.by ? `, ${track.by}` : ""}.`);
  connections.Spotify = sp.installed && sp.on ? { ok: true, said: sp.stage === "playing" ? "playing" : "ready" } : sp.installed ? { said: "off" } : null;
  drawConnections();
  $("spOn").disabled = !sp.installed;
  if (document.activeElement !== $("spOn")) $("spOn").checked = !!sp.on;
  if (changed("rcStations", [stations, said.playing])) {
    const box = $("rcStations"); box.textContent = "";
    for (const s of stations) {
      const play = el("button", s.name === said.playing ? "btn primary" : "btn", s.name);
      play.onclick = () => tell({ do: "play", station: s.name });
      box.append(play);
    }
    if (!stations.length) box.append(el("span", "muted", "no stations yet (add some below)"));
  }
  setText($("rsNone"), stations.length ? "" : "No stations yet.");
  if (changed("rsList", stations)) {
    const box = $("rsList"); box.textContent = "";
    stations.forEach((s, i) => {
      const remove = el("button", "btn small danger", "Delete");
      remove.onclick = () => saveStations(stations.filter((_, k) => k !== i), `${s.name} removed.`);
      const row = el("div"); row.append(el("span", "what", s.name), el("span", "from", s.stream), remove);
      box.append(row);
    });
  }
  // What a reminder rule can have the frame do.
  if (changed("rmThen", stations)) {
    const pick = $("rmThen"), was = pick.value; pick.textContent = "";
    const choices = [["", "and nothing else"]].concat(stations.map(s => [{ do: "play", station: s.name }, `and play ${s.name}`]),
      [[{ do: "stop" }, "and stop the music"], [{ do: "show", tab: "calendar" }, "and show the Calendar"], [{ do: "show", tab: "todo" }, "and show To do"]]);
    for (const [act, words] of choices) pick.append(new Option(words, act ? JSON.stringify(act) : ""));
    if ([...pick.options].some(o => o.value === was)) pick.value = was;
  }
}
/// Have the frame's screen do something, and say whether it was there to hear.
async function tell(action) {
  try {
    const r = await api("/api/act", action);
    if (!r.listening) toast("Sent, but the frame's new screen is not running to hear it.");
  } catch (e) { toast(e.message); }
  // The screen says how it stands a moment after doing it.
  setTimeout(loadRemote, 600); setTimeout(loadRemote, 2500);
}
async function saveStations(wanted, done) {
  try { await api("/api/stations", { stations: wanted }); toast(done); } catch (e) { toast(e.message); return false; }
  await loadRemote();
  return true;
}
const addStation = (name, stream) => saveStations(stations.concat([{ name, stream }]), `${name} added. The frame picks it up within half a minute.`);
async function findStations() {
  const name = $("rsName").value.trim();
  if (!name) return toast("Type the name of a station.");
  $("rsFind").disabled = true;
  const box = $("rsFound"); box.textContent = "";
  try {
    const found = (await api(`/api/stations/find?name=${encodeURIComponent(name)}`)).stations;
    if (!found.length) box.append(el("div", "muted", "No station by that name that the frame can play."));
    for (const s of found) {
      const add = el("button", "btn small", "Add");
      add.onclick = async () => { if (await addStation(s.name, s.stream)) add.closest("div").remove(); };
      const hear = el("button", "btn small", "Listen here");
      // On this phone or computer, to tell whether it is the right one before the frame plays it.
      hear.onclick = () => { const on = hear.textContent !== "Listen here"; preview.pause(); for (const b of box.querySelectorAll("button")) if (b.textContent === "Stop listening") b.textContent = "Listen here"; if (!on) { preview.src = s.stream; preview.play().catch(() => toast("This browser could not play it.")); hear.textContent = "Stop listening"; } };
      const row = el("div"); row.append(el("span", "what", s.name), el("span", "from", s.about), hear, add);
      box.append(row);
    }
  } catch (e) { toast(e.message); }
  $("rsFind").disabled = false;
}
const preview = new Audio(); preview.preload = "none";

// --- the frame: its screen, night, sound, weather and pictures ----------------------------------------------
let frame = null;
const inFrameTab = () => $("tab-frame").contains(document.activeElement) && document.activeElement.tagName !== "BUTTON";
async function loadFrame() {
  let d, w;
  try { d = await api("/api/device"); w = await api("/api/weather"); } catch (e) { return; }
  frame = d; loadRemote();
  const wifi = d.wifi, strength = !wifi || wifi.signal === null ? "" : wifi.signal >= -60 ? ", strong signal" : wifi.signal >= -72 ? ", good signal" : ", weak signal";
  let screen = `The frame is showing ${d.screen === "new" && !d.fell_back ? "the new screen" : "its original screen"}`;
  if (d.asleep) screen += ", switched off for the night";
  screen += wifi ? (wifi.network ? `. Wi-Fi: ${wifi.network}${strength}.` : wifi.on ? ". Wi-Fi: not connected." : ". Wi-Fi is off.") : ".";
  if (d.fell_back) screen += " The new screen stopped responding, so the frame went back to its original screen.";
  if (d.simulated) screen += " (Simulated on this computer: nothing is changed on a real frame.)";
  setText($("frScreen"), screen);
  setText($("frSwap"), d.screen === "new" && !d.fell_back ? "Show the original screen" : "Show the new screen");
  setText($("frHouse"), !d.home ? "The house is not connected." : d.home_in_touch ? "The house is connected and its relay is in touch." : "The house is connected, but its relay has not been heard from in the last minute and a half.");
  setText($("frConnect"), d.home ? "Make a new key" : "Connect the house");
  show($("frDisconnect"), d.home);
  const yt = d.yoto_now, players = yt.players.map(p => p.name).join(", ");
  setText($("ytSaid"), !d.yoto ? "Not set up." : yt.problem ? `Set up, but: ${yt.problem}.` : !yt.in_touch ? "Set up; waiting to hear from Lullimit." : yt.players.length ? `Reading Lullimit. Players: ${players}.` : "Reading Lullimit, which lists no players for this account.");
  connections.House = d.home ? { ok: d.home_in_touch, problem: !d.home_in_touch, said: d.home_in_touch ? "in touch" : "not heard from" } : null;
  connections.Lullimit = d.yoto ? { ok: yt.in_touch && !yt.problem, problem: !!yt.problem, said: yt.problem ? "problem" : yt.in_touch ? "reading" : "waiting" } : null;
  drawConnections();
  setText($("ytSave"), d.yoto ? "Use a new key" : "Use this key");
  show($("ytForget"), d.yoto);
  setText($("frLight"), typeof d.light === "number" ? `The room now measures ${Math.round(d.light)} lux. Under 4 counts as dark.` : "");
  const place = d.place ? `Showing the weather for ${d.place}.` : "No place chosen, so no weather is shown.";
  setText($("frPlace"), w.now ? `${place} Now ${Math.round(w.now.temp)}°.` : place);
  show($("frForget"), !!d.place);
  // What someone is in the middle of changing is left alone.
  if (inFrameTab() || !changed("frame", [d.night, d.chime, d.slideshow, photos.length])) return;
  $("frNight").checked = d.night.on; $("frFrom").value = d.night.from; $("frUntil").value = d.night.until; $("frDark").checked = d.night.when_dark;
  $("frChime").value = d.chime === null ? "" : String(d.chime);
  $("frEvery").value = String(d.slideshow.every); $("frOrder").value = d.slideshow.order;
  // Which photos: all, the favourites, or one of the years there are photos from.
  const which = d.slideshow.show.year ? String(d.slideshow.show.year) : d.slideshow.show;
  const years = [...new Set(photos.map(p => new Date(p[2] * 1000).getFullYear()).concat(d.slideshow.show.year || []))].sort();
  const pick = $("frShow"); pick.textContent = "";
  pick.append(new Option("all the photos", "all"), new Option("only the favourites", "favourites"), ...years.map(y => new Option(`only photos from ${y}`, String(y))));
  pick.value = which;
  $("frHome").value = d.slideshow.home_after === null ? "" : String(d.slideshow.home_after);
  $("frMemories").checked = d.slideshow.memories; $("frWeather").checked = d.slideshow.weather; $("frMorning").checked = d.slideshow.morning;
}
async function saveFrame(path, body, done) {
  try { await api(path, body); toast(done); } catch (e) { toast(e.message); }
  drawn.delete("frame"); document.activeElement.blur(); loadFrame();
}
const saveNight = () => saveFrame("/api/night", { on: $("frNight").checked, from: $("frFrom").value, until: $("frUntil").value, when_dark: $("frDark").checked }, "Night settings saved.");
const shownChoice = () => /^\d+$/.test($("frShow").value) ? { year: Number($("frShow").value) } : $("frShow").value;
const savePictures = () => saveFrame("/api/slideshow", { show: shownChoice(), every: Number($("frEvery").value), order: $("frOrder").value, home_after: $("frHome").value === "" ? null : Number($("frHome").value), memories: $("frMemories").checked, weather: $("frWeather").checked, morning: $("frMorning").checked }, "Saved. The frame picks it up within half a minute.");
async function findPlace() {
  const name = $("frTown").value.trim();
  if (!name) return toast("Type the name of a town or city.");
  $("frFind").disabled = true;
  try { const r = await api("/api/weather/place", { name }); $("frTown").value = ""; toast(`Weather will be shown for ${r.place}.`); } catch (e) { toast(e.message); }
  $("frFind").disabled = false; loadFrame();
}
