// Getting there. Part of the page for managing the frame.
"use strict";

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
