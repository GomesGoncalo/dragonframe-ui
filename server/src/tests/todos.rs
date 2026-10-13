//! Route tests: things to do.

use super::*;

const TUESDAY: &str = "2026-10-13";

async fn todo(app: &Shared) -> Value {
    get(app, &format!("/api/todo?day={TUESDAY}")).await.json()
}

#[tokio::test]
async fn things_to_do_are_set_up_ticked_and_paid_in_points_that_buy_rewards() {
    let (app, _frame) = app("todo");
    let empty = todo(&app).await;
    assert_eq!((&empty["people"], &empty["today"], &empty["rewards"]), (&json!([]), &json!([]), &json!([])));
    assert_eq!(get(&app, "/api/todo").await.status, StatusCode::BAD_REQUEST, "the day has to be said");

    assert_eq!(
        post(&app, "/api/todo/people", r#"{"people":[{"name":"Ana"},{"name":"Rui"}]}"#).await.status,
        StatusCode::OK
    );
    let ana = todo(&app).await["people"][0]["id"].as_str().unwrap().to_string();
    let tasks = json!({ "tasks": [
        { "title": "Feed the cat", "who": ana, "points": 5, "repeat": { "days": [1, 2, 3, 4, 5, 6, 7] } },
        { "title": "Post the letter", "points": 10, "repeat": { "once": null } },
    ] });
    assert_eq!(post(&app, "/api/todo/tasks", &tasks.to_string()).await.status, StatusCode::OK);
    post(&app, "/api/todo/rewards", r#"{"rewards":[{"title":"Film night","cost":12}]}"#).await;

    let before = get(&app, "/api/alive").await.json()["todo"].as_u64().unwrap();
    let list = todo(&app).await;
    let (cat, letter) = (list["today"][0]["id"].as_str().unwrap(), list["today"][1]["id"].as_str().unwrap());
    let tick = |task: &str, more: &str| format!(r#"{{"task":"{task}","day":"{TUESDAY}","done":true{more}}}"#);
    assert_eq!(post(&app, "/api/todo/done", &tick(cat, "")).await.status, StatusCode::OK);
    assert_eq!(post(&app, "/api/todo/done", &tick(letter, "")).await.status, StatusCode::BAD_REQUEST, "whose is it?");
    assert_eq!(post(&app, "/api/todo/done", &tick(letter, &format!(r#","who":"{ana}""#))).await.status, StatusCode::OK);
    assert!(get(&app, "/api/alive").await.json()["todo"].as_u64().unwrap() > before, "the screen is told");

    let list = todo(&app).await;
    assert_eq!((&list["people"][0]["points"], &list["today"][0]["done"]), (&json!(15), &json!(true)));
    let film = list["rewards"][0]["id"].as_str().unwrap();
    let rui = list["people"][1]["id"].as_str().unwrap();
    let redeem = |who: &str| format!(r#"{{"who":"{who}","reward":"{film}"}}"#);
    let short = post(&app, "/api/todo/redeem", &redeem(rui)).await;
    assert_eq!(short.status, StatusCode::CONFLICT);
    assert!(short.json()["error"].as_str().unwrap().contains("0 of 12"));
    assert_eq!(post(&app, "/api/todo/redeem", &redeem(&ana)).await.status, StatusCode::OK);
    let give = format!(r#"{{"who":"{rui}","change":3,"why":"Helped"}}"#);
    assert_eq!(post(&app, "/api/todo/points", &give).await.status, StatusCode::OK);
    let list = todo(&app).await;
    assert_eq!((&list["people"][0]["points"], &list["people"][1]["points"]), (&json!(3), &json!(3)));
    assert_eq!(list["history"][1]["what"], "Reward: Film night");

    for (path, bad) in [
        ("/api/todo/points", format!(r#"{{"who":"{rui}","change":0,"why":"x"}}"#)),
        ("/api/todo/points", format!(r#"{{"who":"{rui}","change":5000,"why":"x"}}"#)),
        ("/api/todo/people", r#"{"people":[{"name":""}]}"#.to_string()),
        ("/api/todo/tasks", r#"{"tasks":[{"title":"x","repeat":{"days":[]}}]}"#.to_string()),
        ("/api/todo/tasks", r#"{"tasks":[{"title":"x","points":-1,"repeat":{"once":null}}]}"#.to_string()),
        ("/api/todo/done", r#"{"task":"nope","day":"2026-10-13","done":true}"#.to_string()),
    ] {
        assert_eq!(post(&app, path, &bad).await.status, StatusCode::BAD_REQUEST, "{path} {bad}");
    }
    let unknown = r#"{"task":"0123456789abcdef","day":"2026-10-13","done":true}"#;
    assert_eq!(post(&app, "/api/todo/done", unknown).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn with_a_pin_a_task_can_still_be_ticked_but_nothing_else_about_them_changed() {
    let (app, _frame) = app("todo-pin");
    post(&app, "/api/todo/people", r#"{"people":[{"name":"Ana"}]}"#).await;
    let ana = todo(&app).await["people"][0]["id"].as_str().unwrap().to_string();
    let tasks = json!({ "tasks": [{ "title": "Feed the cat", "who": ana, "points": 5, "repeat": { "once": null } }] });
    post(&app, "/api/todo/tasks", &tasks.to_string()).await;
    post(&app, "/api/todo/rewards", r#"{"rewards":[{"title":"Film night","cost":1}]}"#).await;
    let list = todo(&app).await;
    let (cat, film) = (list["today"][0]["id"].as_str().unwrap(), list["rewards"][0]["id"].as_str().unwrap());
    post(&app, "/api/pin", r#"{"pin":"2468"}"#).await;

    let tick = format!(r#"{{"task":"{cat}","day":"{TUESDAY}","done":true}}"#);
    assert_eq!(post(&app, "/api/todo/done", &tick).await.status, StatusCode::OK);
    assert_eq!(todo(&app).await["people"][0]["points"], 5);
    for (path, body) in [
        ("/api/todo/people", r#"{"people":[]}"#.to_string()),
        ("/api/todo/tasks", r#"{"tasks":[]}"#.to_string()),
        ("/api/todo/rewards", r#"{"rewards":[]}"#.to_string()),
        ("/api/todo/redeem", format!(r#"{{"who":"{ana}","reward":"{film}"}}"#)),
        ("/api/todo/points", format!(r#"{{"who":"{ana}","change":100,"why":"x"}}"#)),
    ] {
        assert_eq!(post(&app, path, &body).await.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    assert_eq!(todo(&app).await["people"][0]["points"], 5, "points are only spent or given with the PIN");
}
