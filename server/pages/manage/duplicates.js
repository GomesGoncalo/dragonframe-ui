// Duplicate photos. Part of the page for managing the frame.
"use strict";

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
