//! Things to do: who does them, the points they earn, and what points buy.
//!
//! A task belongs to a person (or to whoever gets to it first) and is worth some points.
//! It is done once, on a day or whenever, or comes round on chosen days of the week.
//! Ticking a task on the frame's screen asks for its points for whoever did it. They are
//! credited when the tick is approved, which takes the PIN; or at once, if points are set
//! not to wait for approval. Taking a tick away takes its points back. Points are spent
//! on rewards. Each person saves for one of them: the one chosen for them, or else the
//! cheapest.
//!
//! Points can also be given at once for something done well that is no task: a quick
//! award ("Shared a toy", 1 point) is given from a phone with a touch. Whenever someone is
//! given points, the screen is told to celebrate it (a `Cheer`).
//!
//! Calendar events can be tasks too. A rule names the words to look for in the names of
//! events, whose they are and what they are worth; every event with those words is then a
//! task on its day, ticked and approved like any other. The calendar itself is not ours to
//! read here: whoever asks hands over what is on the calendar that day.
//!
//! Everything is kept in `todo.json` on the frame, so what is added from a phone or
//! computer is on the frame's screen, and what is ticked there is seen everywhere.
//!
//! The server does not know what day it is where the frame hangs (see `ical`): whoever
//! asks says which day they mean.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Datelike;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

use crate::store::{self, Config};
use crate::types::{Cheer, Day, EventId, Goal, PersonName, PointChange, Points, Title, Weekdays, Words};

const MOST_PEOPLE: usize = 12;
const MOST_TASKS: usize = 100;
const MOST_REWARDS: usize = 40;
const MOST_RULES: usize = 20;
const MOST_AWARDS: usize = 40;
/// How much of what has happened is remembered, newest last.
const MOST_HISTORY: usize = 400;
/// A task done once stays in the list, ticked, for the day it was done and is then dropped.
/// Its tick is remembered this many days in case it is taken back.
const KEPT_DAYS: i64 = 2;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TodoError {
    #[error("there is no such person")]
    NoSuchPerson,
    #[error("there is no such task")]
    NoSuchTask,
    #[error("there is no such reward")]
    NoSuchReward,
    #[error("there is no such award")]
    NoSuchAward,
    #[error("say who did it: this task is anyone's")]
    WhoDidIt,
    #[error("not enough points for that yet: {have} of {cost}")]
    NotEnough { have: i64, cost: u32 },
    #[error("there can be at most {0} of those")]
    TooMany(usize),
    #[error("two people cannot have the same name")]
    SameName,
    #[error("nothing is waiting for approval there")]
    NothingWaiting,
}

/// Someone who does tasks and collects points.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: EventId,
    pub name: PersonName,
    /// What they have to spend. It can be below nothing if points were taken away.
    #[serde(default)]
    pub points: i64,
    /// The reward they are saving for; when not chosen, the cheapest.
    #[serde(default)]
    pub goal: Option<EventId>,
}

/// When a task is to be done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    /// Once: on a day, or with no day, whenever.
    Once(Option<Day>),
    /// On these days of every week.
    Days(Weekdays),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: EventId,
    pub title: Title,
    /// Whose it is; nobody's in particular when not given.
    #[serde(default)]
    pub who: Option<EventId>,
    #[serde(default)]
    pub points: Points,
    pub repeat: Repeat,
}

/// A rule that makes tasks of calendar events: those with these words in their names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRule {
    pub id: EventId,
    pub words: Words,
    #[serde(default)]
    pub who: Option<EventId>,
    #[serde(default)]
    pub points: Points,
}

/// Something on the calendar on the day being asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Happening {
    pub title: String,
    /// When in the day, as `HH:MM`; none for something that takes the whole day.
    pub time: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reward {
    pub id: EventId,
    pub title: Title,
    pub cost: Points,
}

/// Points to give at a touch for something done well, to whoever did it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Award {
    pub id: EventId,
    pub title: Title,
    pub points: Points,
}

/// That a task was done on a day, by whom, and what it earned them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Tick {
    task: EventId,
    day: Day,
    who: EventId,
    points: u32,
    /// Whether the points have been credited; until then the tick waits for approval.
    #[serde(default = "yes")]
    paid: bool,
    /// What the task was called when it was ticked.
    #[serde(default)]
    title: Option<String>,
    /// Whether the task was a calendar event, which is no task of ours to look up later.
    #[serde(default)]
    event: bool,
}

fn yes() -> bool {
    true
}

/// Something that happened to someone's points, for looking back on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// Seconds since 1970.
    at: u64,
    who: PersonName,
    what: String,
    /// Points gained, or lost when below nothing.
    change: i64,
}

impl Default for Kept {
    fn default() -> Self {
        Kept {
            people: Vec::new(),
            tasks: Vec::new(),
            rewards: Vec::new(),
            ticks: Vec::new(),
            history: Vec::new(),
            approval: true,
            rules: Vec::new(),
            awards: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    #[serde(default)]
    people: Vec<Person>,
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(default)]
    rewards: Vec<Reward>,
    #[serde(default)]
    ticks: Vec<Tick>,
    #[serde(default)]
    history: Vec<Entry>,
    /// Whether a tick's points wait to be approved (the usual way) or are credited at once.
    #[serde(default = "yes")]
    approval: bool,
    /// The rules that make tasks of calendar events.
    #[serde(default)]
    rules: Vec<EventRule>,
    #[serde(default)]
    awards: Vec<Award>,
}

/// What the page sends to keep the list of people: those with an id are kept (and may be
/// renamed), those without are new, and anyone left out is gone, points and all.
#[derive(Debug, Deserialize)]
pub struct PersonGiven {
    #[serde(default)]
    pub id: Option<EventId>,
    pub name: PersonName,
    /// The reward they are saving for; the cheapest when not given.
    #[serde(default)]
    pub goal: Option<EventId>,
}

#[derive(Debug, Deserialize)]
pub struct TaskGiven {
    #[serde(default)]
    pub id: Option<EventId>,
    pub title: Title,
    #[serde(default)]
    pub who: Option<EventId>,
    #[serde(default)]
    pub points: Points,
    pub repeat: Repeat,
}

#[derive(Debug, Deserialize)]
pub struct RuleGiven {
    #[serde(default)]
    pub id: Option<EventId>,
    pub words: Words,
    #[serde(default)]
    pub who: Option<EventId>,
    #[serde(default)]
    pub points: Points,
}

#[derive(Debug, Deserialize)]
pub struct RewardGiven {
    #[serde(default)]
    pub id: Option<EventId>,
    pub title: Title,
    pub cost: Points,
}

#[derive(Debug, Deserialize)]
pub struct AwardGiven {
    #[serde(default)]
    pub id: Option<EventId>,
    pub title: Title,
    pub points: Points,
}

pub struct Todo {
    path: PathBuf,
    kept: Mutex<Kept>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// An id nothing in `taken` has, made from what the thing is called and the moment.
fn new_id(name: &str, taken: &[&EventId]) -> EventId {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut seed = format!("{name}|{nanos}");
    loop {
        let id = EventId::from_digest(&store::sha1(seed.as_bytes()));
        if !taken.contains(&&id) {
            return id;
        }
        seed.push('+');
    }
}

impl Kept {
    fn person(&mut self, id: &EventId) -> Result<&mut Person, TodoError> {
        self.people.iter_mut().find(|person| person.id == *id).ok_or(TodoError::NoSuchPerson)
    }

    /// Give someone points (or take them), and write it down.
    fn credit(&mut self, who: &EventId, change: i64, what: String) -> Result<(), TodoError> {
        let person = self.person(who)?;
        person.points += change;
        let who = person.name.clone();
        self.history.push(Entry { at: now_secs(), who, what, change });
        let extra = self.history.len().saturating_sub(MOST_HISTORY);
        self.history.drain(..extra);
        Ok(())
    }

    /// The reward someone is saving for: the one chosen for them, or else the cheapest.
    fn goal(&self, person: &Person) -> Option<&Reward> {
        let chosen = person.goal.as_ref().and_then(|goal| self.rewards.iter().find(|reward| reward.id == *goal));
        chosen.or_else(|| self.rewards.iter().min_by_key(|reward| reward.cost.get()))
    }

    /// What the screen shows to celebrate points given to someone, now that they have them.
    fn cheer(&self, who: &EventId, points: i64, what: &str) -> Option<Cheer> {
        let person = self.people.iter().find(|person| person.id == *who)?;
        let goal =
            self.goal(person).map(|reward| Goal { title: reward.title.as_str().to_string(), cost: reward.cost.get() });
        Some(Cheer { name: person.name.as_str().to_string(), what: what.to_string(), points, has: person.points, goal })
    }

    /// Whether a task is in the list for a day: it comes round on that day of the week, or
    /// it is to be done once and has not been (or was done that very day).
    fn listed(&self, task: &Task, day: Day) -> bool {
        match task.repeat {
            Repeat::Days(days) => days.has(day.0.weekday()),
            Repeat::Once(due) => {
                let done = self.ticks.iter().find(|tick| tick.task == task.id).map(|tick| tick.day);
                done.map_or(due.is_none_or(|due| due <= day), |done| done == day)
            }
        }
    }

    /// The tasks that calendar events are, by the rules: each with when in the day it is.
    /// The first rule that is about an event decides whose it is and what it is worth.
    fn of_the_calendar(&self, happenings: &[Happening]) -> Vec<(Task, Option<String>)> {
        let mut tasks: Vec<(Task, Option<String>)> = Vec::new();
        for happening in happenings {
            let rule = self.rules.iter().find(|rule| rule.words.found_in(&happening.title));
            let (Some(rule), Some(title)) = (rule, Title::parse(&happening.title)) else { continue };
            // The same event is the same task however often it is asked about.
            let seed = format!("{:?}|{}", rule.id, title.as_str());
            let id = EventId::from_digest(&store::sha1(seed.as_bytes()));
            if tasks.iter().all(|(task, _)| task.id != id) {
                let (who, points) = (rule.who.clone(), rule.points);
                tasks.push((Task { id, title, who, points, repeat: Repeat::Once(None) }, happening.time.clone()));
            }
        }
        tasks
    }

    /// Forget what no longer matters on `day`: tasks done once on an earlier day, and ticks
    /// too old to be taken back. A tick still waiting for approval is never forgotten.
    fn tidy(&mut self, day: Day) {
        let old = |tick: &Tick| tick.paid && (day.0 - tick.day.0).num_days() > KEPT_DAYS;
        let finished: Vec<EventId> =
            self.ticks.iter().filter(|tick| tick.paid && tick.day < day).map(|tick| tick.task.clone()).collect();
        self.tasks.retain(|task| !(matches!(task.repeat, Repeat::Once(_)) && finished.contains(&task.id)));
        self.ticks.retain(|tick| !old(tick));
    }
}

impl Todo {
    /// What was kept. A file that cannot be read starts everything afresh.
    pub fn load(cfg: &Config) -> Todo {
        let path = cfg.data_dir.join("todo.json");
        let kept = std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok());
        Todo { path, kept: Mutex::new(kept.unwrap_or_default()) }
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, kept: &Kept) {
        if let Err(e) = store::write_atomically(&self.path, json!(kept).to_string().as_bytes()) {
            warn!(error = %e, "could not save the things to do");
        }
    }

    /// Everything, as the pages show it. `day` is the day it is for whoever asks: `today`
    /// lists that day's tasks with whether each is done, late ones first.
    #[cfg(test)]
    pub fn view(&self, day: Day) -> Value {
        self.view_with(day, &[])
    }

    /// The same, with what is on the calendar that day: events the rules make tasks of are
    /// in the day's list too.
    pub fn view_with(&self, day: Day, happenings: &[Happening]) -> Value {
        let kept = self.kept();
        let ours = kept.tasks.iter().filter(|task| kept.listed(task, day)).map(|task| (task.clone(), None, false));
        let events = kept.of_the_calendar(happenings).into_iter().map(|(task, time)| (task, time, true));
        let today: Vec<Value> = ours
            .chain(events)
            .map(|(task, time, event)| {
                let tick = kept.ticks.iter().find(|tick| tick.task == task.id && tick.day == day);
                let late = matches!(task.repeat, Repeat::Once(Some(due)) if due < day) && tick.is_none();
                json!({
                    "id": task.id, "title": task.title, "who": task.who, "points": task.points,
                    "done": tick.is_some(), "by": tick.map(|tick| &tick.who), "late": late,
                    "waiting": tick.is_some_and(|tick| !tick.paid),
                    "once": matches!(task.repeat, Repeat::Once(_)),
                    "event": event, "time": time,
                })
            })
            .collect();
        // Ticks whose points have been asked for and not yet credited, oldest first.
        let waiting: Vec<Value> = kept
            .ticks
            .iter()
            .filter(|tick| !tick.paid)
            .filter_map(|tick| {
                let task = kept.tasks.iter().find(|task| task.id == tick.task);
                let title = tick.title.clone().or_else(|| task.map(|task| task.title.as_str().to_string()))?;
                let who = kept.people.iter().find(|person| person.id == tick.who)?;
                Some(json!({
                    "task": tick.task, "day": tick.day, "title": title, "who": tick.who, "name": who.name,
                    "points": tick.points,
                }))
            })
            .collect();
        let history: Vec<&Entry> = kept.history.iter().rev().take(40).collect();
        // A task done once, on an earlier day, is finished: it is cleared away at the next
        // tick, and is not shown as a task meanwhile.
        let finished = |task: &&Task| {
            matches!(task.repeat, Repeat::Once(_))
                && kept.ticks.iter().any(|tick| tick.task == task.id && tick.paid && tick.day < day)
        };
        let tasks: Vec<&Task> = kept.tasks.iter().filter(|task| !finished(task)).collect();
        // With each person, the reward they are saving for (`aim`), chosen or not.
        let people: Vec<Value> = kept
            .people
            .iter()
            .map(|person| {
                let mut shown = json!(person);
                shown["aim"] = json!(kept.goal(person).map(|reward| &reward.id));
                shown
            })
            .collect();
        json!({
            "day": day, "people": people, "tasks": tasks, "rewards": kept.rewards,
            "today": today, "history": history, "waiting": waiting, "approval": kept.approval,
            "rules": kept.rules, "awards": kept.awards,
        })
    }

    /// Tick a task for a day, or take its tick away. `who` says who did it, which a task
    /// that is somebody's own does not need. Blocking: it is saved.
    #[cfg(test)]
    pub fn tick(
        &self,
        task: &EventId,
        day: Day,
        done: bool,
        who: Option<&EventId>,
    ) -> Result<Option<Cheer>, TodoError> {
        self.tick_with(task, day, done, who, &[])
    }

    /// The same, with what is on the calendar that day, so a calendar event that is a task
    /// can be ticked. Points credited at once are to be celebrated.
    pub fn tick_with(
        &self,
        task: &EventId,
        day: Day,
        done: bool,
        who: Option<&EventId>,
        happenings: &[Happening],
    ) -> Result<Option<Cheer>, TodoError> {
        let mut kept = self.kept();
        kept.tidy(day);
        let ours = kept.tasks.iter().find(|t| t.id == *task).cloned();
        let event = ours.is_none();
        let of_the_calendar =
            || kept.of_the_calendar(happenings).into_iter().map(|(task, _)| task).find(|t| t.id == *task);
        // A tick can always be taken back, even if its event has since left the calendar.
        let ticked_title = kept.ticks.iter().find(|tick| tick.task == *task && tick.day == day).and_then(|tick| {
            let title = Title::parse(tick.title.as_deref()?)?;
            Some(Task { id: task.clone(), title, who: None, points: Points::default(), repeat: Repeat::Once(None) })
        });
        let found = ours.or_else(of_the_calendar).or(ticked_title).ok_or(TodoError::NoSuchTask)?;
        let ticked = kept.ticks.iter().position(|tick| tick.task == *task && tick.day == day);
        let mut cheer = None;
        match (done, ticked) {
            (true, None) => {
                let who = found.who.as_ref().or(who).ok_or(TodoError::WhoDidIt)?.clone();
                let points = found.points.get();
                // Points wait to be approved, unless there are none or nothing waits here.
                let paid = !kept.approval || points == 0;
                if paid {
                    kept.credit(&who, i64::from(points), found.title.as_str().to_string())?;
                    cheer = kept.cheer(&who, i64::from(points), found.title.as_str()).filter(|_| points > 0);
                } else {
                    kept.person(&who)?;
                }
                let title = Some(found.title.as_str().to_string());
                kept.ticks.push(Tick { task: task.clone(), day, who, points, paid, title, event });
            }
            (false, Some(at)) => {
                let tick = kept.ticks.remove(at);
                if !tick.paid {
                    self.save(&kept);
                    return Ok(None);
                }
                // Whoever was given the points may be gone; then there is nothing to take back.
                let _ = kept.credit(
                    &tick.who,
                    -i64::from(tick.points),
                    format!("Not done after all: {}", found.title.as_str()),
                );
            }
            _ => return Ok(None),
        }
        self.save(&kept);
        Ok(cheer)
    }

    /// Approve a tick that is waiting, crediting its points; or turn it down, which takes
    /// the tick away so the task is to do again. Points credited are to be celebrated.
    /// Blocking.
    pub fn approve(&self, task: &EventId, day: Day, approve: bool) -> Result<Option<Cheer>, TodoError> {
        let mut kept = self.kept();
        let mut cheer = None;
        let waiting = |tick: &Tick| tick.task == *task && tick.day == day && !tick.paid;
        let at = kept.ticks.iter().position(waiting).ok_or(TodoError::NothingWaiting)?;
        if approve {
            let (who, points) = (kept.ticks[at].who.clone(), i64::from(kept.ticks[at].points));
            let known = kept.tasks.iter().find(|t| t.id == *task).map(|t| t.title.as_str().to_string());
            let title = kept.ticks[at].title.clone().or(known).unwrap_or_default();
            kept.credit(&who, points, title.clone())?;
            kept.ticks[at].paid = true;
            cheer = kept.cheer(&who, points, &title);
        } else {
            kept.ticks.remove(at);
        }
        self.save(&kept);
        Ok(cheer)
    }

    /// Whether points wait to be approved. Turning that off credits whatever is waiting.
    /// Blocking.
    pub fn set_approval(&self, on: bool) {
        let mut kept = self.kept();
        kept.approval = on;
        if !on {
            let waiting: Vec<Tick> = kept.ticks.iter().filter(|tick| !tick.paid).cloned().collect();
            for tick in waiting {
                let known =
                    kept.tasks.iter().find(|task| task.id == tick.task).map(|task| task.title.as_str().to_string());
                let title = tick.title.clone().or(known).unwrap_or_default();
                // Whoever asked may be gone; then there is nobody to credit.
                let _ = kept.credit(&tick.who, i64::from(tick.points), title);
            }
            for tick in &mut kept.ticks {
                tick.paid = true;
            }
        }
        self.save(&kept);
    }

    /// Keep exactly these people. Blocking.
    pub fn set_people(&self, given: Vec<PersonGiven>) -> Result<(), TodoError> {
        if given.len() > MOST_PEOPLE {
            return Err(TodoError::TooMany(MOST_PEOPLE));
        }
        let mut kept = self.kept();
        let mut people: Vec<Person> = Vec::new();
        for one in given {
            if one.goal.as_ref().is_some_and(|goal| kept.rewards.iter().all(|reward| reward.id != *goal)) {
                return Err(TodoError::NoSuchReward);
            }
            if people.iter().any(|person| person.name.as_str().eq_ignore_ascii_case(one.name.as_str())) {
                return Err(TodoError::SameName);
            }
            let known = one.id.as_ref().and_then(|id| kept.people.iter().find(|person| person.id == *id));
            let taken: Vec<&EventId> = kept.people.iter().chain(&people).map(|person| &person.id).collect();
            let id = known.map_or_else(|| new_id(one.name.as_str(), &taken), |person| person.id.clone());
            let points = known.map_or(0, |person| person.points);
            people.push(Person { id, name: one.name, points, goal: one.goal });
        }
        // A task whose person is gone becomes anyone's.
        for task in &mut kept.tasks {
            if task.who.as_ref().is_some_and(|who| people.iter().all(|person| person.id != *who)) {
                task.who = None;
            }
        }
        for rule in &mut kept.rules {
            if rule.who.as_ref().is_some_and(|who| people.iter().all(|person| person.id != *who)) {
                rule.who = None;
            }
        }
        // And what they asked for and were not yet given goes with them.
        kept.ticks.retain(|tick| tick.paid || people.iter().any(|person| person.id == tick.who));
        info!(people = people.len(), "people for the things to do saved");
        kept.people = people;
        self.save(&kept);
        Ok(())
    }

    /// Keep exactly these tasks. Blocking.
    pub fn set_tasks(&self, given: Vec<TaskGiven>) -> Result<(), TodoError> {
        if given.len() > MOST_TASKS {
            return Err(TodoError::TooMany(MOST_TASKS));
        }
        let mut kept = self.kept();
        let mut tasks: Vec<Task> = Vec::new();
        for one in given {
            if one.who.as_ref().is_some_and(|who| kept.people.iter().all(|person| person.id != *who)) {
                return Err(TodoError::NoSuchPerson);
            }
            let known = one.id.filter(|id| kept.tasks.iter().any(|task| task.id == *id));
            let taken: Vec<&EventId> = kept.tasks.iter().chain(&tasks).map(|task| &task.id).collect();
            let id = known.unwrap_or_else(|| new_id(one.title.as_str(), &taken));
            tasks.push(Task { id, title: one.title, who: one.who, points: one.points, repeat: one.repeat });
        }
        kept.ticks.retain(|tick| tick.event || tasks.iter().any(|task| task.id == tick.task));
        info!(tasks = tasks.len(), "things to do saved");
        kept.tasks = tasks;
        self.save(&kept);
        Ok(())
    }

    /// Keep exactly these rewards. Blocking.
    pub fn set_rewards(&self, given: Vec<RewardGiven>) -> Result<(), TodoError> {
        if given.len() > MOST_REWARDS {
            return Err(TodoError::TooMany(MOST_REWARDS));
        }
        let mut kept = self.kept();
        let mut rewards: Vec<Reward> = Vec::new();
        for one in given {
            let known = one.id.filter(|id| kept.rewards.iter().any(|reward| reward.id == *id));
            let taken: Vec<&EventId> = kept.rewards.iter().chain(&rewards).map(|reward| &reward.id).collect();
            let id = known.unwrap_or_else(|| new_id(one.title.as_str(), &taken));
            rewards.push(Reward { id, title: one.title, cost: one.cost });
        }
        // Whoever was saving for a reward that is gone saves for the cheapest.
        for person in &mut kept.people {
            if person.goal.as_ref().is_some_and(|goal| rewards.iter().all(|reward| reward.id != *goal)) {
                person.goal = None;
            }
        }
        kept.rewards = rewards;
        self.save(&kept);
        Ok(())
    }

    /// Keep exactly these rules for making tasks of calendar events. Blocking.
    pub fn set_rules(&self, given: Vec<RuleGiven>) -> Result<(), TodoError> {
        if given.len() > MOST_RULES {
            return Err(TodoError::TooMany(MOST_RULES));
        }
        let mut kept = self.kept();
        let mut rules: Vec<EventRule> = Vec::new();
        for one in given {
            if one.who.as_ref().is_some_and(|who| kept.people.iter().all(|person| person.id != *who)) {
                return Err(TodoError::NoSuchPerson);
            }
            let known = one.id.filter(|id| kept.rules.iter().any(|rule| rule.id == *id));
            let taken: Vec<&EventId> = kept.rules.iter().chain(&rules).map(|rule| &rule.id).collect();
            let id = known.unwrap_or_else(|| new_id(&format!("{:?}", one.words), &taken));
            rules.push(EventRule { id, words: one.words, who: one.who, points: one.points });
        }
        kept.rules = rules;
        self.save(&kept);
        Ok(())
    }

    /// Spend someone's points on a reward, if they have enough. Blocking.
    pub fn redeem(&self, who: &EventId, reward: &EventId) -> Result<(), TodoError> {
        let mut kept = self.kept();
        let reward = kept.rewards.iter().find(|r| r.id == *reward).ok_or(TodoError::NoSuchReward)?.clone();
        let have = kept.person(who)?.points;
        let cost = reward.cost.get();
        if have < i64::from(cost) {
            return Err(TodoError::NotEnough { have, cost });
        }
        kept.credit(who, -i64::from(cost), format!("Reward: {}", reward.title.as_str()))?;
        info!(cost, "a reward was taken");
        self.save(&kept);
        Ok(())
    }

    /// Give someone points, or take some away, for a reason. Points given are to be
    /// celebrated. Blocking.
    pub fn adjust(&self, who: &EventId, change: PointChange, why: &Title) -> Result<Option<Cheer>, TodoError> {
        let mut kept = self.kept();
        let change = i64::from(change.get());
        kept.credit(who, change, why.as_str().to_string())?;
        let cheer = kept.cheer(who, change, why.as_str()).filter(|_| change > 0);
        self.save(&kept);
        Ok(cheer)
    }

    /// Keep exactly these quick awards. Blocking.
    pub fn set_awards(&self, given: Vec<AwardGiven>) -> Result<(), TodoError> {
        if given.len() > MOST_AWARDS {
            return Err(TodoError::TooMany(MOST_AWARDS));
        }
        let mut kept = self.kept();
        let mut awards: Vec<Award> = Vec::new();
        for one in given {
            let known = one.id.filter(|id| kept.awards.iter().any(|award| award.id == *id));
            let taken: Vec<&EventId> = kept.awards.iter().chain(&awards).map(|award| &award.id).collect();
            let id = known.unwrap_or_else(|| new_id(one.title.as_str(), &taken));
            awards.push(Award { id, title: one.title, points: one.points });
        }
        kept.awards = awards;
        self.save(&kept);
        Ok(())
    }

    /// Give someone a quick award: its points, and a celebration, even for an award worth
    /// none. Blocking.
    pub fn award(&self, who: &EventId, award: &EventId) -> Result<Cheer, TodoError> {
        let mut kept = self.kept();
        let award = kept.awards.iter().find(|a| a.id == *award).ok_or(TodoError::NoSuchAward)?.clone();
        let points = i64::from(award.points.get());
        kept.credit(who, points, award.title.as_str().to_string())?;
        let cheer = kept.cheer(who, points, award.title.as_str()).ok_or(TodoError::NoSuchPerson)?;
        self.save(&kept);
        Ok(cheer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempFrame;
    use serde_json::from_value;

    fn day(text: &str) -> Day {
        Day::parse(text).unwrap()
    }

    /// A list with Ana and Rui, and what the view says of them.
    fn household(name: &str) -> (Todo, TempFrame, EventId, EventId) {
        let frame = TempFrame::new(name);
        let todo = Todo::load(&frame.0);
        todo.set_approval(false);
        todo.set_people(from_value(json!([{ "name": "Ana" }, { "name": "Rui" }])).unwrap()).unwrap();
        let people = todo.view(day("2026-10-13"))["people"].clone();
        let id = |at: usize| from_value::<EventId>(people[at]["id"].clone()).unwrap();
        (todo, frame, id(0), id(1))
    }

    fn points(todo: &Todo, at: usize) -> i64 {
        todo.view(day("2026-10-13"))["people"][at]["points"].as_i64().unwrap()
    }

    fn task_id(todo: &Todo, title: &str) -> EventId {
        let view = todo.view(day("2026-10-13"));
        let task = view["tasks"].as_array().unwrap().iter().find(|task| task["title"] == title).unwrap();
        from_value(task["id"].clone()).unwrap()
    }

    #[test]
    fn a_task_that_comes_round_is_ticked_day_by_day_and_pays_its_points() {
        let (todo, frame, ana, _) = household("todo-days");
        // 13 October 2026 is a Tuesday.
        let tasks = json!([{ "title": "Feed the cat", "who": ana, "points": 5, "repeat": { "days": [2, 4] } }]);
        todo.set_tasks(from_value(tasks).unwrap()).unwrap();
        let cat = task_id(&todo, "Feed the cat");

        let tuesday = todo.view(day("2026-10-13"));
        assert_eq!(tuesday["today"].as_array().unwrap().len(), 1);
        assert_eq!((&tuesday["today"][0]["done"], &tuesday["today"][0]["once"]), (&json!(false), &json!(false)));
        assert!(todo.view(day("2026-10-14"))["today"].as_array().unwrap().is_empty(), "not on a Wednesday");

        todo.tick(&cat, day("2026-10-13"), true, None).unwrap();
        todo.tick(&cat, day("2026-10-13"), true, None).unwrap();
        assert_eq!(points(&todo, 0), 5, "ticking twice pays once");
        assert_eq!(todo.view(day("2026-10-13"))["today"][0]["by"], json!(ana));
        assert_eq!(todo.view(day("2026-10-15"))["today"][0]["done"], false, "Thursday is another day");

        // It is all there after a restart, and a tick can be taken back.
        let again = Todo::load(&frame.0);
        assert_eq!(points(&again, 0), 5);
        again.tick(&cat, day("2026-10-13"), false, None).unwrap();
        assert_eq!(points(&again, 0), 0);
        let history = again.view(day("2026-10-13"))["history"].clone();
        assert_eq!(
            (&history[0]["change"], &history[1]["change"], &history[1]["who"]),
            (&json!(-5), &json!(5), &json!("Ana"))
        );
    }

    #[test]
    fn a_task_done_once_is_late_until_done_then_goes() {
        let (todo, _frame, _, rui) = household("todo-once");
        let tasks = json!([
            { "title": "Post the letter", "points": 10, "repeat": { "once": "2026-10-13" } },
            { "title": "Sort the shed", "repeat": { "once": null } },
        ]);
        todo.set_tasks(from_value(tasks).unwrap()).unwrap();
        let letter = task_id(&todo, "Post the letter");
        let titles = |on: &str| -> Vec<String> {
            let view = todo.view(day(on));
            view["today"].as_array().unwrap().iter().map(|task| task["title"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(titles("2026-10-12"), ["Sort the shed"], "the letter is not due yet");
        assert_eq!(titles("2026-10-13"), ["Post the letter", "Sort the shed"]);
        let late = todo.view(day("2026-10-14"));
        assert_eq!((&late["today"][0]["late"], &late["today"][1]["late"]), (&json!(true), &json!(false)));

        // It is anyone's, so whoever ticks it says who they are.
        assert_eq!(todo.tick(&letter, day("2026-10-14"), true, None), Err(TodoError::WhoDidIt));
        todo.tick(&letter, day("2026-10-14"), true, Some(&rui)).unwrap();
        assert_eq!(points(&todo, 1), 10);
        assert_eq!(titles("2026-10-14"), ["Post the letter", "Sort the shed"], "ticked, for the day it was done");
        assert_eq!(titles("2026-10-15"), ["Sort the shed"]);
        assert_eq!(todo.view(day("2026-10-15"))["tasks"].as_array().unwrap().len(), 1, "and no longer a task");
        // The next day's first tick clears it away for good; its points stay.
        let shed = task_id(&todo, "Sort the shed");
        todo.tick(&shed, day("2026-10-15"), true, Some(&rui)).unwrap();
        assert_eq!(todo.view(day("2026-10-15"))["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(points(&todo, 1), 10, "the shed is worth nothing");
        assert_eq!(todo.tick(&letter, day("2026-10-15"), true, Some(&rui)), Err(TodoError::NoSuchTask));
    }

    #[test]
    fn points_buy_rewards_and_can_be_given_or_taken_by_hand() {
        let (todo, _frame, ana, rui) = household("todo-rewards");
        todo.set_rewards(from_value(json!([{ "title": "Film night", "cost": 30 }])).unwrap()).unwrap();
        let view = todo.view(day("2026-10-13"));
        let film: EventId = from_value(view["rewards"][0]["id"].clone()).unwrap();

        assert_eq!(todo.redeem(&ana, &film), Err(TodoError::NotEnough { have: 0, cost: 30 }));
        todo.adjust(&ana, PointChange::new(40).unwrap(), &Title::parse("Helped all weekend").unwrap()).unwrap();
        todo.redeem(&ana, &film).unwrap();
        assert_eq!((points(&todo, 0), points(&todo, 1)), (10, 0));
        todo.adjust(&rui, PointChange::new(-5).unwrap(), &Title::parse("Left the bike out").unwrap()).unwrap();
        assert_eq!(points(&todo, 1), -5);
        assert_eq!(todo.view(day("2026-10-13"))["history"][1]["what"], "Reward: Film night");

        assert_eq!(todo.redeem(&ana, &ana), Err(TodoError::NoSuchReward));
        assert_eq!(todo.redeem(&film, &film), Err(TodoError::NoSuchPerson));
        // Renaming keeps the reward's id; leaving it out removes it.
        let renamed = json!([{ "id": film, "title": "Cinema", "cost": 50 }]);
        todo.set_rewards(from_value(renamed).unwrap()).unwrap();
        assert_eq!(todo.view(day("2026-10-13"))["rewards"], json!([{ "id": film, "title": "Cinema", "cost": 50 }]));
    }

    #[test]
    fn quick_awards_give_points_at_a_touch_and_are_cheered_towards_a_goal() {
        let (todo, frame, ana, rui) = household("todo-awards");
        let awards = json!([{ "title": "Shared a toy", "points": 1 }, { "title": "Well done", "points": 0 }]);
        todo.set_awards(from_value(awards).unwrap()).unwrap();
        let rewards = json!([{ "title": "Ice cream", "cost": 5 }, { "title": "Sticker", "cost": 2 }]);
        todo.set_rewards(from_value(rewards).unwrap()).unwrap();
        let view = todo.view(day("2026-10-13"));
        let id = |list: &str, at: usize| from_value::<EventId>(view[list][at]["id"].clone()).unwrap();
        let (shared, praise, ice, sticker) = (id("awards", 0), id("awards", 1), id("rewards", 0), id("rewards", 1));
        assert_eq!(view["people"][0]["aim"], json!(sticker), "with nothing chosen, the cheapest");

        let cheer = todo.award(&ana, &shared).unwrap();
        let goal = Some(Goal { title: "Sticker".into(), cost: 2 });
        assert_eq!(cheer, Cheer { name: "Ana".into(), what: "Shared a toy".into(), points: 1, has: 1, goal });
        assert_eq!(todo.award(&ana, &praise).unwrap().points, 0, "praise worth no points is cheered all the same");
        assert_eq!(todo.view(day("2026-10-13"))["history"][1]["what"], "Shared a toy");
        assert_eq!(todo.award(&ana, &ana), Err(TodoError::NoSuchAward));
        assert_eq!(todo.award(&shared, &shared), Err(TodoError::NoSuchPerson));

        // Chosen for Rui, his goal is the ice cream; when it goes, he saves for the cheapest.
        let people = json!([{ "id": ana, "name": "Ana" }, { "id": rui, "name": "Rui", "goal": ice }]);
        todo.set_people(from_value(people).unwrap()).unwrap();
        assert_eq!(todo.award(&rui, &shared).unwrap().goal, Some(Goal { title: "Ice cream".into(), cost: 5 }));
        assert_eq!(Todo::load(&frame.0).view(day("2026-10-13"))["people"][1]["aim"], json!(ice), "it is kept");
        todo.set_rewards(from_value(json!([{ "id": sticker, "title": "Sticker", "cost": 2 }])).unwrap()).unwrap();
        let view = todo.view(day("2026-10-13"));
        assert_eq!((&view["people"][1]["goal"], &view["people"][1]["aim"]), (&Value::Null, &json!(sticker)));
        let unknown = json!([{ "name": "Ana", "goal": ice }]);
        assert_eq!(todo.set_people(from_value(unknown).unwrap()), Err(TodoError::NoSuchReward));

        // Points given by hand, or credited for a task, are cheered; points taken are not.
        let why = Title::parse("Tidied up").unwrap();
        assert_eq!(todo.adjust(&rui, PointChange::new(3).unwrap(), &why).unwrap().unwrap().has, 4);
        assert_eq!(todo.adjust(&rui, PointChange::new(-1).unwrap(), &why).unwrap(), None);
        let tasks = json!([{ "title": "Brush teeth", "who": ana, "points": 2, "repeat": { "once": null } }]);
        todo.set_tasks(from_value(tasks).unwrap()).unwrap();
        let teeth = task_id(&todo, "Brush teeth");
        assert_eq!(todo.tick(&teeth, day("2026-10-13"), true, None).unwrap().unwrap().has, 3);
        assert_eq!(todo.tick(&teeth, day("2026-10-13"), false, None).unwrap(), None);
        todo.set_approval(true);
        assert_eq!(todo.tick(&teeth, day("2026-10-13"), true, None).unwrap(), None, "only asked for");
        assert_eq!(todo.approve(&teeth, day("2026-10-13"), true).unwrap().unwrap().what, "Brush teeth");

        let many: Vec<Value> = (0..41).map(|n| json!({ "title": format!("Award {n}"), "points": 1 })).collect();
        assert_eq!(todo.set_awards(from_value(json!(many)).unwrap()), Err(TodoError::TooMany(40)));
    }

    #[test]
    fn people_keep_their_points_when_renamed_and_their_tasks_become_anyones_when_they_go() {
        let (todo, _frame, ana, rui) = household("todo-people");
        todo.adjust(&ana, PointChange::new(7).unwrap(), &Title::parse("A start").unwrap()).unwrap();
        let tasks =
            json!([{ "title": "Lay the table", "who": rui, "points": 2, "repeat": { "days": [1, 2, 3, 4, 5, 6, 7] } }]);
        todo.set_tasks(from_value(tasks).unwrap()).unwrap();

        todo.set_people(from_value(json!([{ "id": ana, "name": "Ana Maria" }, { "name": "Zé" }])).unwrap()).unwrap();
        let view = todo.view(day("2026-10-13"));
        assert_eq!((&view["people"][0]["name"], &view["people"][0]["points"]), (&json!("Ana Maria"), &json!(7)));
        assert_eq!((&view["people"][1]["name"], &view["people"][1]["points"]), (&json!("Zé"), &json!(0)));
        assert_eq!(view["tasks"][0]["who"], Value::Null, "Rui is gone, so his task is anyone's");

        // What someone asked for goes when they do.
        todo.set_approval(true);
        let table = task_id(&todo, "Lay the table");
        let ze: EventId = from_value(view["people"][1]["id"].clone()).unwrap();
        todo.tick(&table, day("2026-10-13"), true, Some(&ze)).unwrap();
        assert_eq!(todo.view(day("2026-10-13"))["waiting"].as_array().unwrap().len(), 1);
        todo.set_people(from_value(json!([{ "id": ana, "name": "Ana Maria" }])).unwrap()).unwrap();
        let view = todo.view(day("2026-10-13"));
        assert_eq!((&view["waiting"], &view["today"][0]["done"]), (&json!([]), &json!(false)));

        let twice = from_value(json!([{ "name": "Ana" }, { "name": "ana" }])).unwrap();
        assert_eq!(todo.set_people(twice), Err(TodoError::SameName));
        let crowd: Vec<Value> = (0..13).map(|n| json!({ "name": format!("Person {n}") })).collect();
        assert_eq!(todo.set_people(from_value(json!(crowd)).unwrap()), Err(TodoError::TooMany(12)));
        let stranger = json!([{ "title": "x", "who": rui, "repeat": { "once": null } }]);
        assert_eq!(todo.set_tasks(from_value(stranger).unwrap()), Err(TodoError::NoSuchPerson));
    }

    #[test]
    fn points_wait_to_be_approved_unless_set_not_to() {
        let (todo, frame, ana, _) = household("todo-approval");
        todo.set_approval(true);
        let tasks = json!([
            { "title": "Feed the cat", "who": ana, "points": 5, "repeat": { "days": [1, 2, 3, 4, 5, 6, 7] } },
            { "title": "Post the letter", "who": ana, "points": 10, "repeat": { "once": null } },
            { "title": "Smile", "who": ana, "repeat": { "once": null } },
        ]);
        todo.set_tasks(from_value(tasks).unwrap()).unwrap();
        let (cat, letter, smile) =
            (task_id(&todo, "Feed the cat"), task_id(&todo, "Post the letter"), task_id(&todo, "Smile"));
        let tuesday = day("2026-10-13");

        todo.tick(&cat, tuesday, true, None).unwrap();
        todo.tick(&letter, tuesday, true, None).unwrap();
        todo.tick(&smile, tuesday, true, None).unwrap();
        let view = todo.view(tuesday);
        assert_eq!(points(&todo, 0), 0, "asked for, not yet credited");
        assert_eq!((&view["today"][0]["done"], &view["today"][0]["waiting"]), (&json!(true), &json!(true)));
        assert_eq!(view["today"][2]["waiting"], false, "nothing to approve where there are no points");
        assert_eq!(view["waiting"].as_array().unwrap().len(), 2);
        assert_eq!((&view["waiting"][0]["name"], &view["waiting"][0]["points"]), (&json!("Ana"), &json!(5)));

        // Approved, the points are credited; turned down, the task is to do again.
        todo.approve(&cat, tuesday, true).unwrap();
        todo.approve(&letter, tuesday, false).unwrap();
        assert_eq!(points(&todo, 0), 5);
        let view = todo.view(tuesday);
        assert_eq!((&view["today"][0]["waiting"], &view["today"][1]["done"]), (&json!(false), &json!(false)));
        assert_eq!(todo.approve(&cat, tuesday, true), Err(TodoError::NothingWaiting));

        // A request is not forgotten however long it waits, and the task with it; taking
        // the tick back oneself withdraws it.
        todo.tick(&letter, tuesday, true, None).unwrap();
        let later = day("2026-10-20");
        todo.tick(&cat, later, true, None).unwrap();
        let again = Todo::load(&frame.0);
        assert_eq!(again.view(later)["waiting"].as_array().unwrap().len(), 2);
        again.tick(&cat, later, false, None).unwrap();
        assert_eq!(again.view(later)["waiting"].as_array().unwrap().len(), 1);
        // Set not to wait any more, what is waiting is credited.
        again.set_approval(false);
        assert_eq!((points(&again, 0), again.view(later)["waiting"].clone()), (15, json!([])));
    }

    #[test]
    fn calendar_events_are_tasks_where_a_rule_says_so() {
        let (todo, _frame, ana, rui) = household("todo-events");
        todo.set_approval(true);
        let rules = json!([
            { "words": "swimming, football", "who": ana, "points": 6 },
            { "words": "lesson", "who": rui, "points": 1 },
            { "words": "dentist", "points": 4 },
        ]);
        todo.set_rules(from_value(rules).unwrap()).unwrap();
        let on = |title: &str, time: Option<&str>| Happening { title: title.into(), time: time.map(str::to_string) };
        let calendar = [
            on("Swimming lesson", Some("10:00")),
            on("Bins out", None),
            on("Dentist", Some("15:30")),
            on("Swimming lesson", Some("10:00")),
        ];
        let tuesday = day("2026-10-13");

        let view = todo.view_with(tuesday, &calendar);
        let today = view["today"].as_array().unwrap();
        assert_eq!(today.len(), 2, "the bins are nobody's task, and the lesson is listed once");
        assert_eq!(
            (&today[0]["title"], &today[0]["who"], &today[0]["points"]),
            (&json!("Swimming lesson"), &json!(ana), &json!(6))
        );
        assert_eq!(
            (&today[0]["event"], &today[0]["time"], &today[1]["who"]),
            (&json!(true), &json!("10:00"), &Value::Null)
        );
        assert_eq!(view["rules"].as_array().unwrap().len(), 3);

        // Ticked, it waits for approval like any task; the request is known by its name
        // even on a day the event is not on.
        let swim: EventId = from_value(today[0]["id"].clone()).unwrap();
        todo.tick_with(&swim, tuesday, true, None, &calendar).unwrap();
        assert_eq!(todo.tick(&swim, day("2026-10-14"), true, None), Err(TodoError::NoSuchTask), "no lesson that day");
        let waiting = todo.view(day("2026-10-14"))["waiting"].clone();
        assert_eq!((&waiting[0]["title"], &waiting[0]["points"]), (&json!("Swimming lesson"), &json!(6)));
        todo.approve(&swim, tuesday, true).unwrap();
        assert_eq!(points(&todo, 0), 6);
        assert_eq!(todo.view(tuesday)["history"][0]["what"], "Swimming lesson");
        // Other changes to the tasks leave its tick alone, and it can be taken back without the calendar.
        todo.set_tasks(Vec::new()).unwrap();
        assert_eq!(todo.view_with(tuesday, &calendar)["today"][0]["done"], true);
        todo.tick(&swim, tuesday, false, None).unwrap();
        assert_eq!(points(&todo, 0), 0);

        let stranger = json!([{ "words": "x", "who": swim }]);
        assert_eq!(todo.set_rules(from_value(stranger).unwrap()), Err(TodoError::NoSuchPerson));
    }
}
