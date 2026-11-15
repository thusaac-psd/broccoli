//! The judging sessions running on this server: a detached evaluate session
//! counts from the moment a plugin starts it until it has finalized its
//! judgement and run its after-judging hooks. A submission reads `Judged` as
//! soon as its judgement is finalized, before the hooks run, so "no session
//! running" is what says this server's judging work for it is over.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct JudgingSessions(Arc<Inner>);

#[derive(Default)]
struct Inner {
    running: AtomicUsize,
    idle: Notify,
}

/// One running session; dropping it ends the session.
pub struct RunningSession(JudgingSessions);

impl JudgingSessions {
    /// Counts a session as running until the returned guard is dropped.
    pub fn enter(&self) -> RunningSession {
        self.0.running.fetch_add(1, Ordering::AcqRel);
        RunningSession(self.clone())
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
}

impl Drop for RunningSession {
    fn drop(&mut self) {
        if self.0.0.running.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.0.idle.notify_waiters();
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
        let first = sessions.enter();
        let second = sessions.enter();
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
}
