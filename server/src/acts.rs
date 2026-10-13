//! Things the frame's screen has been told to do, on their way to it.
//!
//! Whatever tells the frame to do something (a button on the phone page today; a key, a
//! rule or a spoken word are others) hands an `Action` to `Acts::tell`. The screen page
//! asks for them with a request that is held open until there is something to say, so an
//! action is on the screen within a moment rather than at the page's next half minute.
//! With each such request the page says how it stands (`Said`), which is how the phone
//! page knows what is playing.
//!
//! Nothing is kept on disk: an action is for now. One that the screen did not collect
//! within a minute is dropped, so a frame that was off does not act on old news.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::watch;

use crate::types::{Action, Said};

/// An action waits this long for the screen to collect it.
const KEPT_FOR: Duration = Duration::from_mins(1);
/// The screen is taken to be listening if it asked no longer ago than this.
const LISTENING_WITHIN: Duration = Duration::from_secs(45);
/// At most this many actions wait at once; the oldest make way.
const MOST_WAITING: usize = 32;

/// A moment on the clock that waiting is measured by.
type Moment = tokio::time::Instant;

#[derive(Default)]
struct Inner {
    /// The number of the action told last; they count up from one.
    last: u64,
    waiting: VecDeque<(u64, Moment, Action)>,
    /// What the screen last said about itself, and when.
    said: Option<(Moment, Said)>,
}

pub struct Acts {
    inner: Mutex<Inner>,
    /// Carries the number of the action told last, to wake requests that are waiting for one.
    news: watch::Sender<u64>,
}

impl Default for Acts {
    fn default() -> Self {
        Acts { inner: Mutex::default(), news: watch::channel(0).0 }
    }
}

impl Acts {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Have the screen do something. Returns the action's number.
    pub fn tell(&self, action: Action) -> u64 {
        let number = {
            let mut inner = self.inner();
            inner.last += 1;
            let number = inner.last;
            inner.waiting.push_back((number, Moment::now(), action));
            while inner.waiting.len() > MOST_WAITING {
                inner.waiting.pop_front();
            }
            number
        };
        self.news.send_replace(number);
        number
    }

    /// Whether the screen page has asked what to do lately, so that an action will reach it.
    pub fn listening(&self) -> bool {
        self.inner().said.as_ref().is_some_and(|(at, _)| at.elapsed() <= LISTENING_WITHIN)
    }

    /// Note how the screen says it stands.
    pub fn heard(&self, said: Said) {
        self.inner().said = Some((Moment::now(), said));
    }

    /// The actions told after number `after`, once there are any or `patience` has run out.
    /// Without a number (a page that has just loaded) the answer comes at once and holds
    /// nothing: only where to count from, so the page does not repeat what came before it.
    pub async fn after(&self, after: Option<u64>, patience: Duration) -> Value {
        let Some(after) = after else {
            return json!({ "last": self.inner().last, "acts": [] });
        };
        let mut news = self.news.subscribe();
        // A number from before a restart of the server is ahead of ours: start again from here.
        if after <= self.inner().last {
            let _ = tokio::time::timeout(patience, news.wait_for(|last| *last > after)).await;
        }
        let mut inner = self.inner();
        inner.waiting.retain(|(_, told, _)| told.elapsed() <= KEPT_FOR);
        let acts: Vec<&Action> =
            inner.waiting.iter().filter(|(number, ..)| *number > after).map(|(.., action)| action).collect();
        json!({ "last": inner.last, "acts": acts })
    }

    /// How the screen stands, for the phone page: what it last said, and whether it is listening.
    pub fn view(&self) -> Value {
        let inner = self.inner();
        let listening = inner.said.as_ref().is_some_and(|(at, _)| at.elapsed() <= LISTENING_WITHIN);
        let said = inner.said.as_ref().filter(|_| listening).map(|(_, said)| said);
        json!({ "listening": listening, "said": said })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tab;

    const SHORT: Duration = Duration::from_millis(20);

    #[tokio::test]
    async fn a_page_that_has_just_loaded_learns_where_to_count_from_and_nothing_else() {
        let acts = Acts::default();
        acts.tell(Action::Next);
        assert_eq!(acts.after(None, SHORT).await, json!({ "last": 1, "acts": [] }));
    }

    #[tokio::test]
    async fn actions_told_since_are_handed_over_in_order() {
        let acts = Acts::default();
        acts.tell(Action::Next);
        acts.tell(Action::Show { tab: Tab::Calendar });
        acts.tell(Action::Stop);
        let reply = acts.after(Some(1), SHORT).await;
        assert_eq!(reply, json!({ "last": 3, "acts": [{ "do": "show", "tab": "calendar" }, { "do": "stop" }] }));
        assert_eq!(
            acts.after(Some(3), SHORT).await,
            json!({ "last": 3, "acts": [] }),
            "nothing new: waited, then said so"
        );
    }

    #[tokio::test]
    async fn a_waiting_request_is_answered_the_moment_something_is_told() {
        let acts = std::sync::Arc::new(Acts::default());
        let waiting = tokio::spawn({
            let acts = std::sync::Arc::clone(&acts);
            async move { acts.after(Some(0), Duration::from_secs(30)).await }
        });
        tokio::time::sleep(SHORT).await;
        acts.tell(Action::Pause);
        let reply = tokio::time::timeout(Duration::from_secs(2), waiting).await.expect("answered at once").unwrap();
        assert_eq!(reply, json!({ "last": 1, "acts": [{ "do": "pause" }] }));
    }

    #[tokio::test(start_paused = true)]
    async fn an_action_nobody_collected_is_dropped_and_a_quiet_screen_is_not_listening() {
        let acts = Acts::default();
        assert!(!acts.listening());
        acts.heard(Said { tab: Some(Tab::Pictures), ..Said::default() });
        assert!(acts.listening());
        assert_eq!(acts.view()["said"]["tab"], "pictures");
        acts.tell(Action::Next);
        tokio::time::advance(KEPT_FOR + Duration::from_secs(1)).await;
        assert_eq!(acts.after(Some(0), SHORT).await, json!({ "last": 1, "acts": [] }));
        assert!(!acts.listening());
        assert_eq!(acts.view(), json!({ "listening": false, "said": null }));
    }

    #[tokio::test]
    async fn a_number_from_before_a_restart_is_answered_at_once() {
        let acts = Acts::default();
        let reply = tokio::time::timeout(Duration::from_secs(2), acts.after(Some(40), Duration::from_secs(30))).await;
        assert_eq!(reply.expect("did not wait"), json!({ "last": 0, "acts": [] }));
    }

    #[tokio::test]
    async fn only_so_many_actions_wait() {
        let acts = Acts::default();
        for _ in 0..MOST_WAITING + 5 {
            acts.tell(Action::Next);
        }
        assert_eq!(acts.after(Some(0), SHORT).await["acts"].as_array().unwrap().len(), MOST_WAITING);
    }
}
