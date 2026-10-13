//! Things to do: the list for a day, ticking tasks, and looking after people, tasks,
//! rewards and points.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{blocking, Shared};
use crate::routes::{ApiError, ApiResult, Parsed};
use crate::todo::{Happening, PersonGiven, RewardGiven, RuleGiven, TaskGiven, TodoError};
use crate::types::{Day, EventId, Title};

#[derive(Deserialize)]
pub(crate) struct TickRequest {
    pub(crate) task: EventId,
    /// The day it is where the task was ticked.
    pub(crate) day: Day,
    pub(crate) done: bool,
    /// Who did it, for a task that is anyone's.
    #[serde(default)]
    pub(crate) who: Option<EventId>,
}

#[derive(Deserialize)]
pub(crate) struct ApproveRequest {
    pub(crate) task: EventId,
    pub(crate) day: Day,
    /// Credit the points, or turn the request down.
    pub(crate) approve: bool,
}

#[derive(Deserialize)]
pub(crate) struct ApprovalRequest {
    /// Whether points wait to be approved.
    pub(crate) on: bool,
}

#[derive(Deserialize)]
pub(crate) struct PeopleRequest {
    pub(crate) people: Vec<PersonGiven>,
}

#[derive(Deserialize)]
pub(crate) struct TasksRequest {
    pub(crate) tasks: Vec<TaskGiven>,
}

#[derive(Deserialize)]
pub(crate) struct RewardsRequest {
    pub(crate) rewards: Vec<RewardGiven>,
}

#[derive(Deserialize)]
pub(crate) struct RulesRequest {
    pub(crate) rules: Vec<RuleGiven>,
}

#[derive(Deserialize)]
pub(crate) struct RedeemRequest {
    pub(crate) who: EventId,
    pub(crate) reward: EventId,
}

#[derive(Deserialize)]
pub(crate) struct AdjustRequest {
    pub(crate) who: EventId,
    /// Points to give, or to take when below nothing: at most a thousand either way.
    pub(crate) change: i32,
    pub(crate) why: Title,
}

/// A tick is for a day about now. The server does not know the frame's time zone, so the
/// day before and the day after its own are as good as its own; anything else is a
/// mistake, and would clear away tasks as if their days were long over.
fn about_now(day: Day) -> Result<Day, ApiError> {
    let today = chrono::DateTime::from_timestamp(i64::try_from(crate::app::now_secs()).unwrap_or(0), 0)
        .unwrap_or_default()
        .date_naive();
    if (day.0 - today).num_days().abs() <= 1 {
        Ok(day)
    } else {
        Err(ApiError::bad_request("that is not today"))
    }
}

fn refused(error: &TodoError) -> ApiError {
    let status = match error {
        TodoError::NoSuchPerson | TodoError::NoSuchTask | TodoError::NoSuchReward | TodoError::NothingWaiting => {
            StatusCode::NOT_FOUND
        }
        TodoError::WhoDidIt => StatusCode::BAD_REQUEST,
        TodoError::NotEnough { .. } | TodoError::TooMany(_) | TodoError::SameName => StatusCode::CONFLICT,
    };
    ApiError::new(status, error.to_string())
}

/// What is on the calendar on a day, for the rules that make tasks of events. Blocking.
///
/// A whole-day event is on each of its days and a time on the clock on its own day. An
/// event at a moment is on whichever day that is on the frame's clock.
fn happenings(app: &Shared, day: Day) -> Vec<Happening> {
    const DAY_SECS: i64 = 24 * 60 * 60;
    let zone = app.device.time_zone().and_then(|name| name.parse::<chrono_tz::Tz>().ok());
    let midnight = day.0.and_time(chrono::NaiveTime::MIN).and_utc().timestamp();
    let window = crate::types::Window { from: midnight - DAY_SECS, to: midnight + 2 * DAY_SECS };
    let on_the_day = |event: &Value| -> Option<Happening> {
        let title = event["title"].as_str()?.to_string();
        let clock = |time: chrono::NaiveDateTime| (time.date() == day.0).then(|| time.format("%H:%M").to_string());
        let time = match &event["start"] {
            Value::Number(secs) => {
                let at = chrono::DateTime::from_timestamp(secs.as_i64()?, 0)?;
                Some(clock(zone.map_or_else(|| at.naive_utc(), |zone| at.with_timezone(&zone).naive_local()))?)
            }
            Value::String(text) if text.len() == 10 => {
                let (first, after) = (Day::parse(text)?, Day::parse(event["end"].as_str()?)?);
                (first <= day && day < after).then_some(None)?
            }
            Value::String(text) => Some(clock(chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").ok()?)?),
            _ => return None,
        };
        Some(Happening { title, time })
    };
    app.calendars.between(window).iter().filter_map(on_the_day).collect()
}

/// Make a change to the things to do, and tell the screen there is news.
async fn change(app: Shared, work: impl FnOnce(&Shared) -> Result<(), TodoError> + Send + 'static) -> ApiResult {
    blocking(move || {
        work(&app).map_err(|e| refused(&e))?;
        app.todo_changed();
        Ok(Json(json!({ "saved": true })))
    })
    .await
}

/// Everything about the things to do, with the list for `?day=YYYY-MM-DD`: the day it is
/// for whoever asks.
pub(crate) async fn todo_view(State(app): State<Shared>, Query(query): Query<HashMap<String, String>>) -> ApiResult {
    let day = query.get("day").and_then(|text| Day::parse(text));
    let day = day.ok_or_else(|| ApiError::bad_request("which day is it? (?day=YYYY-MM-DD)"))?;
    Ok(Json(blocking(move || app.todo.view_with(day, &happenings(&app, day))).await))
}

/// Approve a tick that is waiting for its points, or turn it down.
pub(crate) async fn approve_tick(State(app): State<Shared>, Parsed(request): Parsed<ApproveRequest>) -> ApiResult {
    change(app, move |app| app.todo.approve(&request.task, request.day, request.approve)).await
}

/// Choose whether points wait to be approved.
pub(crate) async fn set_approval(State(app): State<Shared>, Parsed(request): Parsed<ApprovalRequest>) -> ApiResult {
    change(app, move |app| {
        app.todo.set_approval(request.on);
        Ok(())
    })
    .await
}

/// Tick a task, or take its tick away. This needs no PIN: ticking is what the list is for,
/// and where points wait for approval a tick only asks for them.
pub(crate) async fn tick_task(State(app): State<Shared>, Parsed(request): Parsed<TickRequest>) -> ApiResult {
    let day = about_now(request.day)?;
    change(app, move |app| {
        app.todo.tick_with(&request.task, day, request.done, request.who.as_ref(), &happenings(app, day))
    })
    .await
}

pub(crate) async fn save_people(State(app): State<Shared>, Parsed(request): Parsed<PeopleRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_people(request.people)).await
}

pub(crate) async fn save_tasks(State(app): State<Shared>, Parsed(request): Parsed<TasksRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_tasks(request.tasks)).await
}

/// Keep exactly these rules for making tasks of calendar events.
pub(crate) async fn save_rules(State(app): State<Shared>, Parsed(request): Parsed<RulesRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_rules(request.rules)).await
}

pub(crate) async fn save_rewards(State(app): State<Shared>, Parsed(request): Parsed<RewardsRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_rewards(request.rewards)).await
}

/// Spend someone's points on a reward.
pub(crate) async fn redeem(State(app): State<Shared>, Parsed(request): Parsed<RedeemRequest>) -> ApiResult {
    change(app, move |app| app.todo.redeem(&request.who, &request.reward)).await
}

/// Give someone points or take some away, by hand.
pub(crate) async fn adjust_points(State(app): State<Shared>, Parsed(request): Parsed<AdjustRequest>) -> ApiResult {
    if request.change == 0 || request.change.abs() > 1000 {
        return Err(ApiError::bad_request("give or take 1 to 1000 points at a time"));
    }
    change(app, move |app| app.todo.adjust(&request.who, request.change, &request.why)).await
}

/// For the screen's half-minute report: how many times the things to do have changed.
pub(crate) fn changes(app: &Shared) -> Value {
    json!(app.todo_changes.load(std::sync::atomic::Ordering::Relaxed))
}
