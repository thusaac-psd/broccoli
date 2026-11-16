//! The threads guests run on.
//!
//! Guest calls never use tokio's blocking pool. Admission
//! ([`crate::admission`]) bounds how many calls hold a thread at once
//! ([`Caps::threads`](crate::admission::Caps::threads)), so this pool never
//! has to make a call wait: a job goes to an idle thread or to a new one. A
//! call therefore never holds a CPU token while it waits for a thread, and the
//! thread count is the sum of explicit caps, not whatever tokio's pool allows.
//!
//! Each thread owns its job channel and offers it on the idle list between
//! jobs; a thread idle for [`IDLE_EXIT`] leaves, so a burst's threads do not
//! stay for the life of the process.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use parking_lot::Mutex;

/// How long a thread with nothing to run stays.
pub const IDLE_EXIT: Duration = Duration::from_secs(30);

/// Thread name, so guest threads are told apart from tokio's.
pub const THREAD_NAME: &str = "plugin-guest";

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Inner {
    /// Job channels of the threads waiting for work, most recently idle last.
    idle: Mutex<Vec<(usize, SyncSender<Job>)>>,
    live: AtomicUsize,
    next_id: AtomicUsize,
}

/// The guest thread pool. Cheap to clone; clones share one pool.
#[derive(Clone, Default)]
pub struct GuestThreads(Arc<Inner>);

impl GuestThreads {
    /// Run `job` on an idle thread, or on a new one if none is idle. A panic
    /// in `job` ends the job, not the thread.
    pub fn run(&self, job: impl FnOnce() + Send + 'static) {
        let mut job: Job = Box::new(move || {
            let _ = catch_unwind(AssertUnwindSafe(job));
        });
        loop {
            let idle = self.0.idle.lock().pop();
            let Some((_, tx)) = idle else {
                break;
            };
            // A thread whose idle wait just ended may have dropped its
            // receiver: try the next one.
            match tx.try_send(job) {
                Ok(()) => return,
                Err(
                    std::sync::mpsc::TrySendError::Full(back)
                    | std::sync::mpsc::TrySendError::Disconnected(back),
                ) => job = back,
            }
        }
        self.spawn(job);
    }

    /// Threads alive now, running or idle.
    pub fn live(&self) -> usize {
        self.0.live.load(Ordering::Acquire)
    }

    fn spawn(&self, first: Job) {
        let pool = self.clone();
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        self.0.live.fetch_add(1, Ordering::AcqRel);
        let spawned = std::thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || pool.work(id, first));
        if let Err(e) = spawned {
            self.0.live.fetch_sub(1, Ordering::AcqRel);
            panic!("cannot start a plugin guest thread: {e}");
        }
    }

    fn work(self, id: usize, first: Job) {
        first();
        let (tx, rx): (SyncSender<Job>, Receiver<Job>) = sync_channel(1);
        loop {
            self.0.idle.lock().push((id, tx.clone()));
            match rx.recv_timeout(IDLE_EXIT) {
                Ok(job) => job(),
                Err(_) => {
                    let mut idle = self.0.idle.lock();
                    let offered = idle.iter().position(|(other, _)| *other == id);
                    match offered {
                        // Nobody took this thread: leave.
                        Some(at) => {
                            idle.remove(at);
                            drop(idle);
                            self.0.live.fetch_sub(1, Ordering::AcqRel);
                            return;
                        }
                        // A job was handed over as the wait ended; run it.
                        None => {
                            drop(idle);
                            if let Ok(job) = rx.recv() {
                                job();
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::mpsc::channel;

    #[test]
    fn concurrent_jobs_each_get_a_thread_and_threads_are_reused() {
        let pool = GuestThreads::default();
        // Four jobs that all wait for each other: they can only finish if
        // each runs on its own thread at once.
        let barrier = Arc::new(Barrier::new(4));
        let (done_tx, done) = channel();
        for _ in 0..4 {
            let (barrier, done_tx) = (barrier.clone(), done_tx.clone());
            pool.run(move || {
                barrier.wait();
                done_tx.send(()).unwrap();
            });
        }
        for _ in 0..4 {
            done.recv().unwrap();
        }
        assert_eq!(pool.live(), 4);
        // Once they are idle, the next jobs reuse them.
        while pool.0.idle.lock().len() < 4 {
            std::thread::yield_now();
        }
        for _ in 0..4 {
            let done_tx = done_tx.clone();
            pool.run(move || done_tx.send(()).unwrap());
        }
        for _ in 0..4 {
            done.recv().unwrap();
        }
        assert_eq!(pool.live(), 4);
    }

    #[test]
    fn a_panicking_job_leaves_its_thread_usable() {
        let pool = GuestThreads::default();
        pool.run(|| panic!("guest bug"));
        while pool.0.idle.lock().is_empty() {
            std::thread::yield_now();
        }
        let (tx, rx) = channel();
        pool.run(move || {
            tx.send(std::thread::current().name().map(str::to_owned))
                .unwrap()
        });
        assert_eq!(rx.recv().unwrap().as_deref(), Some(THREAD_NAME));
        assert_eq!(pool.live(), 1);
    }
}
