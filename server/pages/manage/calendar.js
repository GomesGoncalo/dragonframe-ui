// The calendar, reminders and pictures for events. Part of the page for managing the frame.
"use strict";

// --- calendar ---------------------------------------------------------------------------------------------
// A time from the frame: a number is a moment, text is a day or a time on the clock here.
function calWhen(value) {
  if (typeof value === "number") return new Date(value * 1000);
  const n = value.split(/[-T:]/).map(Number);
  return new Date(n[0], n[1] - 1, n[2], n[3] || 0, n[4] || 0, n[5] || 0);
}
const calDay = date => date.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" });
const calTime = date => date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
const REPEATS = { never: "", daily: "every day", weekdays: "Monday to Friday", weekly: "every week", monthly: "every month", yearly: "every year" };
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
  eventPictures = c.event_pictures || [];
  renderEventPictures(c.events);
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
// --- pictures for events: what Now and Next shows ---------------------------------------------------
let eventPictures = [];
// Pictures to choose from with a touch; any emoji can be typed too.
const PICTURE_CHOICES = ["🏫", "🍽️", "🥪", "🛁", "🛏️", "📖", "🏊", "⚽", "🎨", "🎵", "🛝", "🚗", "🚌", "🛒", "👵", "👴", "🎂", "🦷", "🩺", "🧸", "📺", "🌳", "🪥", "👕"];
function renderEventPictures(events) {
  const now = new Date();
  // The next event each rule gives its picture to (the first rule that fits is the one that counts).
  const next = eventPictures.map(rule => events.filter(e => e.picture && e.picture.words === rule.words && e.picture.picture === rule.picture).find(e => calWhen(e.end) > now));
  if (!changed("eventPictures", [eventPictures, next.map(e => e && [e.title, e.start])])) return;
  const box = $("pcRules"); box.textContent = "";
  eventPictures.forEach((rule, i) => {
    const remove = el("button", "btn small danger", "Delete");
    remove.onclick = () => saveEventPictures(eventPictures.filter((_, k) => k !== i), "Picture deleted.");
    const start = next[i] && calWhen(next[i].start);
    const coming = !next[i] ? "no event in the next five weeks has these words" : `next: ${calDay(start)}${next[i].all_day ? "" : ` ${calTime(start)}`}, “${next[i].title}”`;
    box.append(calRow(rule.picture, rule.say ? `${rule.words} → “${rule.say}”` : rule.words, coming, remove));
  });
}
async function saveEventPictures(wanted, done) {
  try { await api("/api/event-pictures", { rules: wanted }); toast(done); } catch (e) { toast(e.message); return false; }
  loadCalendar();
  return true;
}
async function addEventPicture() {
  const rule = { words: $("pcWords").value, picture: $("pcPicture").value.trim(), say: $("pcSay").value.trim() || null };
  if (!rule.words.trim()) { toast("Say which words to look for in the names of events."); $("pcWords").focus(); return; }
  if (!rule.picture) { toast("Choose a picture below, or type an emoji."); $("pcPicture").focus(); return; }
  if (await saveEventPictures(eventPictures.concat([rule]), "Picture added. The frame shows it within a few seconds.")) { for (const id of ["pcWords", "pcPicture", "pcSay"]) $(id).value = ""; }
}
for (const picture of PICTURE_CHOICES) {
  const choice = el("button", "", picture);
  choice.type = "button";
  choice.onclick = () => { $("pcPicture").value = picture; };
  $("pcChoices").append(choice);
}
