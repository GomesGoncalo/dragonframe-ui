// Things to do: people, tasks, points and rewards. Part of the page for managing the frame.
"use strict";

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
