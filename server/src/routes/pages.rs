//! The pages the server hands out, and the log for reading in a browser.
//!
//! Their sources are ordinary html, css and js files in `pages/`. Each page is sent as
//! one document with its stylesheets and scripts inside it, so it arrives in one request
//! and is never half old and half new.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::response::{Html, IntoResponse};

use crate::app::{blocking, Shared};
use crate::routes::NO_STORE;
use crate::store;

use std::sync::LazyLock;

/// A page with its stylesheets and scripts put inside it: each `<link>` to a stylesheet
/// and each `<script src>` in the page is replaced by the file it names.
fn whole(html: &str, files: &[(&str, &str)]) -> String {
    let mut page = html.to_string();
    for (name, text) in files {
        let sheet = std::path::Path::new(name).extension().is_some_and(|kind| kind.eq_ignore_ascii_case("css"));
        let (tag, inside) = if sheet {
            (format!(r#"<link rel="stylesheet" href="{name}">"#), format!("<style>\n{text}</style>"))
        } else {
            (format!(r#"<script src="{name}"></script>"#), format!("<script>\n{text}</script>"))
        };
        page = page.replace(&tag, &inside);
    }
    page
}

/// The page for managing the frame from a phone or computer.
static MANAGE: LazyLock<String> = LazyLock::new(|| {
    whole(
        include_str!("../../pages/manage/index.html"),
        &[
            ("manage.css", include_str!("../../pages/manage/manage.css")),
            ("manage.js", include_str!("../../pages/manage/manage.js")),
        ],
    )
});

/// The frame's own screen.
static SCREEN: LazyLock<String> = LazyLock::new(|| {
    whole(
        include_str!("../../pages/screen/index.html"),
        &[
            ("screen.css", include_str!("../../pages/screen/screen.css")),
            ("utilities.css", include_str!("../../pages/screen/utilities.css")),
            ("home.css", include_str!("../../pages/screen/home.css")),
            ("yoto.css", include_str!("../../pages/screen/yoto.css")),
            ("nownext.css", include_str!("../../pages/screen/nownext.css")),
            ("music.css", include_str!("../../pages/screen/music.css")),
            ("extras.css", include_str!("../../pages/screen/extras.css")),
            ("todo.css", include_str!("../../pages/screen/todo.css")),
            ("screen.js", include_str!("../../pages/screen/screen.js")),
            ("utilities.js", include_str!("../../pages/screen/utilities.js")),
            ("home.js", include_str!("../../pages/screen/home.js")),
            ("yoto.js", include_str!("../../pages/screen/yoto.js")),
            ("nownext.js", include_str!("../../pages/screen/nownext.js")),
            ("music.js", include_str!("../../pages/screen/music.js")),
            ("actions.js", include_str!("../../pages/screen/actions.js")),
            ("extras.js", include_str!("../../pages/screen/extras.js")),
            ("todo.js", include_str!("../../pages/screen/todo.js")),
        ],
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
