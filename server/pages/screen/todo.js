// The To do tab of the frame's screen: today's tasks for each person, the points they have,
// and the rewards points buy.
//
// Everything is kept by the frame's server, so tasks added from a phone or computer show
// here and what is ticked here is seen there. Ticking needs no PIN: it asks for the task's
// points, which are added when someone with the PIN approves (unless set not to wait).
// Taking a reward takes the PIN too, if one is set. Loaded after screen.js and extras.js, whose helpers it uses.

const TODO_EVERY_MS = 20 * 1000;
// A colour for each person, by their place in the list.
const PERSON_COLOURS = ['#6cb6ff', '#f2a65a', '#7ee0a1', '#e58ad6', '#f2e36b', '#ff8f85', '#9aa5ff', '#5fd4d0'];

let todo = null;        // what the server last said: { day, people, tasks, rewards, today, history }
let todoDrawn = '';
let picking = null;     // what the people are being chosen among for: { task } or { reward }

/// The day it is here, as the server wants it said.
function todayText() {
  const now = new Date(), two = (n) => ('0' + n).slice(-2);
  return now.getFullYear() + '-' + two(now.getMonth() + 1) + '-' + two(now.getDate());
}

async function refreshTodo() {
  try {
    const reply = await fetch('/api/todo?day=' + todayText(), { cache: 'no-store' });
    if (!reply.ok) return;
    todo = await reply.json();
    drawTodo();
  } catch (error) {
    // What was there stays; the next turn asks again.
  }
}

const personById = (id) => (todo.people.filter((person) => person.id === id)[0] || null);
const colourOf = (person) => PERSON_COLOURS[todo.people.indexOf(person) % PERSON_COLOURS.length];

/// Tick a task, or take its tick away, and show what the server makes of it.
async function tickTask(task, who) {
  try {
    await send('/api/todo/done', { task: task.id, day: todo.day, done: !task.done, who: who || null });
    if (!task.done && task.points) {
      const person = personById(task.who || who), name = person ? person.name : '';
      if (todo.approval) say('★ ' + task.points + ' asked for ' + name, 'They are added when a grown-up approves');
      else say('+' + task.points + ' for ' + name, task.title);
    }
  } catch (error) {
    say('That did not work', error.message);
  }
  refreshTodo();
}

function taskRow(task) {
  const row = element('button', 'task' + (task.done ? ' done' : '') + (task.waiting ? ' waiting' : '') + (task.late ? ' late' : ''), '');
  row.appendChild(element('span', 'box', task.waiting ? '…' : task.done ? '✓' : ''));
  // A task that is a calendar event says when in the day it is.
  row.appendChild(element('span', 'what', (task.time ? task.time + '  ' : '') + task.title));
  if (task.event) row.appendChild(element('span', 'tag cal', 'calendar'));
  if (task.late) row.appendChild(element('span', 'tag', 'late'));
  if (task.waiting) row.appendChild(element('span', 'tag wait', 'to approve'));
  if (task.points) row.appendChild(element('span', 'worth', '+' + task.points));
  row.onclick = () => {
    // Somebody's own task is ticked at a touch; anyone's asks who did it.
    if (task.done || task.who) return tickTask(task);
    if (todo.people.length === 1) return tickTask(task, todo.people[0].id);
    pickPerson({ task: task }, 'Who did it?  ·  ' + task.title);
  };
  return row;
}

function drawTodo() {
  if (!todo) return;
  const key = JSON.stringify([todo.day, todo.people, todo.today, todo.rewards, todo.waiting]);
  if (key === todoDrawn) return;
  todoDrawn = key;
  $('todo-day').textContent = new Date().toLocaleDateString(LOCALE, { weekday: 'long', day: 'numeric', month: 'long' });
  const people = $('todo-people');
  people.textContent = '';
  $('todo-none').textContent = todo.people.length ? ''
    : 'Nobody is set up yet. People, tasks and rewards are added from a phone or computer, under To do.';
  // A column for each person: theirs to do, and anyone's that they did.
  const columns = todo.people.map((person) => ({ person: person, tasks: todo.today.filter((task) => (task.who || task.by) === person.id) }));
  const open = todo.today.filter((task) => !task.who && !task.done);
  if (open.length) columns.push({ person: null, tasks: open });
  columns.forEach((column) => {
    const box = element('div', 'person', '');
    const head = element('div', 'head', '');
    head.appendChild(element('span', 'name', column.person ? column.person.name : 'Anyone'));
    if (column.person) {
      head.style.borderColor = colourOf(column.person);
      head.appendChild(element('span', 'points', '★ ' + column.person.points));
    }
    box.appendChild(head);
    const left = column.tasks.filter((task) => !task.done).length;
    column.tasks.forEach((task) => box.appendChild(taskRow(task)));
    if (!column.tasks.length) box.appendChild(element('div', 'none', 'Nothing today'));
    else if (!left) box.appendChild(element('div', 'none', 'All done'));
    people.appendChild(box);
  });
  // The more people, the narrower each column.
  people.className = 'people count' + Math.min(columns.length, 5);
  const rewards = $('todo-rewards');
  rewards.textContent = '';
  todo.rewards.forEach((reward) => {
    const button = element('button', 'press reward', '');
    button.appendChild(element('span', 'what', reward.title));
    button.appendChild(element('span', 'cost', '★ ' + reward.cost));
    button.onclick = () => pickPerson({ reward: reward }, reward.title + '  ·  ★ ' + reward.cost + '  ·  who is it for?');
    rewards.appendChild(button);
  });
  $('todo-rewards-title').textContent = todo.rewards.length ? 'Rewards' : '';
  // Ticks whose points have been asked for: a button for whoever approves them.
  const waiting = (todo.waiting || []).length;
  $('todo-waiting').textContent = waiting ? waiting + ' waiting for approval' : '';
  $('todo-waiting').style.display = waiting ? '' : 'none';
  if (picking && picking.approve) drawApprovals();
}

// --- approving: crediting the points that ticks asked for, after the PIN ------------------

async function approve(request, yes) {
  try {
    await send('/api/todo/approve', { task: request.task, day: request.day, approve: yes });
  } catch (error) {
    if (/PIN/.test(error.message)) {
      token = null;
      pinSet = true;
      return openApprovals();
    }
    say('That did not work', error.message);
  }
  await refreshTodo();
}

function drawApprovals() {
  const box = $('pick-people'), waiting = todo.waiting || [];
  box.textContent = '';
  if (!waiting.length) return closePick();
  waiting.forEach((request) => {
    const row = element('div', 'request', '');
    const day = request.day === todo.day ? '' : '  ·  ' + when(request.day).toLocaleDateString(LOCALE, { weekday: 'short', day: 'numeric', month: 'short' });
    row.appendChild(element('span', 'what', request.name + '  ·  ' + request.title + day));
    row.appendChild(element('span', 'worth', '★ ' + request.points));
    const yes = element('button', 'press on', '✓ Done'), no = element('button', 'press', '✗ Not done');
    yes.onclick = () => approve(request, true);
    no.onclick = () => approve(request, false);
    row.appendChild(yes);
    row.appendChild(no);
    box.appendChild(row);
  });
  if (waiting.length > 1) {
    const all = element('button', 'press on all', 'Approve all ' + waiting.length);
    all.onclick = async () => {
      for (let i = 0; i < waiting.length; i++) await send('/api/todo/approve', { task: waiting[i].task, day: waiting[i].day, approve: true }).catch(() => {});
      refreshTodo();
    };
    box.appendChild(all);
  }
}

function openApprovals() {
  picking = { approve: true };
  clearPickPin();
  const locked = pinSet && !token;
  $('pick-said').textContent = locked ? 'Enter the PIN to approve what was done' : 'Was it done?';
  $('pick').classList.toggle('asking', locked);
  $('pick').classList.add('on');
  if (!locked) drawApprovals();
}
$('todo-waiting').onclick = openApprovals;

// --- choosing a person: who did a task, or who a reward is for ---------------------------

function pickPerson(what, said) {
  picking = what;
  $('pick-said').textContent = said;
  $('pick').classList.remove('asking');
  const box = $('pick-people');
  box.textContent = '';
  todo.people.forEach((person) => {
    const short = what.reward ? what.reward.cost - person.points : 0;
    const button = element('button', 'press', '');
    button.appendChild(element('div', 'name', person.name));
    button.appendChild(element('div', 'has', short > 0 ? '★ ' + person.points + '  ·  ' + short + ' more to go' : '★ ' + person.points));
    button.disabled = short > 0;
    button.style.borderColor = colourOf(person);
    button.onclick = () => personPicked(person);
    box.appendChild(button);
  });
  clearPickPin();
  $('pick').classList.add('on');
}

function closePick() {
  if (picking && (picking.who || picking.approve)) lockSettings();  // what the PIN bought was for this only
  picking = null;
  $('pick').classList.remove('on');
}

async function takeReward() {
  const reward = picking.reward, person = picking.who;
  try {
    await send('/api/todo/redeem', { who: person.id, reward: reward.id });
    say(reward.title + ' for ' + person.name, '★ ' + reward.cost + ' spent');
  } catch (error) {
    if (/PIN/.test(error.message)) {
      token = null;
      pinSet = true;
      return askPickPin();
    }
    say('That did not work', error.message);
  }
  closePick();
  refreshTodo();
}

function askPickPin() {
  $('pick-said').textContent = 'Enter the PIN to give ' + picking.who.name + ' the reward';
  $('pick').classList.add('asking');
}

function personPicked(person) {
  if (picking.task) {
    const task = picking.task;
    closePick();
    return tickTask(task, person.id);
  }
  // A reward spends points, which takes the PIN when there is one.
  picking.who = person;
  if (pinSet && !token) return askPickPin();
  takeReward();
}

const clearPickPin = keypad($('pick-keys'), $('pick-dots'), $('pick-note'), (bought) => {
  token = bought;
  if (picking && picking.approve) openApprovals();
  else takeReward();
});
$('pick-close').onclick = closePick;
$('pick').addEventListener('click', (event) => {
  event.stopPropagation();
  touched();
});

// The list is asked for when its tab is open, when the server says there is news, and when
// the day turns.
todoNews = refreshTodo;
setInterval(() => {
  if (tab === 'todo' || (todo && todo.day !== todayText())) refreshTodo();
  if (picking && tab !== 'todo') closePick();
}, TODO_EVERY_MS);
refreshTodo();
