//! Waiting on the server for a submission or a code run to finish: one loop
//! for `submit -w` and a remote `test`, so both say what they wait for and
//! give up the same way.

use std::time::{Duration, Instant};

use broccoli_cli_core::model::{Standing, SubmissionStatus};
use broccoli_cli_core::tui::progress::Progress;

/// The wait gave up while the work was still queued or judging. Nothing was
/// cancelled: the server still judges it, and `check_later` says where to
/// look. `broccoli` exits with [`StillWaiting::EXIT_CODE`].
#[derive(Debug)]
pub struct StillWaiting {
    pub waited: Duration,
    pub waiting_for: String,
    pub check_later: String,
}

impl StillWaiting {
    pub const EXIT_CODE: u8 = 4;
}

impl std::fmt::Display for StillWaiting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Stopped waiting after {}s ({}). It is not cancelled: {}",
            self.waited.as_secs(),
            self.waiting_for,
            self.check_later
        )
    }
}

impl std::error::Error for StillWaiting {}

/// How long to sleep between polls of work standing at `standing`: quick
/// while it is judged, slower while it waits for a judge, and slow while it
/// waits for a plugin an administrator has to bring back.
fn interval(standing: &Standing) -> Duration {
    match standing {
        Standing::WaitingForPlugin(_) | Standing::Parked(_) => Duration::from_secs(5),
        Standing::Status(SubmissionStatus::Queued) => Duration::from_secs(1),
        _ => Duration::from_millis(250),
    }
}

/// Poll until the work is final, for at most `limit` before giving up with
/// [`StillWaiting`], and return the last answer with how long it took. On a
/// terminal the wait line is redrawn in place; anywhere else each new thing
/// waited for is printed once, so a pipe or a log shows it too.
pub fn until_final<T>(
    limit: Duration,
    check_later: &str,
    poll: impl FnMut() -> anyhow::Result<(Standing, T)>,
) -> anyhow::Result<(T, Duration)> {
    let mut progress = Progress::stderr();
    let start = Instant::now();
    let mut said = None;
    run(
        limit,
        check_later,
        poll,
        || start.elapsed(),
        std::thread::sleep,
        |waiting_for, elapsed| {
            if progress.is_live() {
                progress.tick(format_args!(
                    "{waiting_for} ({:.0}s)",
                    elapsed.as_secs_f64()
                ));
            } else if said.as_deref() != Some(waiting_for) {
                eprintln!("  {waiting_for}");
                said = Some(waiting_for.to_string());
            }
        },
    )
}

/// [`until_final`] with the clock, the sleep and the wait line passed in.
fn run<T>(
    limit: Duration,
    check_later: &str,
    mut poll: impl FnMut() -> anyhow::Result<(Standing, T)>,
    elapsed: impl Fn() -> Duration,
    mut sleep: impl FnMut(Duration),
    mut show: impl FnMut(&str, Duration),
) -> anyhow::Result<(T, Duration)> {
    loop {
        let (standing, answer) = poll()?;
        let waited = elapsed();
        let Some(waiting_for) = standing.waiting_for() else {
            return Ok((answer, waited));
        };
        if waited >= limit {
            return Err(StillWaiting {
                waited,
                waiting_for,
                check_later: check_later.to_string(),
            }
            .into());
        }
        show(&waiting_for, waited);
        sleep(interval(&standing));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use broccoli_cli_core::api::visibility::PluginOutage;

    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    /// Drive [`run`] over `answers` (the last one repeats), on a clock that
    /// only the sleeps move. Each poll answers with its index.
    fn drive(answers: Vec<Standing>) -> (anyhow::Result<(usize, Duration)>, Vec<String>) {
        let now = Cell::new(Duration::ZERO);
        let mut polls = 0;
        let mut shown = Vec::new();
        let result = run(
            MINUTE,
            "check later",
            || {
                let i = polls.min(answers.len() - 1);
                polls += 1;
                Ok((answers[i].clone(), i))
            },
            || now.get(),
            |d| now.set(now.get() + d),
            |line, _| shown.push(line.to_string()),
        );
        (result, shown)
    }

    #[test]
    fn a_verdict_ends_the_wait() {
        let (result, shown) = drive(vec![
            Standing::Status(SubmissionStatus::Queued),
            Standing::Status(SubmissionStatus::Running),
            Standing::Status(SubmissionStatus::Judged),
        ]);
        let (answer, waited) = result.unwrap();
        assert_eq!(answer, 2);
        assert_eq!(waited, Duration::from_millis(1250));
        assert_eq!(
            shown,
            ["Queued: waiting for a judge to take it", "Judging: Running"]
        );
    }

    #[test]
    fn a_hidden_outcome_ends_the_wait() {
        let (result, shown) = drive(vec![Standing::Hidden]);
        assert_eq!(result.unwrap().0, 0);
        assert!(shown.is_empty());
    }

    #[test]
    fn work_no_judge_takes_gives_up_saying_it_is_still_queued() {
        let (result, shown) = drive(vec![Standing::Status(SubmissionStatus::Queued)]);
        let err = result.unwrap_err();
        let still = err.downcast_ref::<StillWaiting>().unwrap();
        assert_eq!(still.waited, MINUTE);
        assert_eq!(still.waiting_for, "Queued: waiting for a judge to take it");
        assert!(err.to_string().contains("It is not cancelled: check later"));
        assert_eq!(shown.len(), 60);
    }

    #[test]
    fn a_plugin_outage_is_waited_on_at_a_slow_pace() {
        let (result, shown) = drive(vec![Standing::WaitingForPlugin(PluginOutage::Unavailable)]);
        assert!(result.unwrap_err().downcast_ref::<StillWaiting>().is_some());
        assert_eq!(shown.len(), 12);
    }
}
