//! The judging work running on this server: a detached evaluate session
//! counts from the moment a plugin starts it, and a contest plugin's
//! submission call from its dispatch, until it has finalized its judgement
//! and run its after-judging hooks. A submission reads `Judged` as soon as
//! its judgement is finalized, before the hooks run, so "nothing running" is
//! what says this server's judging work for it is over.
//!
//! A session belongs to the contest plugin that started it. Unloading that
//! plugin for good (a disable, or the fleet forgetting it) cuts every one of
//! its sessions at once ([`JudgingSessions::cut`]), so each ends now instead
//! of when its next result arrives.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct JudgingSessions(Arc<Inner>);

#[derive(Default)]
struct Inner {
    running: AtomicUsize,
    idle: Notify,
    /// Per plugin with sessions running: the token that cuts them, and how
    /// many hold it.
    plugins: Mutex<HashMap<String, (CancellationToken, usize)>>,
}

/// One running session; dropping it ends the session.
pub struct RunningSession {
    sessions: JudgingSessions,
    plugin_id: String,
    cut: CancellationToken,
}

impl JudgingSessions {
    /// Counts a session of `plugin_id` as running until the returned guard is
    /// dropped.
    pub fn enter(&self, plugin_id: &str) -> RunningSession {
        self.0.running.fetch_add(1, Ordering::AcqRel);
        let mut plugins = self.plugins();
        let (cut, holders) = plugins
            .entry(plugin_id.to_string())
            .or_insert_with(|| (CancellationToken::new(), 0));
        *holders += 1;
        RunningSession {
            sessions: self.clone(),
            plugin_id: plugin_id.to_string(),
            cut: cut.clone(),
        }
    }

    /// Counts judging work that is not a session of its own (a contest
    /// plugin's submission call) as running until the returned guard is
    /// dropped. It is never cut.
    pub fn track(&self) -> TrackedJudging {
        self.0.running.fetch_add(1, Ordering::AcqRel);
        TrackedJudging(self.clone())
    }

    fn leave(&self) {
        if self.0.running.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }

    /// Cuts every session of `plugin_id` running now; sessions it starts
    /// later run uncut. Returns how many were cut.
    pub fn cut(&self, plugin_id: &str) -> usize {
        let Some((cut, holders)) = self.plugins().remove(plugin_id) else {
            return 0;
        };
        cut.cancel();
        holders
    }

    /// Sessions running now.
    pub fn running(&self) -> usize {
        self.0.running.load(Ordering::Acquire)
    }

    /// Completes once no session is running.
    pub async fn idle(&self) {
        loop {
            let notified = self.0.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.running() == 0 {
                return;
            }
            notified.await;
        }
    }

    fn plugins(&self) -> std::sync::MutexGuard<'_, HashMap<String, (CancellationToken, usize)>> {
        self.0.plugins.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl RunningSession {
    /// Completes once the session is cut.
    pub async fn cut(&self) {
        self.cut.cancelled().await
    }
}

impl Drop for RunningSession {
    fn drop(&mut self) {
        {
            let mut plugins = self.sessions.plugins();
            // A cut session's entry is gone: [`JudgingSessions::cut`] removes
            // it under this lock before it cancels the token.
            if !self.cut.is_cancelled()
                && let Some((_, holders)) = plugins.get_mut(&self.plugin_id)
            {
                *holders -= 1;
                if *holders == 0 {
                    plugins.remove(&self.plugin_id);
                }
            }
        }
        self.sessions.leave();
    }
}

/// Judging work [`JudgingSessions::track`] counts; dropping it ends it.
pub struct TrackedJudging(JudgingSessions);

impl Drop for TrackedJudging {
    fn drop(&mut self) {
        self.0.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_completes_once_the_last_session_ends() {
        let sessions = JudgingSessions::default();
        sessions.idle().await;
        let first = sessions.enter("p");
        let second = sessions.enter("q");
        let waiter = tokio::spawn({
            let sessions = sessions.clone();
            async move { sessions.idle().await }
        });
        drop(first);
        assert_eq!(sessions.running(), 1);
        assert!(!waiter.is_finished());
        drop(second);
        waiter.await.unwrap();
        assert_eq!(sessions.running(), 0);
    }

    #[tokio::test]
    async fn a_cut_ends_the_running_sessions_of_its_plugin_only() {
        let sessions = JudgingSessions::default();
        let (a, b, other) = (
            sessions.enter("p"),
            sessions.enter("p"),
            sessions.enter("q"),
        );
        assert_eq!(sessions.cut("p"), 2);
        a.cut().await;
        b.cut().await;
        assert!(!other.cut.is_cancelled());
        let later = sessions.enter("p");
        assert!(
            !later.cut.is_cancelled(),
            "a session started after the cut runs"
        );
        drop((a, b));
        assert_eq!(sessions.cut("p"), 1, "the cut sessions hold nothing");
        drop((later, other));
        assert_eq!(sessions.cut("q"), 0, "an ended session holds nothing");
        assert_eq!(sessions.running(), 0);
    }

    #[tokio::test]
    async fn tracked_judging_counts_toward_idle_and_is_never_cut() {
        let sessions = JudgingSessions::default();
        let tracked = sessions.track();
        assert_eq!(sessions.running(), 1);
        assert_eq!(sessions.cut("p"), 0, "no plugin's session to cut");
        let waiter = tokio::spawn({
            let sessions = sessions.clone();
            async move { sessions.idle().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(tracked);
        waiter.await.unwrap();
    }
}
