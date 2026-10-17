// What the page for managing the frame does: photos, duplicates, syncing, the calendar and the frame's settings.
"use strict";
const PAGE_SIZE = 60, MAX_SIDE = 1920, BIN_PAGE = 60, TABS = ["photos", "duplicates", "sync", "calendar", "todo", "frame", "deleted", "status"];
const KINDS = { all: "All", synced: "Synced", added: "Added here", phone: "From phone app", favourite: "★ Favourites", hidden: "Hidden from the frame" };
const $ = id => document.getElementById(id);
const el = (tag, className, text) => { const e = document.createElement(tag); if (className) e.className = className; if (text !== undefined) e.textContent = text; return e; };
const saved = { get: k => { try { return localStorage.getItem(k); } catch (e) { return null; } }, set: (k, v) => { try { v === null ? localStorage.removeItem(k) : localStorage.setItem(k, v); } catch (e) {} } };

// What the page knows. Photos are [path, kind, taken, added].
let photos = [], filter = "all", page = 0, sortBy = saved.get("frameSort") || "newest", selected = new Set();
let bin = { names: [], at: [], gone: 0, days: 30, shown: BIN_PAGE };
let token = saved.get("frameToken"), pinSet = false, unlocked = true;
let lb = -1, viewer = null, nowRel = null, adding = false, needFull = false, jobWasRunning = false;
let dupGroups = [], dupTotals = [0, 0], listKey = "", listCache = [];

// --- drawing only what changed --------------------------------------------------------
// The page polls the frame every few seconds. Redrawing a section whose data is the same
// makes images reload and the page flicker, so every section remembers what it last drew.
const drawn = new Map();
const changed = (section, data) => { const key = JSON.stringify(data); if (drawn.get(section) === key) return false; drawn.set(section, key); return true; };
const setText = (node, text) => { if (node.textContent !== text) node.textContent = text; };
const show = (node, visible) => node.classList.toggle("hidden", !visible);
function image(src) { const img = el("img"); img.loading = "lazy"; img.decoding = "async"; img.alt = ""; img.onload = () => img.classList.add("ready"); img.src = src; return img; }
function toast(message) { const t = $("toast"); t.textContent = message; t.classList.add("on"); clearTimeout(toast.timer); toast.timer = setTimeout(() => t.classList.remove("on"), 5000); }

const fmtBytes = n => n >= 1e9 ? (n / 1e9).toFixed(1) + " GB" : n >= 1e6 ? Math.round(n / 1e6) + " MB" : Math.round(n / 1e3) + " kB";
const fmtWhen = secs => new Date(secs * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
const monthOf = secs => new Date(secs * 1000).toLocaleDateString(undefined, { month: "long", year: "numeric" });
const plural = (n, word) => `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;
const full = rel => `/file?p=${encodeURIComponent(rel)}`, thumb = rel => `/thumb?p=${encodeURIComponent(rel)}`;
const kindOf = rel => (photos.find(p => p[0] === rel) || [])[1];

// --- talking to the frame ----------------------------------------------------------------
function authHeaders(extra) { const h = Object.assign({ "X-Frame-App": "1" }, extra); if (token) h["X-Frame-Token"] = token; return h; }
async function api(path, body, retried) {
  const r = await fetch(path, { method: body ? "POST" : "GET", headers: authHeaders({ "Content-Type": "application/json" }), body: body ? JSON.stringify(body) : undefined });
  const j = await r.json();
  if (r.status === 401 && !retried && await unlock()) return api(path, body, true);
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}
function setToken(value) { token = value; saved.set("frameToken", value); unlocked = true; show($("lockBtn"), false); }
// Ask for the PIN and exchange it for a session. Returns whether changes are now allowed.
async function unlock() {
  const pin = prompt("Enter the PIN to make changes on the frame:");
  if (!pin) return false;
  const r = await fetch("/api/unlock", { method: "POST", headers: { "X-Frame-App": "1", "Content-Type": "application/json" }, body: JSON.stringify({ pin }) });
  const j = await r.json();
  if (!r.ok) { toast(j.error || "Could not unlock"); return false; }
  setToken(j.token); toast("Unlocked on this browser.");
  return true;
}

// --- the photo list ------------------------------------------------------------------------
// The photos in view, filtered and ordered; recomputed only when something changed.
function shown() {
  const key = `${filter}|${sortBy}|${photos.length}|${photos.length ? photos[0][0] : ""}`;
  if (key !== listKey) {
    listCache = filter === "all" ? photos.slice() : photos.filter(p => p[1] === filter || p[4] === filter);
    if (sortBy === "newest") listCache.sort((a, b) => b[2] - a[2]);
    else if (sortBy === "oldest") listCache.sort((a, b) => a[2] - b[2]);
    else listCache.sort((a, b) => b[3] - a[3]);
    listKey = key;
  }
  return listCache;
}
function makeTile(rel, kind, taken, mark) {
  const tile = el("div", "tile" + (selected.has(rel) ? " sel" : "") + (mark === "hidden" ? " hid" : ""));
  tile.title = fmtWhen(taken);
  tile.append(image(thumb(rel)));
  if (mark) tile.append(el("span", "tag mark", mark === "hidden" ? "Hidden" : "★"));
  if (kind !== "synced") tile.append(el("span", "tag", kind === "phone" ? "Phone app" : "Added here"));
  return tile;
}
function toggle(rel, tile) { selected.has(rel) ? selected.delete(rel) : selected.add(rel); tile.classList.toggle("sel"); renderSelection(); }
function renderSelection() {
  const n = selected.size;
  for (const id of ["selInfo", "selClear", "selDelete"]) show($(id), n > 0);
  setText($("selCount"), String(n));
}
function renderChips() {
  const counts = { all: photos.length };
  for (const p of photos) { counts[p[1]] = (counts[p[1]] || 0) + 1; if (p[4]) counts[p[4]] = (counts[p[4]] || 0) + 1; }
  if (!changed("chips", [counts, filter])) return;
  const chips = $("chips"); chips.textContent = "";
  for (const [kind, label] of Object.entries(KINDS)) {
    if (kind !== "all" && !counts[kind]) continue;
    const b = el("button", kind === filter ? "active" : "", `${label} ${(counts[kind] || 0).toLocaleString()}`);
    b.onclick = () => { filter = kind; page = 0; render(); };
    chips.append(b);
  }
}
function renderJump(list) {
  const jump = $("jump");
  show(jump, sortBy !== "added" && list.length > 0);
  if (sortBy === "added" || !changed("jump", listKey)) return;
  jump.textContent = "";
  jump.append(new Option("Jump to…", ""));
  let last = null, option = null, count = 0;
  list.forEach((p, i) => {
    const month = monthOf(p[2]);
    if (month !== last) { if (option) option.textContent += ` (${count})`; option = new Option(month, i); jump.append(option); last = month; count = 0; }
    count++;
  });
  if (option) option.textContent += ` (${count})`;
}
function render() {
  renderChips();
  const list = shown(), pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
  page = Math.min(page, pages - 1);
  renderJump(list);
  setText($("pageInfo"), `${page + 1} / ${pages}`);
  $("prev").disabled = page === 0; $("next").disabled = page >= pages - 1;
  setText($("empty"), photos.length ? "" : "There are no photos on the frame yet. Add some with the button above.");
  const visible = list.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE);
  if (changed("grid", [visible.map(p => [p[0], p[4]]), sortBy])) {
    const grid = $("grid"); grid.textContent = "";
    let month = null;
    visible.forEach(([rel, kind, taken, , mark], i) => {
      if (sortBy !== "added" && monthOf(taken) !== month) { month = monthOf(taken); grid.append(el("h2", "month", month)); }
      const tile = makeTile(rel, kind, taken, mark), pick = el("button", "pick", "✓");
      pick.title = "Select"; pick.onclick = e => { e.stopPropagation(); toggle(rel, tile); };
      tile.onclick = () => openLightbox(page * PAGE_SIZE + i);
      tile.append(pick); grid.append(tile);
    });
  }
  renderSelection();
}

// --- what the frame is showing, the bin, the sync job ------------------------------------------
function renderNow(now) {
  show($("now"), !!now);
  nowRel = now ? now.rel : null;
  if (!now || !changed("now", now)) return;
  const fresh = image(thumb(now.rel)); fresh.id = "nowImg"; fresh.alt = "Photo on the frame right now"; fresh.loading = "eager"; fresh.classList.add("ready");
  $("nowImg").replaceWith(fresh);
  setText($("nowText"), { synced: "Synced photo", added: "Added by hand", phone: "Sent from the phone app" }[now.kind]);
}
function renderBin() {
  setText($("exCount"), bin.names.length ? `(${bin.names.length.toLocaleString()})` : "");
  $("restoreAll").disabled = $("emptyBin").disabled = !bin.names.length;
  setText($("binNote"), `Synced photos you delete are kept off the frame and stay here for ${bin.days} days, in case you change your mind. Restore one to let the next sync put it back. After ${bin.days} days, or when you empty the bin, a photo leaves this list for good and still stays off the frame.`);
  setText($("goneInfo"), bin.gone ? `${bin.gone.toLocaleString()} deleted for good` : "");
  show($("forgetGone"), bin.gone > 0);
  setText($("exEmpty"), bin.names.length ? "" : "The bin is empty.");
  const visible = bin.names.slice(0, bin.shown);
  show($("exMore"), bin.names.length > visible.length);
  setText($("exMore"), `Show ${Math.min(BIN_PAGE, bin.names.length - visible.length)} more of ${bin.names.length.toLocaleString()}`);
  if (!changed("bin", visible)) return;
  const list = $("exList"); list.textContent = "";
  visible.forEach((name, i) => {
    const left = Math.max(0, Math.ceil(bin.days - (Date.now() / 1000 - bin.at[i]) / 86400));
    const tile = el("div", "tile gone"); tile.title = `Deleted ${fmtWhen(bin.at[i])} · ${plural(left, "day")} left`;
    const img = image(thumb("frame-sync/" + name));
    // Photos deleted before thumbnails were kept have none until a sync fetches one.
    img.onerror = () => img.replaceWith(el("span", "noimg", "No preview yet. It appears after the next sync."));
    const restoreBtn = el("button", "swap", "Restore"); restoreBtn.onclick = () => restore([name]);
    tile.append(img, restoreBtn); list.append(tile);
  });
}
function eta(p) {
  const left = Math.round((p.total - p.done) / p.rate);
  if (!left) return "";
  return " · about " + (left >= 3600 ? `${Math.floor(left / 3600)} h ${Math.round(left % 3600 / 60)} min` : left >= 60 ? `${Math.round(left / 60)} min` : `${left} s`) + " left";
}
function renderJob(job) {
  if (jobWasRunning && !job.running) { load(true); toast(job.result && job.result.error ? "Sync failed" : "Sync finished"); }
  jobWasRunning = job.running;
  if (!changed("job", job)) return;
  $("syncStart").disabled = job.running;
  setText($("syncStart"), job.running ? "Sync running…" : "Start sync");
  show($("syncLive"), job.running || job.log.length > 0);
  const p = job.progress;
  $("progFill").style.width = p && p.total ? `${Math.round(100 * p.done / p.total)}%` : (job.result && job.result.ok ? "100%" : "0%");
  let text = p ? `${p.done.toLocaleString()} of ${p.total.toLocaleString()}${p.rate ? ` · ${p.rate}/s${eta(p)}` : ""} · ${p.detail}` : (job.running ? "Preparing…" : "");
  if (!job.running && job.result) text = job.result.error ? `Failed: ${job.result.error}` : "Finished.";
  setText($("progText"), text);
  const log = $("log"), atEnd = log.scrollTop + log.clientHeight >= log.scrollHeight - 8;
  show(log, job.log.length > 0);
  setText(log, job.log.join("\n"));
  if (atEnd) log.scrollTop = log.scrollHeight;
}

async function load(wantFull) {
  let s;
  try { s = await api("/api/state" + (wantFull ? "?full=1" : "")); }
  catch (e) { $("dot").className = "dot off"; setText($("status"), "The frame is not responding"); needFull = true; return; }
  $("dot").className = "dot on";
  bin.names = s.excluded; bin.at = s.deleted_at; bin.gone = s.deleted_for_good; bin.days = s.bin_days;
  renderBin(); renderJob(s.job); renderNow(s.now);
  if (document.activeElement !== $("optAuto")) $("optAuto").checked = s.auto;
  pinSet = s.pin; unlocked = s.unlocked; show($("lockBtn"), pinSet && !unlocked);
  if (!wantFull) return;
  needFull = false;
  photos = s.photos; listKey = "";
  const have = new Set(photos.map(p => p[0]));
  selected = new Set([...selected].filter(rel => have.has(rel)));
  setText($("status"), `${photos.length.toLocaleString()} photos` + (s.disk ? ` · ${fmtBytes(s.disk[1])} free` : ""));
  if (document.activeElement !== $("sources") && changed("sources", s.sources)) $("sources").value = s.sources.join("\n");
  render();
}

// --- changing things -----------------------------------------------------------------------------
async function del(rels) {
  const phone = rels.filter(rel => kindOf(rel) === "phone").length;
  let question = rels.length === 1 ? "Delete this photo from the frame?" : `Delete ${rels.length.toLocaleString()} photos from the frame?`;
  if (phone) question += `\n\n${phone === rels.length ? "These were" : phone + " of them were"} sent from the phone app. They will be deleted from the frame only, and will still be listed in the phone app.`;
  if (!confirm(question)) return false;
  try { await api("/api/delete", { paths: rels }); } catch (e) { toast(e.message); return false; }
  const gone = new Set(rels);
  photos = photos.filter(p => !gone.has(p[0])); rels.forEach(rel => selected.delete(rel)); listKey = "";
  setText($("status"), $("status").textContent.replace(/^[\d,.\s]+ photos/, `${photos.length.toLocaleString()} photos`));
  render(); load(false);
  toast(rels.length === 1 ? "Deleted from the frame." : `Deleted ${rels.length.toLocaleString()} photos from the frame.`);
  return true;
}
async function restore(names) {
  try { await api("/api/restore", { names }); } catch (e) { return toast(e.message); }
  await load(false); toast("Restored. Run a sync to put them back on the frame.");
}
// Resize to what the frame can show before uploading: far less to send, and no work for the frame.
async function shrink(file) {
  try {
    const bitmap = await createImageBitmap(file);
    const scale = Math.min(1, MAX_SIDE / Math.max(bitmap.width, bitmap.height));
    const canvas = el("canvas");
    canvas.width = Math.round(bitmap.width * scale); canvas.height = Math.round(bitmap.height * scale);
    canvas.getContext("2d").drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    bitmap.close();
    return await new Promise((ok, fail) => canvas.toBlob(b => b ? ok(b) : fail(new Error("encode failed")), "image/jpeg", 0.88));
  } catch (e) { return file; }  // this browser cannot read it; let the frame try
}
async function addFiles(files) {
  files = [...files].filter(f => f.size);
  if (!files.length) return;
  if (adding) return toast("Still adding the previous photos.");
  if (pinSet && !unlocked && !(await unlock())) return;
  adding = true; show($("adding"), true); showTab("photos");
  let done = 0; const failed = [];
  const step = () => { setText($("addText"), `Adding photos to the frame: ${done} of ${files.length}`); $("addFill").style.width = `${Math.round(100 * done / files.length)}%`; };
  step();
  const queue = files.slice();
  const worker = async () => {
    for (let f; (f = queue.shift()); ) {
      try {
        const headers = authHeaders({ "X-Filename": encodeURIComponent(f.name), "X-Taken": Math.floor(f.lastModified / 1000) });
        const r = await fetch("/api/add", { method: "POST", headers, body: await shrink(f) });
        if (!r.ok) throw new Error((await r.json()).error || r.statusText);
      } catch (e) { failed.push(e.message); }
      done++; step();
    }
  };
  await Promise.all([worker(), worker(), worker()]);
  adding = false; show($("adding"), false);
  filter = "all"; sortBy = "added"; $("sort").value = sortBy; page = 0; await load(true);
  const ok = files.length - failed.length;
  toast(failed.length ? `Added ${ok}, ${failed.length} failed: ${failed[0]}` : `Added ${plural(ok, "photo")} to the frame.`);
}

// --- the viewer: the photo list, or the photos of one duplicate group -------------------------------
const lbRels = () => viewer ? viewer.rels : shown().map(p => p[0]);
function describe(g, i) {
  const q = g.quality[i], p = g.people[i];
  let text = q ? `Sharpness ${q[0].toLocaleString()} · ${(q[1] / 10).toFixed(1)}% blown out or black` : "Not measured yet";
  if (p) text = (p[0] ? `${plural(p[0], "face")} · eyes open ${p[1]}% · smiling ${p[2]}% · ` : "No faces found · ") + text;
  if (i === g.suggested) text += " · suggested";
  return text;
}
function openLightbox(i) {
  const rels = lbRels(); lb = i;
  $("lbImg").src = full(rels[i]);
  const info = $("lbInfo"); info.textContent = `${i + 1} / ${rels.length}`;
  show($("lbKeep"), !!viewer); show($("lbDelete"), !viewer);
  // In the photo list a photo can be made a favourite, or hidden from the frame without deleting it.
  const mark = viewer ? null : markOf(rels[i]);
  show($("lbFav"), !viewer); show($("lbHide"), !viewer);
  setText($("lbFav"), mark === "favourite" ? "★ Favourite" : "☆ Favourite"); $("lbFav").classList.toggle("primary", mark === "favourite");
  setText($("lbHide"), mark === "hidden" ? "Show on the frame again" : "Hide from the frame");
  if (mark === "hidden") info.textContent += " · hidden from the frame";
  if (viewer) {
    const keeping = i === viewer.keep;
    info.textContent += ` · ${describe(viewer, i)}`;
    if (keeping) info.append(el("b", "", " · Keeping"));
    $("lbKeep").disabled = keeping; setText($("lbKeep"), keeping ? "Keeping this one" : "Keep this one");
  }
  $("lightbox").classList.add("open");
}
const markOf = rel => (photos.find(p => p[0] === rel) || [])[4] || null;
/// Make the photo in the viewer a favourite, hide it, or neither; the viewer stays on it if it is still listed.
async function markPhoto(mark) {
  const i = lb, rel = lbRels()[i];
  try { await api("/api/mark", { photo: rel, mark }); } catch (e) { return toast(e.message); }
  const photo = photos.find(p => p[0] === rel);
  if (photo) photo[4] = mark;
  listKey = ""; render();
  const rels = lbRels(), still = rels.indexOf(rel);
  if (!rels.length) closeLightbox(); else openLightbox(still >= 0 ? still : Math.min(i, rels.length - 1));
  toast(mark === "hidden" ? "Hidden. The frame leaves it out within ten minutes." : mark === "favourite" ? "A favourite: it comes round more often." : "Back to an ordinary photo.");
}
function closeLightbox() { lb = -1; viewer = null; $("lightbox").classList.remove("open"); $("lbImg").removeAttribute("src"); }
function stepLightbox(d) { const n = lbRels().length; if (lb < 0 || !n) return; openLightbox((lb + d + n) % n); }

// --- duplicates ----------------------------------------------------------------------------------------
// A group is drawn once; afterwards its tiles are reordered and relabelled in place, so photos
// do not reload when the frame learns more about them.
function groupNote(g) {
  if (g.checking) return "checking eyes and smiles, a few seconds per photo…";
  if (g.suggested < 0) return "still being measured";
  const basis = g.by === "expressions" ? "suggested for open eyes and smiles, then sharpness"
    : g.people.some(x => !x) ? "suggested for sharpness so far; faces are checked in the background" : "suggested for sharpness and exposure";
  return basis + (g.keep !== g.suggested ? " · you changed it" : "");
}
function renderGroup(g) {
  const info = new Map(photos.map(p => [p[0], p]));
  if (!g.tiles) {
    g.tiles = new Map();
    g.label = el("span", "note"); g.check = el("button", "btn small"); g.remove = el("button", "btn small danger");
    const compare = el("button", "btn small", "View and compare");
    compare.onclick = () => { viewer = g; openLightbox(g.keep); checkPeople(g); };
    g.check.onclick = () => checkPeople(g);
    g.remove.onclick = async () => {
      if (await del(g.rels.filter((_, i) => i !== g.keep))) { g.el.remove(); dupGroups = dupGroups.filter(x => x !== g); renderDupSummary(); }
    };
    const bar = el("div", "bar"); bar.append(g.label, el("span", "spacer"), g.check, compare, g.remove);
    g.grid = el("div", "grid");
    g.el.append(bar, g.grid);
  }
  setText(g.label, `${g.rels.length} photos · ${fmtWhen(info.get(g.rels[0])[2])} · ${groupNote(g)}`);
  setText(g.check, g.checking ? "Checking…" : "Check eyes and smiles"); g.check.disabled = !!g.checking;
  show(g.check, g.people.some(x => !x));
  setText(g.remove, `Delete the other ${g.rels.length - 1}`);
  g.rels.forEach((rel, i) => {
    let tile = g.tiles.get(rel);
    if (!tile) {
      const [, kind, taken] = info.get(rel);
      tile = makeTile(rel, kind, taken);
      tile.state = el("span", "tag state"); tile.also = el("span", "tag also", "Suggested");
      tile.swap = el("button", "swap", "Keep this one");
      tile.append(tile.state, tile.also, tile.swap);
      g.tiles.set(rel, tile);
    }
    const keeping = i === g.keep;
    tile.title = describe(g, i);
    tile.classList.toggle("keep", keeping);
    show(tile.state, keeping); setText(tile.state, "Keeping");
    show(tile.also, !keeping && i === g.suggested);
    tile.onclick = () => { viewer = g; openLightbox(g.rels.indexOf(rel)); checkPeople(g); };
    tile.swap.onclick = e => { e.stopPropagation(); g.keep = g.rels.indexOf(rel); renderGroup(g); };
    if (g.grid.children[i] !== tile) g.grid.insertBefore(tile, g.grid.children[i] || null);  // moving keeps the image loaded
  });
}
// Apply what the frame now knows about a group, keeping your own choice of photo if you made one.
function applyRanking(g, found) {
  if (JSON.stringify([found.photos, found.people, found.quality]) === JSON.stringify([g.rels, g.people, g.quality])) return false;
  const kept = g.rels[g.keep], choseOwn = g.suggested >= 0 && g.keep !== g.suggested, viewing = viewer === g && lb >= 0 ? g.rels[lb] : null;
  g.rels = found.photos; g.quality = found.quality; g.people = found.people; g.by = found.by;
  g.suggested = found.ranked ? 0 : -1;
  g.keep = choseOwn ? Math.max(0, g.rels.indexOf(kept)) : 0;
  renderGroup(g);
  if (viewing) openLightbox(Math.max(0, g.rels.indexOf(viewing)));
  return true;
}
// Have the frame look at the faces in one group right away (a few seconds per photo).
async function checkPeople(g) {
  if (g.checking || g.people.every(x => x)) return;
  g.checking = true; renderGroup(g);
  let r;
  try { r = await api("/api/analyse", { paths: g.rels }); } catch (e) { g.checking = false; renderGroup(g); return toast(e.message); }
  g.checking = false;
  if (!applyRanking(g, r.group)) renderGroup(g);
  if (r.failed) toast(`${plural(r.failed, "photo")} could not be checked.`);
}
function renderDupSummary(found, pending) {
  if (found !== undefined) dupTotals = [found, pending];
  const [total, waiting] = dupTotals;
  const extra = dupGroups.reduce((n, g) => n + g.rels.length - 1, 0);
  let text = dupGroups.length ? `${plural(dupGroups.length, "group")}, ${plural(extra, "photo")} you could remove` : "No look-alikes found at this setting.";
  if (total > dupGroups.length && dupGroups.length) text += ` (the largest of ${total.toLocaleString()})`;
  if (waiting) text += ` · ${plural(waiting, "photo")} still being measured`;
  setText($("dupInfo"), text);
  show($("dupDeleteAll"), dupGroups.length > 0);
}
// Tell the frame which groups are on screen so it looks at those faces first, and pick up
// whatever it has worked out since last time.
const onScreen = new IntersectionObserver(entries => { for (const e of entries) { const g = dupGroups.find(x => x.el === e.target); if (g) g.visible = e.isIntersecting; } }, { rootMargin: "200px" });
async function reportWatching() {
  if ($("tab-duplicates").classList.contains("hidden") || document.hidden) return;
  const waiting = dupGroups.filter(g => (g.visible || g === viewer) && !g.checking && g.people.some(x => !x));
  waiting.sort((a, b) => (b === viewer) - (a === viewer));
  const asked = waiting.slice(0, 12);
  if (!asked.length) return;
  let r;
  try { r = await api("/api/watching", { groups: asked.map(g => g.rels) }); } catch (e) { return; }
  r.groups.forEach((found, i) => { if (!asked[i].checking) applyRanking(asked[i], found); });
}
async function findDuplicates() {
  setText($("dupInfo"), "Looking…");
  if (!photos.length) await load(true);
  let r;
  try { r = await api("/api/duplicates?d=" + $("dupLevel").value); } catch (e) { setText($("dupInfo"), e.message); return; }
  const box = $("dupGroups"); box.textContent = ""; onScreen.disconnect();
  const have = new Set(photos.map(p => p[0]));
  dupGroups = [];
  for (const found of r.groups) {
    // The frame lists the best photo first; leave out any photo deleted since.
    const kept = found.photos.map((rel, i) => [rel, found.quality[i], found.people[i]]).filter(([rel]) => have.has(rel));
    if (kept.length < 2) continue;
    const g = { rels: kept.map(x => x[0]), quality: kept.map(x => x[1]), people: kept.map(x => x[2]), by: found.by, suggested: found.ranked ? 0 : -1, keep: 0, el: el("div", "card group") };
    renderGroup(g); box.append(g.el); dupGroups.push(g); onScreen.observe(g.el);
  }
  renderDupSummary(r.found, r.pending);
}

// --- calendar ---------------------------------------------------------------------------------------------
// A time from the frame: a number is a moment, text is a day or a time on the clock here.
function calWhen(value) {
  if (typeof value === "number") return new Date(value * 1000);
  const n = value.split(/[-T:]/).map(Number);
  return new Date(n[0], n[1] - 1, n[2], n[3] || 0, n[4] || 0, n[5] || 0);
}
const calDay = date => date.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" });
const calTime = date => date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
const REPEATS = { never: "", weekly: "every week", monthly: "every month", yearly: "every year" };
function calRow(when, what, from, button) {
  const row = el("div"); row.append(el("span", "when", when), el("span", "what", what));
  if (from) row.append(el("span", "from", from));
  if (button) row.append(button);
  return row;
}
async function loadCalendar() {
  let c;
  try { c = await api("/api/calendar"); } catch (e) { return; }
  // The links are secrets: with a PIN set, the frame only hands them to an unlocked browser.
  const links = $("calLinks"), kept = c.feeds.length > 0 && !c.unlocked;
  links.disabled = kept;
  links.placeholder = kept ? "Unlock (top of the page) to see or change the calendar links" : "https://calendar.google.com/calendar/…";
  if (document.activeElement !== links && changed("calLinks", kept ? null : c.feeds.map(f => f.link))) links.value = kept ? "" : c.feeds.map(f => f.link).join("\n");
  if (changed("calFeeds", c.feeds.map(f => [f.host, f.name, f.events, f.fetched, f.error]))) {
    const box = $("calFeeds"); box.textContent = "";
    for (const f of c.feeds) {
      const state = f.error ? `Not updated: ${f.error}` : f.fetched ? `updated ${fmtWhen(f.fetched)}` : "fetching…";
      const row = calRow(plural(f.events, "event"), f.name || f.host, f.name ? f.host : "", el("span", f.error ? "from bad" : "from", state));
      box.append(row);
    }
  }
  if (changed("calAdded", c.added)) {
    const box = $("calAdded"); box.textContent = "";
    for (const e of c.added) {
      const day = calWhen(e.day).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
      const remove = el("button", "btn small danger", "Delete");
      remove.onclick = async () => {
        if (!confirm(`Delete "${e.title}" from the frame's calendar?`)) return;
        try { await api("/api/events/delete", { id: e.id }); toast("Event deleted."); loadCalendar(); } catch (err) { toast(err.message); }
      };
      box.append(calRow(e.time ? `${day} · ${e.time}` : day, e.title, REPEATS[e.repeat], remove));
    }
  }
  rules = c.reminders || [];
  renderRules(c.events);
  loadRemote();  // for the stations a rule can play
  travel = c.travel || null; ways = c.ways || []; tripEvents = c.events;
  renderTrips(); if (travel) { planTrips(); findPlaces(); }
  const now = new Date(), today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const coming = c.events.filter(e => e.all_day ? calWhen(e.end) > today : calWhen(e.end) >= now).slice(0, 40);
  setText($("calNone"), coming.length ? "" : "Nothing in the next five weeks.");
  if (changed("calComing", coming)) {
    const box = $("calComing"); box.textContent = "";
    for (const e of coming) {
      const start = calWhen(e.start);
      box.append(calRow(e.all_day ? calDay(start) : `${calDay(start)} · ${calTime(start)}`, e.place ? `${e.title} (${e.place})` : e.title, e.added ? "added here" : e.calendar || ""));
    }
  }
}
// --- reminders: rules that turn calendar events into notes on the frame ---------------------------
let rules = [];
const AHEAD = { m0: "when it starts", m15: "15 minutes before", m30: "30 minutes before", m60: "an hour before", m120: "2 hours before", d0: "on the day at", d1: "the day before at", d2: "2 days before at", d7: "a week before at" };
const aheadText = when => typeof when.minutes === "number" ? (AHEAD["m" + when.minutes] || (when.minutes % 60 ? `${when.minutes} minutes before` : `${when.minutes / 60} hours before`)) : `${AHEAD["d" + when.days] || `${when.days} days before at`} ${when.at}`;
/// When a rule's reminder for an event comes up (the frame's screen works it out the same way).
function dueOf(event, when) {
  const start = calWhen(event.start);
  if (typeof when.minutes === "number") return new Date(start.getTime() - when.minutes * 60000);
  const [hour, minute] = when.at.split(":").map(Number);
  return new Date(start.getFullYear(), start.getMonth(), start.getDate() - when.days, hour, minute);
}
function renderRules(events) {
  const now = new Date();
  const next = rules.map((rule, i) => events.filter(e => (e.remind || []).includes(i)).filter(e => calWhen(e.end) > now || calWhen(e.start) > now).map(e => [dueOf(e, rule.when), e])[0]);
  if (!changed("rules", [rules, next.map(n => n && [n[0].getTime(), n[1].title])])) return;
  const box = $("rmRules"); box.textContent = "";
  rules.forEach((rule, i) => {
    const remove = el("button", "btn small danger", "Delete");
    remove.onclick = () => saveRules(rules.filter((_, k) => k !== i), "Rule deleted.");
    const coming = !next[i] ? "no event in the next five weeks has these words" : next[i][0] <= now ? `on the frame now, for “${next[i][1].title}”` : `next: ${calDay(next[i][0])} ${calTime(next[i][0])}, for “${next[i][1].title}”`;
    const row = calRow(aheadText(rule.when) + (rule.chime ? " · chime" : ""), (rule.say ? `${rule.words} → “${rule.say}”` : rule.words) + (rule.then ? `, ${thenText(rule.then)}` : ""), coming, remove);
    box.append(row);
  });
}
async function saveRules(wanted, done) {
  try { await api("/api/reminders", { rules: wanted }); toast(done); } catch (e) { toast(e.message); return false; }
  loadCalendar();
  return true;
}
async function addRule() {
  const kind = $("rmWhen").value, amount = Number(kind.slice(1));
  const when = kind[0] === "m" ? { minutes: amount } : { days: amount, at: $("rmAt").value || "19:00" };
  const rule = { words: $("rmWords").value, when, say: $("rmSay").value.trim() || null, chime: $("rmChime").checked, then: $("rmThen").value ? JSON.parse($("rmThen").value) : null };
  if (!rule.words.trim()) { toast("Say which words to look for in the names of events."); $("rmWords").focus(); return; }
  if (await saveRules(rules.concat([rule]), "Rule added. The frame picks it up within half a minute.")) { $("rmWords").value = ""; $("rmSay").value = ""; }
}
// --- things to do: people, tasks, points and rewards -------------------------------------------
const DAY_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
let todo = null;
const todayText = () => { const d = new Date(); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; };
const nameOf = id => { const p = todo.people.find(p => p.id === id); return p ? p.name : "Anyone"; };
const peopleKept = people => people.map(p => ({ id: p.id, name: p.name, goal: p.goal }));
function repeatText(repeat) {
  if ("once" in repeat) return repeat.once ? `once, on ${calWhen(repeat.once).toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" })}` : "once";
  if (repeat.days.length === 7) return "every day";
  if (repeat.days.join() === "1,2,3,4,5") return "Monday to Friday";
  return repeat.days.map(d => DAY_NAMES[d - 1]).join(", ");
}
async function loadTodo() {
  try { todo = await api(`/api/todo?day=${todayText()}`); } catch (e) { return; }
  if (changed("tdWho", todo.people.map(p => [p.id, p.name]))) {
    for (const who of [$("tdWho"), $("tdRuleWho")]) {
      const was = who.value; who.textContent = "";
      who.append(new Option("Anyone", ""), ...todo.people.map(p => new Option(p.name, p.id)));
      if ([...who.options].some(o => o.value === was)) who.value = was;
    }
  }
  if (changed("tdRules", [todo.rules, todo.today, todo.people])) {
    const box = $("tdRules"); box.textContent = "";
    const fromCalendar = todo.today.filter(t => t.event);
    todo.rules.forEach((rule, i) => {
      const remove = el("button", "btn small danger", "Delete");
      remove.onclick = () => saveTodo("/api/todo/rules", { rules: todo.rules.filter(r => r.id !== rule.id) }, "Rule deleted.");
      // The first rule that fits an event is the one that counts for it.
      const words = rule.words.toLowerCase().split(", ");
      const mine = fromCalendar.filter(t => t.who === rule.who && t.points === rule.points && words.some(w => t.title.toLowerCase().includes(w)));
      const today = mine.length ? `today: ${mine.map(t => t.time ? `${t.title} ${t.time}` : t.title).join(", ")}` : "nothing today";
      box.append(calRow(`${nameOf(rule.who)} · ${rule.points} pts`, rule.words, today, remove));
    });
  }
  setText($("tdNoAwards"), !todo.people.length ? "Add a person below first." : todo.awards.length ? "" : "No awards yet. Add some below, for example Shared a toy or Kind words.");
  if (changed("tdGive", [todo.people, todo.awards])) {
    const box = $("tdGive"); box.textContent = "";
    if (todo.awards.length) for (const person of todo.people) {
      const row = el("div", "give"); row.append(el("span", "who", person.name));
      for (const award of todo.awards) {
        const button = el("button", "btn small primary", `${award.title}${award.points ? ` +${award.points}` : ""}`);
        button.onclick = () => saveTodo("/api/todo/award", { who: person.id, award: award.id }, `${award.title}: ${person.name} is cheered on the frame.`);
        row.append(button);
      }
      box.append(row);
    }
  }
  if (changed("tdAwards", todo.awards)) {
    const box = $("tdAwards"); box.textContent = "";
    for (const award of todo.awards) {
      const remove = el("button", "btn small danger", "Delete");
      remove.onclick = () => saveTodo("/api/todo/awards", { awards: todo.awards.filter(a => a.id !== award.id) }, "Award deleted.");
      box.append(calRow(`★ ${award.points}`, award.title, "", remove));
    }
  }
  if (changed("tdPeople", [todo.people, todo.rewards])) {
    const box = $("tdPeople"); box.textContent = "";
    for (const person of todo.people) {
      const give = el("button", "btn small", "Give or take points"), remove = el("button", "btn small danger", "Remove");
      // The reward they are saving for: the cheapest unless one is chosen.
      const goal = el("select"); goal.setAttribute("aria-label", `What ${person.name} is saving for`);
      goal.append(new Option("Saving for the cheapest reward", ""), ...todo.rewards.map(r => new Option(`Saving for ${r.title} (★ ${r.cost})`, r.id)));
      goal.value = person.goal || "";
      goal.onchange = () => saveTodo("/api/todo/people", { people: peopleKept(todo.people.map(p => p.id === person.id ? { ...p, goal: goal.value || null } : p)) }, "Saved.");
      give.onclick = async () => {
        const amount = Number(prompt(`Points to give ${person.name} (a number below zero takes points away):`, "5"));
        if (!Number.isInteger(amount) || !amount) return;
        const why = prompt("What for?", amount > 0 ? "Well done" : "");
        if (why === null) return;
        saveTodo("/api/todo/points", { who: person.id, change: amount, why: why.trim() || (amount > 0 ? "Points given" : "Points taken") }, "Done.");
      };
      remove.onclick = () => { if (confirm(`Remove ${person.name}? Their ${person.points} points go too, and their tasks become anyone's.`)) saveTodo("/api/todo/people", { people: peopleKept(todo.people.filter(p => p.id !== person.id)) }, "Removed."); };
      box.append(calRow(`★ ${person.points}`, person.name, "", give)); box.lastChild.append(remove);
      if (todo.rewards.length) box.lastChild.append(goal);
    }
  }
  if (document.activeElement !== $("tdApproval")) $("tdApproval").checked = todo.approval;
  setText($("tdNoWaiting"), todo.waiting.length ? "" : todo.approval ? "Nothing is waiting. Tasks ticked on the frame are listed here to be approved." : "Points are added as soon as a task is ticked.");
  if (changed("tdWaiting", todo.waiting)) {
    const box = $("tdWaiting"); box.textContent = "";
    for (const request of todo.waiting) {
      const yes = el("button", "btn small primary", "Done: add the points"), no = el("button", "btn small danger", "Not done");
      yes.onclick = () => saveTodo("/api/todo/approve", { task: request.task, day: request.day, approve: true }, `★ ${request.points} added for ${request.name}.`);
      no.onclick = () => saveTodo("/api/todo/approve", { task: request.task, day: request.day, approve: false }, "Turned down. The task is to do again.");
      const day = request.day === todo.day ? "today" : calWhen(request.day).toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" });
      box.append(calRow(`★ ${request.points}`, `${request.name}: ${request.title}`, day, yes)); box.lastChild.append(no);
    }
  }
  if (changed("tdTasks", [todo.tasks, todo.today, todo.people])) {
    const box = $("tdTasks"); box.textContent = "";
    for (const task of todo.tasks) {
      const today = todo.today.find(t => t.id === task.id);
      const state = !today ? "" : today.waiting ? `ticked by ${nameOf(today.by)}, waiting for approval` : today.done ? `done today by ${nameOf(today.by)}` : today.late ? "late" : "to do today";
      const remove = el("button", "btn small danger", "Delete");
      remove.onclick = () => { if (confirm(`Delete "${task.title}"?`)) saveTodo("/api/todo/tasks", { tasks: todo.tasks.filter(t => t.id !== task.id) }, "Task deleted."); };
      box.append(calRow(`${nameOf(task.who)} · ${task.points} pts`, task.title, `${repeatText(task.repeat)}${state ? ` · ${state}` : ""}`, remove));
    }
    if (!todo.tasks.length) box.append(el("p", "muted", "No tasks yet."));
  }
  if (changed("tdRewards", todo.rewards)) {
    const box = $("tdRewards"); box.textContent = "";
    for (const reward of todo.rewards) {
      const remove = el("button", "btn small danger", "Delete");
      remove.onclick = () => saveTodo("/api/todo/rewards", { rewards: todo.rewards.filter(r => r.id !== reward.id) }, "Reward deleted.");
      box.append(calRow(`★ ${reward.cost}`, reward.title, "", remove));
    }
  }
  setText($("tdNoHistory"), todo.history.length ? "" : "Nothing yet. Points earned, spent, given and taken are listed here.");
  if (changed("tdHistory", todo.history)) {
    const box = $("tdHistory"); box.textContent = "";
    for (const entry of todo.history) box.append(calRow(fmtWhen(entry.at), `${entry.who}: ${entry.what}`, `${entry.change > 0 ? "+" : ""}${entry.change}`, null));
  }
}
async function saveTodo(path, body, done) {
  try { await api(path, body); if (done) toast(done); } catch (e) { toast(e.message); return false; }
  await loadTodo();
  return true;
}
async function addPerson() {
  const name = $("tdName").value.trim();
  if (!name) { $("tdName").focus(); return; }
  if (!todo) await loadTodo();
  if (await saveTodo("/api/todo/people", { people: peopleKept(todo.people).concat([{ name }]) }, `${name} added.`)) $("tdName").value = "";
}
async function addTask() {
  const title = $("tdTitle").value.trim(), kind = $("tdRepeat").value;
  if (!title) { toast("Say what is to be done."); $("tdTitle").focus(); return; }
  const chosen = [...$("tdDays").querySelectorAll("input")].filter(i => i.checked).map(i => Number(i.value));
  if (kind === "days" && !chosen.length) { toast("Choose at least one day of the week."); return; }
  if (kind === "day" && !$("tdDay").value) { toast("Choose the day."); $("tdDay").focus(); return; }
  const repeat = kind === "once" ? { once: null } : kind === "day" ? { once: $("tdDay").value } : { days: kind === "daily" ? [1, 2, 3, 4, 5, 6, 7] : kind === "weekdays" ? [1, 2, 3, 4, 5] : chosen };
  const task = { title, who: $("tdWho").value || null, points: Number($("tdPoints").value) || 0, repeat };
  if (await saveTodo("/api/todo/tasks", { tasks: todo.tasks.concat([task]) }, "Task added. It is on the frame within half a minute.")) $("tdTitle").value = "";
}
async function addEventRule() {
  const words = $("tdWords").value.trim();
  if (!words) { toast("Say which words to look for in the names of events."); $("tdWords").focus(); return; }
  const rule = { words, who: $("tdRuleWho").value || null, points: Number($("tdRulePoints").value) || 0 };
  if (await saveTodo("/api/todo/rules", { rules: todo.rules.concat([rule]) }, "Rule added. Matching events are tasks on their day.")) $("tdWords").value = "";
}
async function addAward() {
  const title = $("tdAward").value.trim();
  if (!title) { $("tdAward").focus(); return; }
  if (!todo) await loadTodo();
  if (await saveTodo("/api/todo/awards", { awards: todo.awards.concat([{ title, points: Number($("tdAwardPoints").value) || 0 }]) }, "Award added.")) $("tdAward").value = "";
}
async function addReward() {
  const title = $("tdReward").value.trim();
  if (!title) { $("tdReward").focus(); return; }
  if (await saveTodo("/api/todo/rewards", { rewards: todo.rewards.concat([{ title, cost: Number($("tdCost").value) || 0 }]) }, "Reward added.")) $("tdReward").value = "";
}

// --- getting there: how the events that are travelled to are reached ------------------------------
const WAYS = { transit: "public transport", walk: "on foot", cycle: "by bicycle", car: "by car", park: "by car to a station, then public transport" };
let travel = null, ways = [], tripPlans = {}, editing = null, tripEvents = [], placesFound = {};
/// Where a typed place was found, asked once for each; an address can be found somewhere other than was meant.
async function findPlaces() {
  const typed = [travel.home].concat(travel.trips.flatMap(t => [t.to, t.from, t.via])).filter(Boolean);
  for (const place of new Set(typed)) {
    if (placesFound[place]) continue;
    try { placesFound[place] = (await api(`/api/travel/find?place=${encodeURIComponent(place)}`)).name || "found"; } catch (e) { placesFound[place] = `not found (${e.message})`; }
    renderTrips();
  }
}
const foundAs = place => placesFound[place] ? ` (found: ${placesFound[place]})` : "";
const tripKey = t => `${t.title}\n${t.place || ""}`;
const inTrips = () => $("trTrips").contains(document.activeElement) && document.activeElement.tagName !== "BUTTON";
/// Ask when to leave for the next time each trip's event happens.
async function planTrips() {
  for (const { trip, next } of tripRows()) {
    if (trip < 0 || !next) continue;
    const arrive = Math.floor(calWhen(next.start).getTime() / 1000), key = `${JSON.stringify(travel.trips[trip])}|${travel.home}|${arrive}`;
    // A plan is asked for once; a failure is asked about again after a minute, as it may pass.
    if (tripPlans[key] && !(tripPlans[key].error && Date.now() - tripPlans[key].at > 60000)) continue;
    try { tripPlans[key] = await api(`/api/travel?trip=${trip}&arrive=${arrive}`); } catch (e) { tripPlans[key] = { error: e.message, at: Date.now() }; }
    renderTrips();
  }
}
/// One row for each kind of event that is, or could be, travelled to: those coming up with a time, and the trips set up.
function tripRows() {
  const rows = new Map(), now = new Date();
  for (const e of tripEvents) {
    if (e.all_day || calWhen(e.start) <= now) continue;
    const key = tripKey(e);
    if (!rows.has(key)) rows.set(key, { key, title: e.title, place: e.place || null, next: e, trip: -1 });
  }
  (travel ? travel.trips : []).forEach((t, i) => {
    const key = tripKey(t);
    if (!rows.has(key)) rows.set(key, { key, title: t.title, place: t.place || null, next: null, trip: i });
    rows.get(key).trip = i;
  });
  // The ones set up first, then those with a location, which are the likeliest to be travelled to.
  return [...rows.values()].sort((a, b) => (b.trip >= 0) - (a.trip >= 0) || !!b.place - !!a.place).slice(0, 40);
}
function tripForm(row) {
  const trip = row.trip >= 0 ? travel.trips[row.trip] : null, form = el("div", "bar tripform");
  const field = (value, hint, label) => { const input = el("input"); input.type = "text"; input.maxLength = 120; input.value = value || ""; input.placeholder = hint; input.setAttribute("aria-label", label); return input; };
  const to = field(trip ? trip.to : row.place, "Where it is: postcode or address", "Where it is");
  const from = field(trip && trip.from, "From home", "Where the trip starts");
  const via = field(trip && trip.via, "Station to drive to", "Station to drive to");
  const how = el("select"); how.setAttribute("aria-label", "How you get there");
  for (const way of ways) how.append(new Option(WAYS[way], way));
  if (trip && ways.includes(trip.how)) how.value = trip.how;
  const margin = el("input"); margin.type = "number"; margin.min = 0; margin.max = 120; margin.value = trip ? trip.margin : 10; margin.title = "Minutes to spare"; margin.setAttribute("aria-label", "Minutes to spare");
  const showVia = () => show(via, how.value === "park"); how.onchange = showVia; showVia();
  const save = el("button", "btn primary", "Save"), cancel = el("button", "btn", "Cancel");
  cancel.onclick = () => { editing = null; drawn.delete("trips"); renderTrips(); };
  save.onclick = async () => {
    if (!to.value.trim()) { toast("Say where it is."); to.focus(); return; }
    const made = { title: row.title, place: row.place, to: to.value, from: from.value.trim() || null, how: how.value, via: how.value === "park" ? via.value : null, margin: Number(margin.value) || 0 };
    const trips = travel.trips.filter(t => tripKey(t) !== row.key).concat([made]);
    if (await saveTravel(travel.home, trips, "Saved. The frame will say when to leave.")) editing = null;
  };
  form.append(to, how, via, from, margin, el("span", "muted", "min to spare"), save, cancel);
  return form;
}
function renderTrips() {
  const rows = tripRows(), locked = !travel;
  setText($("trWays"), locked ? "Unlock (top of the page) to see or change where home is and the trips." : ways.includes("car") ? "A key for the driving service is set, so trips can also be by car, or by car to a station and on by public transport. Car times allow for usual traffic." : "Public transport, walking and cycling are worked out by Transport for London's journey planner. Trips by car need a key for TomTom's routing service (free for this much use, from developer.tomtom.com); with one typed here, car and car-then-train are offered too. The key is kept on the frame and never shown again.");
  show($("trKeyForget"), !locked && ways.includes("car")); $("trHome").disabled = locked; $("trKey").disabled = locked; $("trKeySave").disabled = locked;
  if (!locked && document.activeElement !== $("trHome") && changed("trHome", travel.home)) $("trHome").value = travel.home || "";
  setText($("trHomeFound"), !locked && travel.home && placesFound[travel.home] ? `Home was found as: ${placesFound[travel.home]}. If that is not your home, type its postcode instead.` : "");
  if (inTrips() || !changed("trips", [travel, ways, rows.map(r => [r.key, r.trip, r.next && r.next.start]), editing, tripPlans, placesFound])) return;
  const box = $("trTrips"); box.textContent = "";
  if (locked) return;
  for (const row of rows) {
    const trip = row.trip >= 0 ? travel.trips[row.trip] : null;
    const when = row.next ? `${calDay(calWhen(row.next.start))} · ${calTime(calWhen(row.next.start))}` : "not coming up";
    const line = calRow(when, row.place ? `${row.title} (${row.place})` : row.title, "", null);
    if (trip) {
      const arrive = row.next ? Math.floor(calWhen(row.next.start).getTime() / 1000) : 0, plan = tripPlans[`${JSON.stringify(trip)}|${travel.home}|${arrive}`];
      let said = `${WAYS[trip.how] || "by car (needs the key)"} to ${trip.to}${foundAs(trip.to)}`;
      if (trip.from) said += `, from ${trip.from}${foundAs(trip.from)}`;
      if (trip.via) said += `, by ${trip.via}${foundAs(trip.via)}`;
      if (plan && plan.error) said += ` · ${plan.error}`;
      else if (plan) said += ` · leave ${calTime(new Date(plan.leave * 1000))}${plan.catch ? `, ${plan.catch}` : ""}`;
      line.append(el("span", plan && plan.error ? "from bad" : "from", said));
      const remove = el("button", "btn small danger", "Remove");
      remove.onclick = () => saveTravel(travel.home, travel.trips.filter(t => tripKey(t) !== row.key), "Trip removed.");
      const change = el("button", "btn small", "Change"); change.onclick = () => { editing = row.key; drawn.delete("trips"); renderTrips(); };
      line.append(change, remove);
    } else {
      const add = el("button", "btn small", "Set up"); add.onclick = () => { editing = row.key; drawn.delete("trips"); renderTrips(); };
      line.append(add);
    }
    box.append(line);
    if (editing === row.key) box.append(tripForm(row));
  }
  if (!rows.length) box.append(el("p", "muted", "No event with a time is coming up in the next five weeks."));
}
async function saveTravel(home, trips, done) {
  try { await api("/api/travel", { home: home || null, trips }); toast(done); } catch (e) { toast(e.message); return false; }
  drawn.delete("trips"); document.activeElement.blur(); await loadCalendar();
  return true;
}
async function saveRoadsKey(key) {
  try { await api("/api/travel/key", { key }); toast(key ? "Key saved. Trips by car are now offered." : "The key is forgotten. Trips by car can no longer be worked out."); } catch (e) { toast(e.message); return; }
  $("trKey").value = ""; tripPlans = {}; loadCalendar();
}
async function addEvent() {
  const title = $("evTitle").value.trim();
  if (!title || !$("evDay").value) return toast("An event needs a title and a date.");
  try { await api("/api/events/add", { title, day: $("evDay").value, time: $("evTime").value || null, repeat: $("evRepeat").value }); }
  catch (e) { return toast(e.message); }
  $("evTitle").value = ""; $("evTime").value = ""; $("evRepeat").value = "never";
  toast("Added to the frame's calendar."); loadCalendar();
}

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
  document.querySelectorAll("nav button").forEach(b => b.classList.toggle("active", b.dataset.tab === tab));
  for (const t of TABS) show($("tab-" + t), t === tab);
  if (tab === "status") loadStatus();
  if (tab === "calendar") loadCalendar();
  if (tab === "frame") loadFrame();
  if (tab === "todo") loadTodo();
  if (tab === "duplicates" && !$("dupGroups").childElementCount) findDuplicates();
}
document.querySelectorAll("nav button").forEach(b => b.onclick = () => { location.hash = b.dataset.tab; });
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

showTab(location.hash.slice(1));
load(true);
setInterval(() => { if (!document.hidden) load(needFull); }, 4000);
setInterval(reportWatching, 5000);
setInterval(() => { if (!$("tab-status").classList.contains("hidden") && !document.hidden) loadStatus(); }, 10000);
setInterval(() => { if (!$("tab-calendar").classList.contains("hidden") && !document.hidden) loadCalendar(); }, 15000);
setInterval(() => { if (!$("tab-frame").classList.contains("hidden") && !document.hidden) loadFrame(); }, 10000);
setInterval(() => { if (!$("tab-todo").classList.contains("hidden") && !document.hidden) loadTodo(); }, 8000);
