//! The pages the server hands out, and the log for reading in a browser.
//!
//! Their sources are ordinary html, css and js files in `pages/`. Each page is sent as
//! one document with its stylesheet and script inside it, so it arrives in one request and
//! is never half old and half new.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::response::{Html, IntoResponse};

use crate::app::{blocking, Shared};
use crate::routes::NO_STORE;
use crate::store;

use std::sync::LazyLock;

/// A page with its stylesheet and script put inside it: the one `<link>` and the one
/// `<script src>` in the page are replaced by what they name.
fn whole(html: &str, sheet: (&str, &str), script: (&str, &str)) -> String {
    html.replace(&format!(r#"<link rel="stylesheet" href="{}">"#, sheet.0), &format!("<style>\n{}</style>", sheet.1))
        .replace(&format!(r#"<script src="{}"></script>"#, script.0), &format!("<script>\n{}</script>", script.1))
}

/// The page for managing the frame from a phone or computer.
static MANAGE: LazyLock<String> = LazyLock::new(|| {
    whole(
        include_str!("../../pages/manage/index.html"),
        ("manage.css", include_str!("../../pages/manage/manage.css")),
        ("manage.js", include_str!("../../pages/manage/manage.js")),
    )
});

/// The frame's own screen.
static SCREEN: LazyLock<String> = LazyLock::new(|| {
    whole(
        include_str!("../../pages/screen/index.html"),
        ("screen.css", include_str!("../../pages/screen/screen.css")),
        ("screen.js", include_str!("../../pages/screen/screen.js")),
    )
});

/// The screen in a box of the frame's size, for trying it on a computer.
const SIMULATOR: &str = include_str!("../../pages/simulator.html");

pub(crate) async fn page() -> impl IntoResponse {
    ([(CACHE_CONTROL, NO_STORE)], Html(MANAGE.as_str()))
}

pub(crate) async fn frame_screen() -> impl IntoResponse {
    ([(CACHE_CONTROL, NO_STORE)], Html(SCREEN.as_str()))
}

pub(crate) async fn simulator() -> impl IntoResponse {
    ([(CACHE_CONTROL, NO_STORE)], Html(SIMULATOR))
}

pub(crate) async fn logs(State(app): State<Shared>) -> impl IntoResponse {
    let cfg = Arc::clone(&app.cfg);
    let text = blocking(move || store::recent_log(&cfg, 500)).await;
    ([(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8")), (CACHE_CONTROL, NO_STORE)], text)
}
