//! Things to do: who does them, the points they earn, and what points buy.
//!
//! A task belongs to a person (or to whoever gets to it first) and is worth some points.
//! It is done once, on a day or whenever, or comes round on chosen days of the week.
//! Ticking a task on the frame's screen gives its points to whoever did it; taking the
//! tick away takes them back. Points are spent on rewards.
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
use crate::types::{Day, EventId, PersonName, Points, Title, Weekdays};

const MOST_PEOPLE: usize = 12;
const MOST_TASKS: usize = 100;
const MOST_REWARDS: usize = 40;
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
    #[error("say who did it: this task is anyone's")]
    WhoDidIt,
    #[error("not enough points for that yet: {have} of {cost}")]
    NotEnough { have: i64, cost: u32 },
    #[error("there can be at most {0} of those")]
    TooMany(usize),
    #[error("two people cannot have the same name")]
    SameName,
}

/// Someone who does tasks and collects points.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: EventId,
    pub name: PersonName,
    /// What they have to spend. It can be below nothing if points were taken away.
    #[serde(default)]
    pub points: i64,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reward {
    pub id: EventId,
    pub title: Title,
    pub cost: Points,
}

/// That a task was done on a day, by whom, and what it earned them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Tick {
    task: EventId,
    day: Day,
    who: EventId,
    points: u32,
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// What the page sends to keep the list of people: those with an id are kept (and may be
/// renamed), those without are new, and anyone left out is gone, points and all.
#[derive(Debug, Deserialize)]
pub struct PersonGiven {
    #[serde(default)]
    pub id: Option<EventId>,
    pub name: PersonName,
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
pub struct RewardGiven {
    #[serde(default)]
    pub id: Option<EventId>,
    pub title: Title,
    pub cost: Points,
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

    /// Forget what no longer matters on `day`: tasks done once on an earlier day, and ticks
    /// too old to be taken back.
    fn tidy(&mut self, day: Day) {
        let old = |tick: &Tick| (day.0 - tick.day.0).num_days() > KEPT_DAYS;
        let finished: Vec<EventId> =
            self.ticks.iter().filter(|tick| tick.day < day).map(|tick| tick.task.clone()).collect();
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
    pub fn view(&self, day: Day) -> Value {
        let kept = self.kept();
        let today: Vec<Value> = kept
            .tasks
            .iter()
            .filter(|task| kept.listed(task, day))
            .map(|task| {
                let tick = kept.ticks.iter().find(|tick| tick.task == task.id && tick.day == day);
                let late = matches!(task.repeat, Repeat::Once(Some(due)) if due < day) && tick.is_none();
                json!({
                    "id": task.id, "title": task.title, "who": task.who, "points": task.points,
                    "done": tick.is_some(), "by": tick.map(|tick| &tick.who), "late": late,
                    "once": matches!(task.repeat, Repeat::Once(_)),
                })
            })
            .collect();
        let history: Vec<&Entry> = kept.history.iter().rev().take(40).collect();
        json!({
            "day": day, "people": kept.people, "tasks": kept.tasks, "rewards": kept.rewards,
            "today": today, "history": history,
        })
    }

    /// Tick a task for a day, or take its tick away. `who` says who did it, which a task
    /// that is somebody's own does not need. Blocking: it is saved.
    pub fn tick(&self, task: &EventId, day: Day, done: bool, who: Option<&EventId>) -> Result<(), TodoError> {
        let mut kept = self.kept();
        kept.tidy(day);
        let found = kept.tasks.iter().find(|t| t.id == *task).ok_or(TodoError::NoSuchTask)?.clone();
        let ticked = kept.ticks.iter().position(|tick| tick.task == *task && tick.day == day);
        match (done, ticked) {
            (true, None) => {
                let who = found.who.as_ref().or(who).ok_or(TodoError::WhoDidIt)?.clone();
                let points = found.points.get();
                kept.credit(&who, i64::from(points), found.title.as_str().to_string())?;
                kept.ticks.push(Tick { task: task.clone(), day, who, points });
            }
            (false, Some(at)) => {
                let tick = kept.ticks.remove(at);
                // Whoever was given the points may be gone; then there is nothing to take back.
                let _ = kept.credit(
                    &tick.who,
                    -i64::from(tick.points),
                    format!("Not done after all: {}", found.title.as_str()),
                );
            }
            _ => return Ok(()),
        }
        self.save(&kept);
        Ok(())
    }

    /// Keep exactly these people. Blocking.
    pub fn set_people(&self, given: Vec<PersonGiven>) -> Result<(), TodoError> {
        if given.len() > MOST_PEOPLE {
            return Err(TodoError::TooMany(MOST_PEOPLE));
        }
        let mut kept = self.kept();
        let mut people: Vec<Person> = Vec::new();
        for one in given {
            if people.iter().any(|person| person.name.as_str().eq_ignore_ascii_case(one.name.as_str())) {
                return Err(TodoError::SameName);
            }
            let known = one.id.as_ref().and_then(|id| kept.people.iter().find(|person| person.id == *id));
            let taken: Vec<&EventId> = kept.people.iter().chain(&people).map(|person| &person.id).collect();
            let id = known.map_or_else(|| new_id(one.name.as_str(), &taken), |person| person.id.clone());
            people.push(Person { id, name: one.name, points: known.map_or(0, |person| person.points) });
        }
        // A task whose person is gone becomes anyone's.
        for task in &mut kept.tasks {
            if task.who.as_ref().is_some_and(|who| people.iter().all(|person| person.id != *who)) {
                task.who = None;
            }
        }
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
        kept.ticks.retain(|tick| tasks.iter().any(|task| task.id == tick.task));
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
        kept.rewards = rewards;
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

    /// Give someone points, or take some away, for a reason. Blocking.
    pub fn adjust(&self, who: &EventId, change: i32, why: &Title) -> Result<(), TodoError> {
        let mut kept = self.kept();
        kept.credit(who, i64::from(change), why.as_str().to_string())?;
        self.save(&kept);
        Ok(())
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
        todo.adjust(&ana, 40, &Title::parse("Helped all weekend").unwrap()).unwrap();
        todo.redeem(&ana, &film).unwrap();
        assert_eq!((points(&todo, 0), points(&todo, 1)), (10, 0));
        todo.adjust(&rui, -5, &Title::parse("Left the bike out").unwrap()).unwrap();
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
    fn people_keep_their_points_when_renamed_and_their_tasks_become_anyones_when_they_go() {
        let (todo, _frame, ana, rui) = household("todo-people");
        todo.adjust(&ana, 7, &Title::parse("A start").unwrap()).unwrap();
        let tasks =
            json!([{ "title": "Lay the table", "who": rui, "points": 2, "repeat": { "days": [1, 2, 3, 4, 5, 6, 7] } }]);
        todo.set_tasks(from_value(tasks).unwrap()).unwrap();

        todo.set_people(from_value(json!([{ "id": ana, "name": "Ana Maria" }, { "name": "Zé" }])).unwrap()).unwrap();
        let view = todo.view(day("2026-10-13"));
        assert_eq!((&view["people"][0]["name"], &view["people"][0]["points"]), (&json!("Ana Maria"), &json!(7)));
        assert_eq!((&view["people"][1]["name"], &view["people"][1]["points"]), (&json!("Zé"), &json!(0)));
        assert_eq!(view["tasks"][0]["who"], Value::Null, "Rui is gone, so his task is anyone's");

        let twice = from_value(json!([{ "name": "Ana" }, { "name": "ana" }])).unwrap();
        assert_eq!(todo.set_people(twice), Err(TodoError::SameName));
        let crowd: Vec<Value> = (0..13).map(|n| json!({ "name": format!("Person {n}") })).collect();
        assert_eq!(todo.set_people(from_value(json!(crowd)).unwrap()), Err(TodoError::TooMany(12)));
        let stranger = json!([{ "title": "x", "who": rui, "repeat": { "once": null } }]);
        assert_eq!(todo.set_tasks(from_value(stranger).unwrap()), Err(TodoError::NoSuchPerson));
    }
}
