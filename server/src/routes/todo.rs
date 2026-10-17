//! Things to do: the list for a day, ticking tasks, and looking after people, tasks,
//! rewards and points.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{blocking, Shared};
use crate::routes::{ApiError, ApiResult, Asked, Parsed};
use crate::todo::{AwardGiven, Happening, PersonGiven, RewardGiven, RuleGiven, TaskGiven, TodoError};
use crate::types::{Action, Cheer, Day, EventId, PointChange, Title, Today};

#[derive(Deserialize)]
pub(crate) struct TickRequest {
    pub(crate) task: EventId,
    /// The day it is where the task was ticked.
    pub(crate) day: Today,
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
pub(crate) struct AwardsRequest {
    pub(crate) awards: Vec<AwardGiven>,
}

#[derive(Deserialize)]
pub(crate) struct AwardRequest {
    pub(crate) who: EventId,
    pub(crate) award: EventId,
}

#[derive(Deserialize)]
pub(crate) struct AdjustRequest {
    pub(crate) who: EventId,
    pub(crate) change: PointChange,
    pub(crate) why: Title,
}

/// Which day the list is asked for: `?day=YYYY-MM-DD`, the day it is for whoever asks.
#[derive(Deserialize)]
pub(crate) struct DayQuery {
    pub(crate) day: Day,
}

fn refused(error: &TodoError) -> ApiError {
    let status = match error {
        TodoError::NoSuchPerson
        | TodoError::NoSuchTask
        | TodoError::NoSuchReward
        | TodoError::NoSuchAward
        | TodoError::NothingWaiting => StatusCode::NOT_FOUND,
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

/// Make a change to the things to do, and tell the screen there is news: at once, when
/// someone was given points that are to be celebrated.
async fn change(
    app: Shared,
    work: impl FnOnce(&Shared) -> Result<Option<Cheer>, TodoError> + Send + 'static,
) -> ApiResult {
    blocking(move || {
        let cheer = work(&app).map_err(|e| refused(&e))?;
        app.todo_changed();
        if let Some(cheer) = cheer {
            app.acts.tell(Action::Cheer(cheer));
        }
        Ok(Json(json!({ "saved": true })))
    })
    .await
}

/// Everything about the things to do, with the list for `?day=YYYY-MM-DD`: the day it is
/// for whoever asks.
pub(crate) async fn todo_view(State(app): State<Shared>, Asked(DayQuery { day }): Asked<DayQuery>) -> ApiResult {
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
        Ok(None)
    })
    .await
}

/// Tick a task, or take its tick away. This needs no PIN: ticking is what the list is for,
/// and where points wait for approval a tick only asks for them. A tick is only for a day
/// about now: one for a day long gone would clear away tasks as if their days were over.
pub(crate) async fn tick_task(State(app): State<Shared>, Parsed(request): Parsed<TickRequest>) -> ApiResult {
    let Today(day) = request.day;
    change(app, move |app| {
        app.todo.tick_with(&request.task, day, request.done, request.who.as_ref(), &happenings(app, day))
    })
    .await
}

pub(crate) async fn save_people(State(app): State<Shared>, Parsed(request): Parsed<PeopleRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_people(request.people).map(|()| None)).await
}

pub(crate) async fn save_tasks(State(app): State<Shared>, Parsed(request): Parsed<TasksRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_tasks(request.tasks).map(|()| None)).await
}

/// Keep exactly these rules for making tasks of calendar events.
pub(crate) async fn save_rules(State(app): State<Shared>, Parsed(request): Parsed<RulesRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_rules(request.rules).map(|()| None)).await
}

pub(crate) async fn save_rewards(State(app): State<Shared>, Parsed(request): Parsed<RewardsRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_rewards(request.rewards).map(|()| None)).await
}

/// Spend someone's points on a reward.
pub(crate) async fn redeem(State(app): State<Shared>, Parsed(request): Parsed<RedeemRequest>) -> ApiResult {
    change(app, move |app| app.todo.redeem(&request.who, &request.reward).map(|()| None)).await
}

/// Give someone points or take some away, by hand.
pub(crate) async fn adjust_points(State(app): State<Shared>, Parsed(request): Parsed<AdjustRequest>) -> ApiResult {
    change(app, move |app| app.todo.adjust(&request.who, request.change, &request.why)).await
}

/// Keep exactly these quick awards.
pub(crate) async fn save_awards(State(app): State<Shared>, Parsed(request): Parsed<AwardsRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_awards(request.awards).map(|()| None)).await
}

/// Give someone a quick award; the screen celebrates it.
pub(crate) async fn give_award(State(app): State<Shared>, Parsed(request): Parsed<AwardRequest>) -> ApiResult {
    change(app, move |app| app.todo.award(&request.who, &request.award).map(Some)).await
}

/// For the screen's half-minute report: how many times the things to do have changed.
pub(crate) fn changes(app: &Shared) -> Value {
    json!(app.todo_changes.load(std::sync::atomic::Ordering::Relaxed))
}
