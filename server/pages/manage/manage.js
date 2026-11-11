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
