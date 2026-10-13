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
use crate::todo::{PersonGiven, RewardGiven, TaskGiven, TodoError};
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

fn refused(error: &TodoError) -> ApiError {
    let status = match error {
        TodoError::NoSuchPerson | TodoError::NoSuchTask | TodoError::NoSuchReward => StatusCode::NOT_FOUND,
        TodoError::WhoDidIt => StatusCode::BAD_REQUEST,
        TodoError::NotEnough { .. } | TodoError::TooMany(_) | TodoError::SameName => StatusCode::CONFLICT,
    };
    ApiError::new(status, error.to_string())
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
    Ok(Json(blocking(move || app.todo.view(day)).await))
}

/// Tick a task, or take its tick away. This needs no PIN: ticking is what the list is for.
pub(crate) async fn tick_task(State(app): State<Shared>, Parsed(request): Parsed<TickRequest>) -> ApiResult {
    change(app, move |app| app.todo.tick(&request.task, request.day, request.done, request.who.as_ref())).await
}

pub(crate) async fn save_people(State(app): State<Shared>, Parsed(request): Parsed<PeopleRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_people(request.people)).await
}

pub(crate) async fn save_tasks(State(app): State<Shared>, Parsed(request): Parsed<TasksRequest>) -> ApiResult {
    change(app, move |app| app.todo.set_tasks(request.tasks)).await
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
