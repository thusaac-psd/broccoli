//! The judging sessions running on this server: a detached evaluate session
//! counts from the moment a plugin starts it until it has finalized its
//! judgement and run its after-judging hooks. A submission reads `Judged` as
//! soon as its judgement is finalized, before the hooks run, so "no session
//! running" is what says this server's judging work for it is over.
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
    plugins: Mutex<Plugins>,
}

/// The plugins with sessions running, each with the entry its sessions hold.
#[derive(Default)]
struct Plugins {
    entries: HashMap<String, Entry>,
    /// The id the next entry gets.
    next_id: u64,
}

/// The sessions of one plugin that one cut ends together.
struct Entry {
    /// Tells this entry apart from one the same plugin holds after a cut.
    id: u64,
    cut: CancellationToken,
    holders: usize,
}

/// One running session; dropping it ends the session.
pub struct RunningSession {
    sessions: JudgingSessions,
    plugin_id: String,
    /// The [`Entry`] this session holds.
    entry_id: u64,
    cut: CancellationToken,
}

impl JudgingSessions {
    /// Counts a session of `plugin_id` as running until the returned guard is
    /// dropped.
    pub fn enter(&self, plugin_id: &str) -> RunningSession {
        self.0.running.fetch_add(1, Ordering::AcqRel);
        let mut plugins = self.plugins();
        let Plugins { entries, next_id } = &mut *plugins;
        let entry = entries.entry(plugin_id.to_string()).or_insert_with(|| {
            *next_id += 1;
            Entry {
                id: *next_id,
                cut: CancellationToken::new(),
                holders: 0,
            }
        });
        entry.holders += 1;
        RunningSession {
            sessions: self.clone(),
            plugin_id: plugin_id.to_string(),
            entry_id: entry.id,
            cut: entry.cut.clone(),
        }
    }

    /// Cuts every session of `plugin_id` running now; sessions it starts
    /// later run uncut. Returns how many were cut.
    pub fn cut(&self, plugin_id: &str) -> usize {
        let Some(entry) = self.plugins().entries.remove(plugin_id) else {
            return 0;
        };
        entry.cut.cancel();
        entry.holders
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

    fn plugins(&self) -> std::sync::MutexGuard<'_, Plugins> {
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
            // Only the entry this session joined: a cut took it out (with its
            // holder count) and the plugin may hold a new one by now.
            let mut plugins = self.sessions.plugins();
            if let Some(entry) = plugins.entries.get_mut(&self.plugin_id)
                && entry.id == self.entry_id
            {
                entry.holders -= 1;
                if entry.holders == 0 {
                    plugins.entries.remove(&self.plugin_id);
                }
            }
        }
        let inner = &self.sessions.0;
        if inner.running.fetch_sub(1, Ordering::AcqRel) == 1 {
            inner.idle.notify_waiters();
        }
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

    /// A cut takes the plugin's entry out before it cancels the entry's
    /// token. A session of that cut ending in between must leave alone the
    /// entry a session started meanwhile holds.
    #[tokio::test]
    async fn a_cut_session_ending_leaves_a_later_sessions_entry_alone() {
        let sessions = JudgingSessions::default();
        let old = sessions.enter("p");
        // The first half of a cut: the entry is gone, its token not yet
        // cancelled.
        let taken = sessions.plugins().entries.remove("p");
        let later = sessions.enter("p");
        drop(old);
        drop(taken);
        assert_eq!(sessions.cut("p"), 1, "the later session is still cut");
        later.cut().await;
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
}
