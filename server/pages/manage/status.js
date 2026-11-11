// Status, and the wiring of the page. Part of the page for managing the frame.
"use strict";

// --- status -----------------------------------------------------------------------------------------------
function card(title, rows, above, below) {
  const c = el("div", "card"); c.append(el("h2", "", title));
  if (above) c.append(above);
  const dl = el("dl");
  for (const [k, v, cls] of rows) dl.append(el("dt", "", k), el("dd", cls || "", v));
  c.append(dl); if (below) c.append(below);
  return c;
}
async function loadStatus() {
  let s;
  try { s = await api("/api/status"); } catch (e) { return toast(e.message); }
  if (!changed("status", [Object.assign({}, s, { uptime_secs: Math.floor(s.uptime_secs / 600) }), unlocked])) return;
  const box = $("statusCards"); box.textContent = "";
  const src = s.photos.by_source, line = k => `${src[k].count.toLocaleString()} · ${fmtBytes(src[k].bytes)}`;
  const storage = [["Synced from albums", line("synced")], ["Added by hand", line("added")], ["From the phone app", line("phone")], ["All photos", `${s.photos.count.toLocaleString()} · ${fmtBytes(s.photos.bytes)}`]];
  let meter = null;
  if (s.disk) {
    const [total, free] = s.disk, used = total - free;
    meter = el("div", "meter");
    const ours = el("span", "ours"); ours.style.width = `${100 * s.photos.bytes / total}%`; ours.title = "Photos";
    const other = el("span", "other"); other.style.width = `${100 * Math.max(0, used - s.photos.bytes) / total}%`; other.title = "Everything else on the frame";
    meter.append(ours, other);
    storage.push(["Free space", `${fmtBytes(free)} of ${fmtBytes(total)}`, free < 1e9 ? "bad" : ""]);
  }
  box.append(card("Storage", storage, meter));

  const y = s.sync, sync = [["Albums", y.albums ? String(y.albums) : "none saved"]];
  sync.push(["Last sync", y.running ? "running now" : y.last ? fmtWhen(y.last) : "never"]);
  if (y.last && !y.running) sync.push(["Result", y.ok ? "succeeded" : "failed", y.ok ? "good" : "bad"]);
  if (y.summary) sync.push(["Details", y.summary]);
  sync.push(["Daily sync", y.auto ? "on" : "off"]);
  if (y.next) sync.push(["Next daily sync", y.next * 1000 < Date.now() ? "due now" : fmtWhen(y.next)]);
  if (y.auto && !y.albums) sync.push(["Note", "Daily sync is on but no album is saved", "bad"]);
  box.append(card("Google Photos sync", sync));

  const n = s.photos.count, pr = s.prepared, done = v => v >= n ? "all done" : `${v.toLocaleString()} of ${n.toLocaleString()}`;
  const faces = !pr.face_models ? "face models not installed" : pr.with_look_alikes ? `${Math.min(pr.people, pr.with_look_alikes).toLocaleString()} of ${pr.with_look_alikes.toLocaleString()} photos with look-alikes` : `${pr.people.toLocaleString()} photos`;
  box.append(card("Preparation", [["Thumbnails", done(pr.thumbnails)], ["Analysed for duplicates", done(pr.fingerprints)], ["Measured for sharpness", done(pr.quality)], ["Checked for eyes and smiles", faces], ["In the bin or deleted for good", String(s.excluded)]]));

  const pinBar = el("div", "bar"); pinBar.style.margin = "14px 0 0";
  const setBtn = el("button", "btn", s.pin ? "Change PIN" : "Set a PIN");
  setBtn.onclick = async () => {
    const pin = prompt("New PIN (4 to 8 digits). It will be needed to add, delete or sync; viewing stays open.");
    if (!pin) return;
    if (prompt("Type the new PIN again to confirm:") !== pin) return toast("The two PINs did not match.");
    try { const r = await api("/api/pin", { pin }); setToken(r.token); pinSet = true; toast("PIN set. This browser stays unlocked."); loadStatus(); } catch (e) { toast(e.message); }
  };
  pinBar.append(setBtn);
  if (s.pin) {
    const off = el("button", "btn danger", "Remove PIN");
    off.onclick = async () => {
      if (!confirm("Remove the PIN? Anyone on your Wi-Fi will be able to make changes.")) return;
      try { await api("/api/pin", { pin: "" }); saved.set("frameToken", null); token = null; pinSet = false; toast("PIN removed."); loadStatus(); } catch (e) { toast(e.message); }
    };
    pinBar.append(off);
  }
  box.append(card("Protection", [["PIN", s.pin ? "set" : "not set", s.pin ? "good" : ""], ["This browser", !s.pin ? "no PIN needed" : unlocked ? "unlocked" : "locked"]], null, pinBar));

  const up = s.uptime_secs, upText = up >= 86400 ? `${Math.floor(up / 86400)} d ${Math.floor(up % 86400 / 3600)} h` : up >= 3600 ? `${Math.floor(up / 3600)} h ${Math.floor(up % 3600 / 60)} min` : `${Math.max(1, Math.round(up / 60))} min`;
  const logLink = el("a", "", "Open the log"); logLink.href = "/logs"; logLink.target = "_blank"; logLink.style.cssText = "display:inline-block;margin-top:14px";
  const screenLink = el("a", "", "Open the frame's own screen"); screenLink.href = "/frame"; screenLink.target = "_blank"; screenLink.style.cssText = "display:block;margin-top:8px";
  const serverLinks = el("div"); serverLinks.append(logLink, screenLink);
  box.append(card("Server", [["Version", s.version], ["Running for", upText]], null, serverLinks));
}

// --- wiring --------------------------------------------------------------------------------------------------
function showTab(tab) {
  if (!TABS.includes(tab)) tab = "photos";
  saved.set("frameManageTab", tab);
  document.querySelectorAll("nav button").forEach(b => b.classList.toggle("active", b.dataset.tab === tab));
  for (const t of TABS) show($("tab-" + t), t === tab);
  if (tab === "status") loadStatus();
  if (tab === "calendar") loadCalendar();
  if (tab === "frame") loadFrame();
  if (tab === "todo") loadTodo();
  if (tab === "duplicates" && !$("dupGroups").childElementCount) findDuplicates();
}
document.querySelectorAll("nav button").forEach(b => b.onclick = () => { location.hash = b.dataset.tab; });
// The Frame tab's cards come in three groups, one at a time; the last one looked at is kept.
function showFramePart(part) {
  if (!["screen", "connections", "display"].includes(part)) part = "screen";
  $("tab-frame").dataset.part = part; saved.set("frameManagePart", part);
  document.querySelectorAll("#frameParts button").forEach(b => b.classList.toggle("active", b.dataset.part === part));
}
document.querySelectorAll("#frameParts button").forEach(b => b.onclick = () => showFramePart(b.dataset.part));
showFramePart(saved.get("frameManagePart"));
// One line for each connection, at the top of the Frame tab: what is well, and what is not.
const connections = {};
function drawConnections() {
  const box = $("connections"); box.textContent = "";
  for (const [name, c] of Object.entries(connections)) {
    if (!c) continue;
    const pill = document.createElement("button"); pill.className = "pill" + (c.problem ? " problem" : "");
    const dot = document.createElement("span"); dot.className = "dot " + (c.problem ? "off" : c.ok ? "on" : "");
    pill.append(dot, `${name}: ${c.said}`);
    pill.onclick = () => showFramePart("connections");
    box.append(pill);
  }
}
window.onhashchange = () => { showTab(location.hash.slice(1)); scrollTo(0, 0); };
// The sticky toolbar sits just under the header, whatever height that wraps to.
const fitHeader = () => document.documentElement.style.setProperty("--header", $("header").offsetHeight + "px");
new ResizeObserver(fitHeader).observe($("header")); fitHeader();

// The colours: the system's, light, dark, or light by day (07:00 to 19:00) and dark by night. Kept by this browser.
function applyTheme() {
  const theme = saved.get("frameTheme") || "system", hour = new Date().getHours();
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme === "time" ? (hour >= 7 && hour < 19 ? "light" : "dark") : theme;
}
$("theme").value = saved.get("frameTheme") || "system";
$("theme").onchange = e => { saved.set("frameTheme", e.target.value === "system" ? null : e.target.value); applyTheme(); };
setInterval(applyTheme, 60000);
$("sort").value = sortBy;
$("sort").onchange = e => { sortBy = e.target.value; saved.set("frameSort", sortBy); page = 0; render(); };
$("jump").onchange = e => { if (e.target.value !== "") { page = Math.floor(e.target.value / PAGE_SIZE); render(); scrollTo(0, 0); e.target.value = ""; } };
$("prev").onclick = () => { page--; render(); scrollTo(0, 0); };
$("next").onclick = () => { page++; render(); scrollTo(0, 0); };
$("selClear").onclick = () => { selected.clear(); drawn.delete("grid"); render(); };
$("selDelete").onclick = () => del([...selected]);
$("nowDelete").onclick = () => { if (nowRel) del([nowRel]); };
$("addBtn").onclick = () => $("addInput").click();
$("addInput").onchange = e => { addFiles(e.target.files); e.target.value = ""; };
$("lockBtn").onclick = () => unlock();
$("reloadShow").onclick = () => api("/api/refresh-slideshow", {}).then(() => toast("The slideshow will restart with the current photos."), e => toast(e.message));
$("dupFind").onclick = findDuplicates;
$("dupLevel").onchange = findDuplicates;
$("dupDeleteAll").onclick = async () => { const others = dupGroups.flatMap(g => g.rels.filter((_, i) => i !== g.keep)); if (others.length && await del(others)) findDuplicates(); };
$("exMore").onclick = () => { bin.shown += BIN_PAGE; renderBin(); };
$("emptyBin").onclick = () => { if (confirm(`Empty the bin? ${plural(bin.names.length, "photo")} will stay off the frame for good and can no longer be restored from here.`)) api("/api/empty-bin", {}).then(r => { toast(`Emptied ${plural(r.emptied, "photo")} from the bin.`); load(false); }, e => toast(e.message)); };
$("forgetGone").onclick = () => { if (confirm(`Allow the ${plural(bin.gone, "photo")} deleted for good to be downloaded again? The next sync will put them back on the frame.`)) api("/api/forget-emptied", {}).then(() => { toast("They will come back at the next sync."); load(false); }, e => toast(e.message)); };
$("restoreAll").onclick = () => { if (confirm(`Restore all ${plural(bin.names.length, "photo")} in the bin?`)) api("/api/restore", { names: "all" }).then(() => load(false), e => toast(e.message)); };
$("sources").onchange = () => api("/api/sources", { sources: $("sources").value.split("\n") }).then(() => toast("Album links saved."), e => toast(e.message));
$("optAuto").onchange = e => api("/api/auto", { enabled: e.target.checked }).then(r => toast(r.auto ? "The frame will sync these albums once a day." : "Daily sync is off."), err => toast(err.message));
$("calLinks").onchange = () => api("/api/calendars", { links: $("calLinks").value.split("\n") }).then(() => { toast("Calendars saved. The frame is fetching them."); drawn.delete("calLinks"); loadCalendar(); setTimeout(loadCalendar, 4000); }, e => toast(e.message));
$("evAdd").onclick = addEvent;
$("rmAdd").onclick = addRule;
$("pcAdd").onclick = addEventPicture;
$("tdApproval").onchange = e => saveTodo("/api/todo/approval", { on: e.target.checked }, e.target.checked ? "Points now wait for approval." : "Points are now added at once; what was waiting has been added.");
$("tdAddRule").onclick = addEventRule;
$("tdWords").onkeydown = e => { if (e.key === "Enter") addEventRule(); };
$("tdAddPerson").onclick = addPerson; $("tdAddTask").onclick = addTask; $("tdAddReward").onclick = addReward; $("tdAddAward").onclick = addAward;
$("tdName").onkeydown = e => { if (e.key === "Enter") addPerson(); };
$("tdTitle").onkeydown = e => { if (e.key === "Enter") addTask(); };
$("tdReward").onkeydown = e => { if (e.key === "Enter") addReward(); };
$("tdRepeat").onchange = () => { show($("tdDay"), $("tdRepeat").value === "day"); show($("tdDays"), $("tdRepeat").value === "days"); };
DAY_NAMES.forEach((name, i) => { const label = el("label"), box = el("input"); box.type = "checkbox"; box.value = i + 1; label.append(box, ` ${name}`); $("tdDays").append(label); });
$("tdDay").value = todayText();
$("trHome").onchange = () => { if (travel) { tripPlans = {}; saveTravel($("trHome").value.trim(), travel.trips, "Home saved."); } };
$("trKeySave").onclick = () => { if ($("trKey").value.trim()) saveRoadsKey($("trKey").value.trim()); else { toast("Paste the key first."); $("trKey").focus(); } };
$("trKeyForget").onclick = () => { if (confirm("Forget the key? Trips by car stay listed but can no longer be worked out.")) saveRoadsKey(null); };
$("rmWhen").onchange = () => show($("rmAt"), $("rmWhen").value[0] === "d");
for (const id of ["rmWords", "rmSay"]) $(id).onkeydown = e => { if (e.key === "Enter") addRule(); };
for (const id of ["pcWords", "pcPicture", "pcSay"]) $(id).onkeydown = e => { if (e.key === "Enter") addEventPicture(); };
for (const id of ["frNight", "frFrom", "frUntil", "frDark"]) $(id).onchange = saveNight;
for (const id of ["frShow", "frEvery", "frOrder", "frHome", "frMemories", "frWeather", "frMorning"]) $(id).onchange = savePictures;
$("frChime").onchange = e => saveFrame("/api/chime", { before: e.target.value === "" ? null : Number(e.target.value) }, "Chime saved.");
$("frConnect").onclick = async () => {
  if (frame && frame.home && !confirm("Make a new key? The relay stops being listened to until the new key is put in its settings.")) return;
  try {
    const r = await api("/api/home/key", {});
    // Shown this once: it goes in the relay's settings on the machine that runs Home Assistant.
    $("frKey").textContent = `FRAME_URL=${location.origin}\nFRAME_KEY=${r.key}`; show($("frKey"), true);
    toast("Key made. Copy the two lines into the relay's settings.");
  } catch (e) { toast(e.message); }
  loadFrame();
};
async function saveYotoKey(key) {
  $("ytSave").disabled = true;
  try { await api("/api/yoto/key", { key }); toast(key ? "Key saved. Yoto is now in Utilities on the frame." : "The key is forgotten. Yoto is gone from the frame."); $("ytKey").value = ""; } catch (e) { toast(e.message); }
  $("ytSave").disabled = false; loadFrame();
}
$("ytSave").onclick = () => { const key = $("ytKey").value.trim(); if (key) saveYotoKey(key); else { toast("Paste the key first."); $("ytKey").focus(); } };
$("ytKey").onkeydown = e => { if (e.key === "Enter") $("ytSave").onclick(); };
$("ytForget").onclick = () => { if (confirm("Forget the Lullimit key? Yoto goes from the frame's Utilities.")) saveYotoKey(null); };
$("frDisconnect").onclick = () => { if (confirm("Disconnect the house? The frame forgets what it knew and stops listening to the relay.")) { show($("frKey"), false); saveFrame("/api/home/forget", {}, "The house is disconnected."); } };
for (const button of document.querySelectorAll("[data-act]")) button.onclick = () => tell(JSON.parse(button.dataset.act));
$("rcPause").onclick = () => tell({ do: acting && acting.said && acting.said.paused ? "resume" : "pause" });
$("rcTimer").onclick = () => tell({ do: "timer", minutes: Number($("rcMinutes").value) });
$("rcSay").onclick = () => { const say = $("rcNote").value.trim(); if (!say) return toast("Type the note first."); tell({ do: "note", say }); $("rcNote").value = ""; };
$("rcNote").onkeydown = e => { if (e.key === "Enter") $("rcSay").onclick(); };
$("spOn").onchange = async e => { try { await api("/api/spotify", { on: e.target.checked }); toast(e.target.checked ? "Spotify can now play on the frame." : "Spotify is switched off on the frame."); } catch (err) { toast(err.message); } e.target.blur(); setTimeout(loadRemote, 1500); loadRemote(); };
$("rsFind").onclick = findStations;
$("rsName").onkeydown = e => { if (e.key === "Enter") findStations(); };
$("rsOwnAdd").onclick = async () => { const name = $("rsOwnName").value.trim(), stream = $("rsOwnStream").value.trim(); if (!name || !stream) return toast("Give the station a name and the address of its stream."); if (await addStation(name, stream)) { $("rsOwnName").value = ""; $("rsOwnStream").value = ""; } };
$("frFind").onclick = findPlace;
$("frTown").onkeydown = e => { if (e.key === "Enter") findPlace(); };
$("frForget").onclick = () => saveFrame("/api/weather/forget", {}, "The weather is no longer shown.");
$("frSwap").onclick = () => { const other = frame && frame.screen === "new" && !frame.fell_back ? "legacy" : "new"; saveFrame("/api/screen", { show: other }, other === "new" ? "The frame is switching to the new screen." : "The frame is switching to its original screen."); };
$("frWifi").onclick = () => { if (confirm("Open the list of Wi-Fi networks on the frame's screen? Someone has to be at the frame to choose one. It returns to its screen a minute after it was last touched.")) saveFrame("/api/wifi/settings", {}, "The frame is showing its Wi-Fi networks."); };
$("evTitle").onkeydown = e => { if (e.key === "Enter") addEvent(); };
{ const d = new Date(); $("evDay").value = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; }
$("syncStart").onclick = async () => {
  const limit = parseInt($("optLimit").value, 10);
  const body = { sources: $("sources").value.split("\n"), delete: $("optDelete").checked, refresh: $("optRefresh").checked, limit: Number.isFinite(limit) ? limit : null };
  if (body.delete && !confirm("Synced photos that are not in these albums will be removed from the frame. Continue?")) return;
  try { await api("/api/sync", body); jobWasRunning = true; load(false); } catch (e) { toast(e.message); }
};

$("lbClose").onclick = closeLightbox; $("lbPrev").onclick = () => stepLightbox(-1); $("lbNext").onclick = () => stepLightbox(1);
$("lbDelete").onclick = async () => { const i = lb; if (await del([lbRels()[i]])) { const n = lbRels().length; n ? openLightbox(Math.min(i, n - 1)) : closeLightbox(); } };
$("lbFav").onclick = () => markPhoto(markOf(lbRels()[lb]) === "favourite" ? null : "favourite");
$("lbHide").onclick = () => markPhoto(markOf(lbRels()[lb]) === "hidden" ? null : "hidden");
$("lbKeep").onclick = () => { if (viewer) { viewer.keep = lb; renderGroup(viewer); openLightbox(lb); } };
$("stage").onclick = e => { if (e.target === $("stage")) closeLightbox(); };
document.onkeydown = e => { if (lb < 0) return; if (e.key === "Escape") closeLightbox(); if (e.key === "ArrowLeft") stepLightbox(-1); if (e.key === "ArrowRight") stepLightbox(1); };
// Swipe left and right in the viewer on touch screens.
let swipeFrom = null;
$("stage").addEventListener("touchstart", e => { swipeFrom = e.touches.length === 1 ? e.touches[0].clientX : null; }, { passive: true });
$("stage").addEventListener("touchend", e => { if (swipeFrom === null) return; const dx = e.changedTouches[0].clientX - swipeFrom; swipeFrom = null; if (Math.abs(dx) > 50) stepLightbox(dx < 0 ? 1 : -1); }, { passive: true });

let dragDepth = 0;
const hasFiles = e => e.dataTransfer && [...e.dataTransfer.types].includes("Files");
window.addEventListener("dragenter", e => { if (hasFiles(e)) { dragDepth++; $("drop").classList.add("on"); } });
window.addEventListener("dragleave", e => { if (hasFiles(e) && --dragDepth <= 0) { dragDepth = 0; $("drop").classList.remove("on"); } });
window.addEventListener("dragover", e => { if (hasFiles(e)) e.preventDefault(); });
window.addEventListener("drop", e => { if (!hasFiles(e)) return; e.preventDefault(); dragDepth = 0; $("drop").classList.remove("on"); addFiles(e.dataTransfer.files); });

showTab(location.hash.slice(1) || saved.get("frameManageTab"));
load(true);
setInterval(() => { if (!document.hidden) load(needFull); }, 4000);
setInterval(reportWatching, 5000);
setInterval(() => { if (!$("tab-status").classList.contains("hidden") && !document.hidden) loadStatus(); }, 10000);
setInterval(() => { if (!$("tab-calendar").classList.contains("hidden") && !document.hidden) loadCalendar(); }, 15000);
setInterval(() => { if (!$("tab-frame").classList.contains("hidden") && !document.hidden) loadFrame(); }, 10000);
setInterval(() => { if (!$("tab-todo").classList.contains("hidden") && !document.hidden) loadTodo(); }, 8000);
