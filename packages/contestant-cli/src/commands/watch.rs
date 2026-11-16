use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::*;

use broccoli_cli_core::api::clarification::{ClarificationListResponse, ClarificationResponse};
use broccoli_cli_core::api::submission::SubmissionListResponse;
use broccoli_cli_core::backoff;
use broccoli_cli_core::client::{Client, ClientError, ClientResult};
use broccoli_cli_core::config;
use broccoli_cli_core::fmt;
use broccoli_cli_core::model::{
    ClarificationKind, StatusDisplay, SubmissionStatus, Verdict, VerdictDisplay,
};
use broccoli_cli_core::session::SessionError;
use broccoli_cli_core::tui::theme::THEME;
use chrono::{DateTime, Utc};

/// RAII guard that restores the terminal on drop, even on panic.
struct TerminalGuard {
    stdout: io::Stdout,
}

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        let mut stdout = io::stdout();
        enable_raw_mode()?;
        stdout.execute(EnterAlternateScreen)?;
        Ok(Self { stdout })
    }

    fn stdout(&mut self) -> &mut io::Stdout {
        &mut self.stdout
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = self.stdout.execute(LeaveAlternateScreen);
    }
}

#[derive(Args)]
pub struct WatchArgs {
    /// Contest ID or name (e.g. 3 or "Spring Round")
    pub contest_id: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    MySubmissions,
    Problems,
    Clarifications,
}

impl Tab {
    fn next(self) -> Tab {
        match self {
            Tab::MySubmissions => Tab::Problems,
            Tab::Problems => Tab::Clarifications,
            Tab::Clarifications => Tab::MySubmissions,
        }
    }
    fn prev(self) -> Tab {
        match self {
            Tab::MySubmissions => Tab::Clarifications,
            Tab::Problems => Tab::MySubmissions,
            Tab::Clarifications => Tab::Problems,
        }
    }
}

struct SubmissionRow {
    id: String,
    problem_id: String,
    problem: String,
    status: SubmissionStatus,
    verdict: Option<Verdict>,
    score: Option<f64>,
    time_used: Option<i64>,
    memory_used: Option<i64>,
}

struct ProblemRow {
    label: String,
    title: String,
    problem_id: String,
    solved: bool,
}

struct ClarReply {
    author: String,
    content: String,
    is_public: bool,
    created_at: DateTime<Utc>,
}

struct ClarificationRow {
    id: String,
    author: String,
    content: String,
    kind: ClarificationKind,
    created_at: DateTime<Utc>,
    replies: Vec<ClarReply>,
    resolved: bool,
    resolved_at: Option<DateTime<Utc>>,
    resolved_by: Option<String>,
}

impl ClarificationRow {
    fn answered(&self) -> bool {
        !self.replies.is_empty()
    }
}

/// Modal overlay content; rebuilt from live app state every frame.
enum Overlay {
    /// Submission detail keyed by id; body lives in `AppData::submission_detail`.
    Submission { id: String, scroll: u16 },
    /// Static problem statement; `body` is the raw markdown for `o`.
    Problem {
        title: String,
        subtitle: String,
        body: String,
        scroll: u16,
    },
    /// Clarification thread keyed by id, rendered from live `clarifications`.
    Clarification { id: String, scroll: u16 },
}

impl Overlay {
    fn scroll(&self) -> u16 {
        match self {
            Overlay::Submission { scroll, .. }
            | Overlay::Problem { scroll, .. }
            | Overlay::Clarification { scroll, .. } => *scroll,
        }
    }

    fn scroll_mut(&mut self) -> &mut u16 {
        match self {
            Overlay::Submission { scroll, .. }
            | Overlay::Problem { scroll, .. }
            | Overlay::Clarification { scroll, .. } => scroll,
        }
    }
}

struct AppData {
    contest_id: String,
    contest_title: String,
    /// Contest end time for the live countdown.
    end_time: DateTime<Utc>,
    remaining: String,
    selected_tab: Tab,
    my_submissions: Vec<SubmissionRow>,
    problems: Vec<ProblemRow>,
    clarifications: Vec<ClarificationRow>,
    sel_sub: usize,
    sel_prob: usize,
    sel_clar: usize,
    overlay: Option<Overlay>,
    /// Open submission overlay's detail body, refreshed live by the poller.
    submission_detail: Option<broccoli_cli_core::api::submission::SubmissionResponse>,
    /// How the poller has been doing; drives the header warning.
    link: Link,
    /// Inner (width, height) of the overlay modal from the last render, for scroll clamping.
    overlay_viewport: Cell<(u16, u16)>,
    /// Reply count the user has seen per clarification id (absent means never opened).
    clar_acked: HashMap<String, usize>,
    /// Previous poll's reply counts, to detect new content for the flash.
    clar_prev: HashMap<String, usize>,
    /// False until the first poll, so existing items don't flash/badge on startup.
    clar_inited: bool,
    /// False until the first submissions poll, so pre-existing Accepted
    /// submissions don't flash "✓ Accepted" or steal the cursor on startup.
    subs_inited: bool,
    /// When `Some`, the in-progress clarification input text.
    compose: Option<String>,
    /// Transient one-line footer status.
    flash: Option<String>,
    /// When `flash` was first shown, so it fades after a few seconds.
    flash_at: Option<Instant>,
}

impl AppData {
    /// Drop everything that belonged to the previous user's session.
    fn forget_user(&mut self) {
        self.my_submissions.clear();
        self.subs_inited = false;
        self.submission_detail = None;
        if matches!(self.overlay, Some(Overlay::Submission { .. })) {
            self.overlay = None;
        }
        for p in &mut self.problems {
            p.solved = false;
        }
    }

    fn current_len(&self) -> usize {
        match self.selected_tab {
            Tab::MySubmissions => self.my_submissions.len(),
            Tab::Problems => self.problems.len(),
            Tab::Clarifications => self.clarifications.len(),
        }
    }

    fn selection(&mut self) -> &mut usize {
        match self.selected_tab {
            Tab::MySubmissions => &mut self.sel_sub,
            Tab::Problems => &mut self.sel_prob,
            Tab::Clarifications => &mut self.sel_clar,
        }
    }

    fn move_selection(&mut self, delta: i64) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let sel = self.selection();
        let new = (*sel as i64 + delta).clamp(0, len as i64 - 1);
        *sel = new as usize;
    }

    fn clamp_selections(&mut self) {
        self.sel_sub = self
            .sel_sub
            .min(self.my_submissions.len().saturating_sub(1));
        self.sel_prob = self.sel_prob.min(self.problems.len().saturating_sub(1));
        self.sel_clar = self
            .sel_clar
            .min(self.clarifications.len().saturating_sub(1));
    }

    /// New clarification, or a reply added since the user last viewed it.
    fn is_clar_unread(&self, c: &ClarificationRow) -> bool {
        match self.clar_acked.get(&c.id) {
            None => self.clar_inited, // unseen after startup = unread
            Some(&seen) => c.replies.len() > seen,
        }
    }

    fn unread_clar_count(&self) -> usize {
        self.clarifications
            .iter()
            .filter(|c| self.is_clar_unread(c))
            .count()
    }

    /// Mark a clarification read (call when its detail is opened).
    fn ack_clar(&mut self, id: &str, reply_count: usize) {
        self.clar_acked.insert(id.to_string(), reply_count);
    }
}

/// Time between polls while the server answers.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Longest wait between polls while the server keeps failing.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(30);

enum PollUpdate {
    Submissions(SubmissionListResponse),
    Clarifications(ClarificationListResponse),
    /// How the most recent poll cycle ended.
    Cycle(PollOutcome),
    /// A new login replaced a rejected session with a different user's.
    UserChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollOutcome {
    Ok,
    Failed,
    /// The session ended; polling resumes once `broccoli login` saves a new one.
    SessionRejected,
}

impl PollOutcome {
    /// The worse of two outcomes in one cycle.
    fn worst(self, other: Self) -> Self {
        use PollOutcome::*;
        match (self, other) {
            (SessionRejected, _) | (_, SessionRejected) => SessionRejected,
            (Failed, _) | (_, Failed) => Failed,
            (Ok, Ok) => Ok,
        }
    }

    fn of<T>(result: &ClientResult<T>) -> Self {
        match result {
            Ok(_) => Self::Ok,
            Err(ClientError::Session(SessionError::Rejected)) => Self::SessionRejected,
            Err(_) => Self::Failed,
        }
    }
}

/// The poller's connection to the server, as the header reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    Connected,
    Failing { cycles: u32 },
    SessionRejected,
}

impl Link {
    fn after(self, outcome: PollOutcome) -> Self {
        match (outcome, self) {
            (PollOutcome::Ok, _) => Self::Connected,
            (PollOutcome::SessionRejected, _) => Self::SessionRejected,
            (PollOutcome::Failed, Self::Failing { cycles }) => Self::Failing {
                cycles: cycles.saturating_add(1),
            },
            (PollOutcome::Failed, _) => Self::Failing { cycles: 1 },
        }
    }

    /// Header warning, if any. A single failed cycle is not worth one.
    fn warning(self) -> Option<&'static str> {
        match self {
            Self::Connected | Self::Failing { cycles: 0..=1 } => None,
            Self::Failing { .. } => Some("  ⚠ reconnecting… "),
            Self::SessionRejected => Some("  ⚠ session expired: run `broccoli login` "),
        }
    }
}

pub fn run(args: WatchArgs) -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let client = Client::new(creds);

    let contest_id = broccoli_cli_core::resolve::contest_id(&client, &args.contest_id)?;

    let contest = client.get_contest(&contest_id)?;
    let problems = client
        .list_contest_problems(&contest_id)
        .unwrap_or_default();
    // Must scope the submissions poll to our own uid; an unscoped fallback would
    // leak every participant's submissions on a `submissions_visible` contest.
    let self_uid = {
        let mut uid = None;
        for attempt in 0..3 {
            match client.me() {
                Ok(m) => {
                    uid = Some(m.id);
                    break;
                }
                Err(_) if attempt < 2 => thread::sleep(Duration::from_millis(300)),
                Err(e) => {
                    return Err(e).context(
                        "Could not determine your account (GET /auth/me failed). \
                     Check your connection and run `broccoli watch` again.",
                    );
                }
            }
        }
        uid.expect("loop sets uid or returns")
    };

    let remaining = calculate_remaining(&contest.end_time);

    let mut app = AppData {
        contest_id: contest_id.clone(),
        contest_title: contest.title,
        end_time: contest.end_time,
        remaining,
        selected_tab: Tab::MySubmissions,
        my_submissions: Vec::new(),
        problems: problems
            .iter()
            .map(|p| ProblemRow {
                label: p.label.clone(),
                title: p.problem_title.clone(),
                problem_id: p.problem_id.to_string(),
                solved: false,
            })
            .collect(),
        clarifications: Vec::new(),
        sel_sub: 0,
        sel_prob: 0,
        sel_clar: 0,
        overlay: None,
        submission_detail: None,
        link: Link::Connected,
        overlay_viewport: Cell::new((0, 0)),
        clar_acked: HashMap::new(),
        clar_prev: HashMap::new(),
        clar_inited: false,
        subs_inited: false,
        compose: None,
        flash: None,
        flash_at: None,
    };

    // One shared Client across the poller and UI threads. Its Session
    // serializes token refresh, so concurrent calls never double-rotate the
    // refresh token.
    let client = Arc::new(client);

    let (tx, rx) = mpsc::channel::<PollUpdate>();
    let cid = contest_id.clone();
    let mut uid = self_uid;

    let running = Arc::new(AtomicBool::new(true));
    let poller_running = running.clone();
    // 'r' key sets this so the poller wakes early and refreshes instantly.
    let refresh_now = Arc::new(AtomicBool::new(false));
    let poller_refresh = refresh_now.clone();
    let poll_client = Arc::clone(&client);

    let poller = thread::spawn(move || {
        let mut last = PollOutcome::Ok;
        let mut failures = 0u32;
        'outer: while poller_running.load(Ordering::Relaxed) {
            // A rejected session resumes only through a new login, which may
            // be another user's: re-read who we are before scoping the polls.
            if last == PollOutcome::SessionRejected
                && let Ok(me) = poll_client.me()
                && me.id != uid
            {
                uid = me.id;
                if tx.send(PollUpdate::UserChanged).is_err() {
                    break 'outer;
                }
            }
            let subs = poll_client.list_contest_submissions(
                &cid,
                None,
                Some(&uid.to_string()),
                None,
                Some(100),
            );
            let mut outcome = PollOutcome::of(&subs);
            if let Ok(d) = subs
                && tx.send(PollUpdate::Submissions(d)).is_err()
            {
                break 'outer;
            }
            let clars = poll_client.list_clarifications(&cid);
            outcome = outcome.worst(PollOutcome::of(&clars));
            if let Ok(d) = clars
                && tx.send(PollUpdate::Clarifications(d)).is_err()
            {
                break 'outer;
            }
            if tx.send(PollUpdate::Cycle(outcome)).is_err() {
                break 'outer;
            }
            last = outcome;
            failures = if outcome == PollOutcome::Failed {
                failures + 1
            } else {
                0
            };
            // Every POLL_INTERVAL, backing off while the server keeps failing;
            // `r` wakes the poller early.
            let wait = match failures {
                0 => POLL_INTERVAL,
                n => backoff::exponential(POLL_INTERVAL, n, MAX_POLL_INTERVAL),
            };
            let deadline = Instant::now() + wait;
            while Instant::now() < deadline {
                if !poller_running.load(Ordering::Relaxed)
                    || poller_refresh.swap(false, Ordering::Relaxed)
                {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    });

    let mut guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(guard.stdout());
    let mut terminal = Terminal::new(backend)?;

    let loop_result = run_event_loop(&mut terminal, &mut app, &rx, &client, &refresh_now);

    running.store(false, Ordering::Relaxed);
    drop(terminal);
    drop(rx);
    let _ = poller.join();

    loop_result
}

/// Interactive render/input loop, factored out so the terminal guard always restores.
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<&mut io::Stdout>>,
    app: &mut AppData,
    rx: &mpsc::Receiver<PollUpdate>,
    client: &Arc<Client>,
    refresh_now: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    loop {
        // Both can flash in one cycle; keep the highest-priority one (fresh AC beats reply).
        let mut subs_changed = false;
        let mut got_poll = false;
        let mut pending_flash: Option<(u8, String)> = None;
        let consider = |prio: u8, msg: Option<String>, flash: &mut Option<(u8, String)>| {
            if let Some(msg) = msg
                && flash.as_ref().is_none_or(|(p, _)| prio >= *p)
            {
                *flash = Some((prio, msg));
            }
        };
        while let Ok(update) = rx.try_recv() {
            got_poll = true;
            match update {
                PollUpdate::Submissions(d) => {
                    // Snapshot already-Accepted ids to spot a fresh AC after the update.
                    let was_accepted: HashSet<String> = app
                        .my_submissions
                        .iter()
                        .filter(|s| s.verdict.as_ref().is_some_and(|v| v.is_accepted()))
                        .map(|s| s.id.clone())
                        .collect();
                    update_submissions(app, &d);
                    if app.subs_inited {
                        consider(2, notify_new_accept(app, &was_accepted), &mut pending_flash);
                    } else {
                        // First poll: adopt existing submissions as the baseline so a
                        // pre-existing AC doesn't flash or jump the cursor (mirrors
                        // clar_inited for clarifications).
                        app.subs_inited = true;
                    }
                    subs_changed = true;
                }
                PollUpdate::Clarifications(d) => {
                    update_clarifications(app, &d);
                    consider(1, notify_new_clar(app), &mut pending_flash);
                }
                PollUpdate::Cycle(outcome) => app.link = app.link.after(outcome),
                PollUpdate::UserChanged => app.forget_user(),
            }
            app.clamp_selections();
        }
        if let Some((_, msg)) = pending_flash {
            app.flash = Some(msg);
            app.flash_at = None; // restamp the freshly-set flash
        }
        // Clear the "Refreshing..." toast once fresh data lands.
        if got_poll && app.flash.as_deref() == Some("Refreshing…") {
            app.flash = None;
            app.flash_at = None;
        }

        // Stop re-fetching an open submission overlay once it reaches a terminal verdict.
        if subs_changed && let Some(Overlay::Submission { id, .. }) = app.overlay.as_ref() {
            let still_judging = app
                .submission_detail
                .as_ref()
                .map(|s| s.status.is_in_progress())
                .unwrap_or(true);
            if still_judging {
                let id = id.clone();
                if let Ok(sub) = client.get_submission(&id) {
                    app.submission_detail = Some(sub);
                }
            }
        }

        app.remaining = calculate_remaining(&app.end_time);

        // Stamp a fresh flash, then fade it after a few seconds.
        if app.flash.is_none() {
            app.flash_at = None;
        } else if app.flash_at.is_none() {
            app.flash_at = Some(Instant::now());
        } else if app
            .flash_at
            .is_some_and(|t| t.elapsed() > Duration::from_secs(4))
        {
            app.flash = None;
            app.flash_at = None;
        }

        terminal.draw(|f| render(f, app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // Any keypress dismisses the current flash.
        app.flash = None;
        app.flash_at = None;

        if app.compose.is_some() {
            match key.code {
                KeyCode::Esc => app.compose = None,
                KeyCode::Backspace => {
                    app.compose.as_mut().expect("composing").pop();
                }
                KeyCode::Char(c) => app.compose.as_mut().expect("composing").push(c),
                KeyCode::Enter => submit_clarification(app, client, refresh_now),
                _ => {}
            }
            continue;
        }

        if app.overlay.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.overlay = None;
                    app.submission_detail = None;
                }
                KeyCode::Down | KeyCode::Char('j') => scroll_overlay(app, 1),
                KeyCode::Up | KeyCode::Char('k') => scroll_overlay(app, -1),
                KeyCode::PageDown => scroll_overlay(app, 10),
                KeyCode::PageUp => scroll_overlay(app, -10),
                KeyCode::Char('g') | KeyCode::Home => scroll_overlay(app, i64::MIN),
                KeyCode::Char('G') | KeyCode::End => scroll_overlay(app, i64::MAX),
                KeyCode::Char('o') => open_problem_externally(terminal, app)?,
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('1') => app.selected_tab = Tab::MySubmissions,
            KeyCode::Char('2') => app.selected_tab = Tab::Problems,
            KeyCode::Char('3') => app.selected_tab = Tab::Clarifications,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                app.selected_tab = app.selected_tab.next()
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                app.selected_tab = app.selected_tab.prev()
            }
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Char('r') => {
                refresh_now.store(true, Ordering::Relaxed);
                app.flash = Some("Refreshing…".to_string());
            }
            KeyCode::Char('a') => app.compose = Some(String::new()),
            KeyCode::Enter => open_detail(app, client),
            _ => {}
        }
    }
    Ok(())
}

fn scroll_overlay(app: &mut AppData, delta: i64) {
    // Clamp against the wrapped, viewport-aware row count so we never scroll past the end.
    let (vw, vh) = app.overlay_viewport.get();
    let max = overlay_max_scroll(app, vw, vh);
    if let Some(ov) = app.overlay.as_mut() {
        let s = ov.scroll_mut();
        *s = (*s as i64).saturating_add(delta).clamp(0, max) as u16;
    }
}

/// Max scroll offset via ratatui's `line_count` so wrap math matches the render path exactly.
fn overlay_max_scroll(app: &AppData, vw: u16, vh: u16) -> i64 {
    if vw == 0 {
        return 0;
    }
    let rows = Paragraph::new(overlay_lines(app))
        .wrap(Wrap { trim: false })
        .line_count(vw);
    rows.saturating_sub(vh.max(1) as usize)
        .min(u16::MAX as usize) as i64
}

fn open_detail(app: &mut AppData, client: &Client) {
    app.submission_detail = None;
    match app.selected_tab {
        Tab::MySubmissions => {
            let Some(row) = app.my_submissions.get(app.sel_sub) else {
                return;
            };
            let id = row.id.clone();
            match client.get_submission(&id) {
                Ok(sub) => {
                    app.submission_detail = Some(sub);
                    app.overlay = Some(Overlay::Submission { id, scroll: 0 });
                }
                Err(e) => app.flash = Some(format!("Could not load submission: {}", e)),
            }
        }
        Tab::Problems => {
            let Some(row) = app.problems.get(app.sel_prob) else {
                return;
            };
            match client.get_contest_problem(&app.contest_id, &row.problem_id) {
                Ok(p) => {
                    app.overlay = Some(Overlay::Problem {
                        title: format!("{} (problem {})", p.title, p.id),
                        subtitle: format!(
                            "Time limit: {}   Memory: {}",
                            fmt::time_ms(p.time_limit as i64),
                            fmt::memory_kb(p.memory_limit as i64)
                        ),
                        body: p.content.clone(),
                        scroll: 0,
                    });
                }
                Err(e) => app.flash = Some(format!("Could not load problem: {}", e)),
            }
        }
        Tab::Clarifications => {
            if let Some(row) = app.clarifications.get(app.sel_clar) {
                let (id, replies) = (row.id.clone(), row.replies.len());
                app.overlay = Some(Overlay::Clarification {
                    id: id.clone(),
                    scroll: 0,
                });
                app.ack_clar(&id, replies);
            }
        }
    }
}

/// Submit the compose buffer and force a refresh; keeps the text on failure.
fn submit_clarification(app: &mut AppData, client: &Arc<Client>, refresh_now: &Arc<AtomicBool>) {
    let Some(content) = app.compose.take().map(|c| c.trim().to_string()) else {
        return;
    };
    if content.is_empty() {
        app.flash = Some("Nothing to ask — clarification cancelled.".to_string());
        return;
    }
    match client.create_clarification(&app.contest_id, &content) {
        Ok(cl) => {
            app.flash = Some(format!("✓ Clarification #{} submitted", cl.id));
            refresh_now.store(true, Ordering::Relaxed);
        }
        Err(e) => {
            app.compose = Some(content); // keep the draft on failure
            app.flash = Some(format!("Could not submit: {}", e));
        }
    }
}

/// Live title for the current overlay (empty if none is open).
fn overlay_title(app: &AppData) -> String {
    match app.overlay.as_ref() {
        Some(Overlay::Submission { id, .. }) => match app.submission_detail.as_ref() {
            Some(s) if !s.problem_title.is_empty() => {
                format!(" Submission #{} · {} ", id, s.problem_title)
            }
            _ => format!(" Submission #{} ", id),
        },
        Some(Overlay::Problem { title, .. }) => format!(" {} ", title),
        Some(Overlay::Clarification { id, .. }) => format!(" Clarification #{} ", id),
        None => String::new(),
    }
}

/// Rebuild the overlay body from live app state; called every frame.
fn overlay_lines(app: &AppData) -> Vec<Line<'static>> {
    match app.overlay.as_ref() {
        Some(Overlay::Submission { .. }) => match app.submission_detail.as_ref() {
            Some(sub) => build_submission_lines(sub),
            None => vec![colored("(loading…)", THEME.muted, false)],
        },
        Some(Overlay::Problem {
            title,
            subtitle,
            body,
            ..
        }) => build_problem_lines(title, subtitle, body),
        Some(Overlay::Clarification { id, .. }) => {
            match app.clarifications.iter().find(|c| &c.id == id) {
                Some(row) => build_clarification_lines(row),
                None => vec![colored(
                    "(clarification no longer available)",
                    THEME.muted,
                    false,
                )],
            }
        }
        None => Vec::new(),
    }
}

fn plain(s: impl Into<String>) -> Line<'static> {
    Line::from(s.into())
}
fn field(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<9}", label), Style::default().fg(THEME.muted)),
        Span::raw(value.into()),
    ])
}
fn colored(value: impl Into<String>, color: Color, bold: bool) -> Line<'static> {
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(value.into(), style))
}

/// A timestamp in local time.
fn format_timestamp(ts: &DateTime<Utc>) -> String {
    ts.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn build_submission_lines(
    sub: &broccoli_cli_core::api::submission::SubmissionResponse,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(field("Language", sub.language.clone()));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "Status"), Style::default().fg(THEME.muted)),
        Span::styled(
            sub.status.human().to_string(),
            Style::default().fg(sub.status.color()),
        ),
    ]));
    if let Some(r) = sub.result.as_ref() {
        if let Some(v) = r.verdict.as_ref() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<9}", "Verdict"),
                    Style::default().fg(THEME.muted),
                ),
                Span::styled(
                    v.human().to_string(),
                    Style::default().fg(v.color()).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        if let Some(s) = r.score {
            lines.push(field("Score", fmt::score(s)));
        }
        lines.push(field(
            "Time",
            r.time_used
                .map(|t| fmt::time_ms(t as i64))
                .unwrap_or_else(|| "—".into()),
        ));
        lines.push(field(
            "Memory",
            r.memory_used
                .map(|m| fmt::memory_kb(m as i64))
                .unwrap_or_else(|| "—".into()),
        ));
        if let Some(msg) = r.error_message.as_deref().filter(|m| !m.is_empty()) {
            lines.push(plain(""));
            lines.push(colored("Error:", THEME.error, true));
            lines.extend(
                msg.lines()
                    .map(|l| colored(format!("  {}", l), THEME.error, false)),
            );
        }
        if let Some(co) = r.compile_output.as_deref().filter(|c| !c.is_empty()) {
            lines.push(plain(""));
            lines.push(colored("Compile output:", THEME.warning, true));
            lines.extend(co.lines().map(|l| {
                Line::from(Span::styled(
                    format!("  {}", l),
                    Style::default().fg(THEME.muted),
                ))
            }));
        }
        if !r.test_case_results.is_empty() {
            lines.push(plain(""));
            lines.push(colored(
                format!("Test cases ({}):", r.test_case_results.len()),
                THEME.primary,
                true,
            ));
            for (i, tc) in r.test_case_results.iter().enumerate() {
                let verdict = tc.verdict.clone().unwrap_or(Verdict::Other("?".into()));
                let t = tc
                    .time_used
                    .map(|t| fmt::time_ms(t as i64))
                    .unwrap_or_default();
                let text = format!(
                    "  #{:<3} {:<22} {:>4}  {}",
                    i + 1,
                    verdict.human(),
                    format!("{:.0}", tc.score.unwrap_or(0.0)),
                    t
                );
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(verdict.color()),
                )));
            }
        }
    } else {
        lines.push(plain(""));
        lines.push(colored("(awaiting judgement…)", THEME.muted, false));
    }
    lines
}

fn build_problem_lines(_title: &str, subtitle: &str, body: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if !subtitle.is_empty() {
        lines.push(colored(subtitle.to_string(), THEME.muted, false));
        lines.push(colored("─".repeat(40), THEME.muted, false));
        lines.push(plain(""));
    }
    for l in body.lines() {
        lines.push(plain(l.to_string()));
    }
    lines.push(plain(""));
    lines.push(colored(
        "Press 'o' to open in your editor/pager.",
        THEME.muted,
        false,
    ));
    lines
}

fn build_clarification_lines(row: &ClarificationRow) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let (status_text, status_color) = if row.resolved {
        ("● resolved".to_string(), THEME.success)
    } else if row.answered() {
        ("○ answered".to_string(), THEME.primary)
    } else {
        ("○ open".to_string(), THEME.warning)
    };
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "Type"), Style::default().fg(THEME.muted)),
        Span::styled(row.kind.human().to_string(), Style::default().fg(THEME.fg)),
        Span::raw("   "),
        Span::styled(
            status_text,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(field("From", row.author.clone()));
    lines.push(field("Asked", format_timestamp(&row.created_at)));
    if row.resolved
        && let Some(at) = row.resolved_at.as_ref()
    {
        let by = row
            .resolved_by
            .as_deref()
            .map(|b| format!(" by {}", b))
            .unwrap_or_default();
        lines.push(field("Resolved", format!("{}{}", format_timestamp(at), by)));
    }

    lines.push(plain(""));
    lines.push(colored("Question", THEME.fg, true));
    lines.extend(row.content.lines().map(|l| plain(format!("  {}", l))));
    lines.push(plain(""));

    if row.replies.is_empty() {
        lines.push(colored("(no answer yet)", THEME.muted, false));
    } else {
        lines.push(colored(
            format!("Replies ({})", row.replies.len()),
            THEME.success,
            true,
        ));
        for (i, reply) in row.replies.iter().enumerate() {
            if i > 0 {
                lines.push(plain(""));
            }
            let mut meta = vec![
                Span::styled(
                    format!("  {}", reply.author),
                    Style::default()
                        .fg(THEME.primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  · {}", format_timestamp(&reply.created_at)),
                    Style::default().fg(THEME.muted),
                ),
            ];
            if !reply.is_public {
                meta.push(Span::styled(
                    "  (private)".to_string(),
                    Style::default().fg(THEME.warning),
                ));
            }
            lines.push(Line::from(meta));
            lines.extend(
                reply
                    .content
                    .lines()
                    .map(|l| colored(format!("    {}", l), THEME.success, false)),
            );
        }
    }
    lines
}

/// Open the current problem statement in an external viewer, suspending the TUI.
fn open_problem_externally(
    terminal: &mut Terminal<CrosstermBackend<&mut io::Stdout>>,
    app: &mut AppData,
) -> anyhow::Result<()> {
    let Some(Overlay::Problem {
        title,
        subtitle,
        body,
        ..
    }) = app.overlay.as_ref()
    else {
        app.flash = Some("Open a problem statement first (Problems tab → Enter).".to_string());
        return Ok(());
    };
    // Self-contained export: title + limits, then body.
    let mut document = format!("# {}\n", title);
    if !subtitle.is_empty() {
        document.push_str(&format!("\n{}\n", subtitle));
    }
    document.push('\n');
    document.push_str(body);
    document.push('\n');

    // Random nonce + create_new: a predictable name in the shared temp dir lets a
    // co-located user pre-plant a symlink for the write to follow (cf. ScratchDir).
    let nonce: u64 = rand::random();
    let path = std::env::temp_dir().join(format!(
        "broccoli-problem-{}-{}-{:016x}.md",
        app.contest_id,
        std::process::id(),
        nonce
    ));
    let write_result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, document.as_bytes()));
    if write_result.is_err() {
        app.flash = Some("Could not write temp file for the viewer".to_string());
        return Ok(());
    }

    let (program, args) = resolve_viewer();

    // Suspend the TUI.
    disable_raw_mode().ok();
    io::stdout().execute(LeaveAlternateScreen).ok();

    // Pass the path as OsStr so non-UTF-8 Windows temp dirs aren't corrupted.
    let status = std::process::Command::new(&program)
        .args(&args)
        .arg(&path)
        .status();

    enable_raw_mode().ok();
    io::stdout().execute(EnterAlternateScreen).ok();
    terminal.clear().ok();
    let _ = std::fs::remove_file(&path);

    if status.is_err() {
        app.flash = Some(format!(
            "Could not launch viewer '{}'",
            program.to_string_lossy()
        ));
    }
    Ok(())
}

/// Pick an external viewer, preferring vim/nvim/vi (read-only), then `$PAGER`, then less/more.
fn resolve_viewer() -> (std::ffi::OsString, Vec<String>) {
    use std::ffi::OsString;
    use std::path::Path;

    // Prefer vim over `$PAGER` (often `less`): it gives search and syntax.
    for editor in ["vim", "nvim", "vi"] {
        if let Some(path) = find_executable(editor) {
            // -R: read-only, the statement isn't meant to be edited.
            return (path.into_os_string(), vec!["-R".to_string()]);
        }
    }
    if let Ok(p) = std::env::var("PAGER") {
        let p = p.trim();
        if !p.is_empty() {
            // Use a bare program or real path whole; only split a value with flags.
            if Path::new(p).is_file() || !p.contains(char::is_whitespace) {
                return (OsString::from(p), Vec::new());
            }
            let mut parts = p.split_whitespace();
            let program = parts.next().unwrap_or("less");
            return (OsString::from(program), parts.map(String::from).collect());
        }
    }
    for pager in ["less", "more"] {
        if let Some(path) = find_executable(pager) {
            return (path.into_os_string(), Vec::new());
        }
    }
    let fallback = if cfg!(windows) { "more" } else { "less" };
    (OsString::from(fallback), Vec::new())
}

/// Resolve `name` to a full PATH executable; only probes directly-launchable
/// extensions (never a `.cmd`/`.bat` shim `Command` can't exec).
fn find_executable(name: &str) -> Option<std::path::PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".com"]
    } else {
        &[""]
    };
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for ext in exts {
            let candidate = dir.join(format!("{}{}", name, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Seconds left until `end_time`, or `None` once the contest has ended.
fn remaining_seconds(end_time: &DateTime<Utc>) -> Option<i64> {
    let secs = (*end_time - Utc::now()).num_seconds();
    (secs > 0).then_some(secs)
}

fn calculate_remaining(end_time: &DateTime<Utc>) -> String {
    match remaining_seconds(end_time) {
        // Seconds included so the header visibly ticks.
        Some(secs) => format!(
            "{}h {:02}m {:02}s remaining",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        ),
        None => "Finished".into(),
    }
}

fn update_submissions(app: &mut AppData, data: &SubmissionListResponse) {
    app.my_submissions = data
        .data
        .iter()
        .map(|s| SubmissionRow {
            id: s.id.to_string(),
            problem_id: s.problem_id.to_string(),
            problem: s.problem_title.clone(),
            status: s.status,
            verdict: s.verdict.clone(),
            score: s.score,
            time_used: s.time_used.map(i64::from),
            memory_used: s.memory_used.map(i64::from),
        })
        .collect();

    let solved_ids: HashSet<&str> = app
        .my_submissions
        .iter()
        .filter(|s| s.verdict.as_ref().is_some_and(|v| v.is_accepted()))
        .map(|s| s.problem_id.as_str())
        .collect();
    // Solved is monotonic: keep the ✓ even when an early AC scrolls past the 100-row window.
    for p in &mut app.problems {
        p.solved = p.solved || solved_ids.contains(p.problem_id.as_str());
    }
}

/// On a fresh AC, jump the selection to it and return a confirmation flash.
fn notify_new_accept(app: &mut AppData, was_accepted: &HashSet<String>) -> Option<String> {
    let (idx, problem) = app
        .my_submissions
        .iter()
        .enumerate()
        .find(|(_, s)| {
            s.verdict.as_ref().is_some_and(|v| v.is_accepted()) && !was_accepted.contains(&s.id)
        })
        .map(|(i, s)| (i, s.problem.clone()))?;
    app.sel_sub = idx;
    Some(format!("✓ Accepted — {}", problem))
}

fn update_clarifications(app: &mut AppData, data: &ClarificationListResponse) {
    app.clarifications = data.data.iter().map(clarification_row).collect();
}

/// Flash for content that arrived since the last poll; announcements win over
/// replies for the single footer slot.
fn notify_new_clar(app: &mut AppData) -> Option<String> {
    let mut announcement: Option<String> = None;
    let mut reply: Option<String> = None;
    if app.clar_inited {
        for c in &app.clarifications {
            let prev = app.clar_prev.get(&c.id).copied();
            let is_new = prev.is_none();
            let new_reply = prev.is_some_and(|n| c.replies.len() > n);
            if !(is_new || new_reply) {
                continue;
            }
            if matches!(c.kind, ClarificationKind::Announcement) {
                announcement.get_or_insert_with(|| c.id.clone());
            } else if new_reply {
                reply.get_or_insert_with(|| c.id.clone());
            }
        }
    }

    // Snapshot reply counts for the next poll, and drop read-state for gone
    // clarifications so a reused id can't inherit a stale "seen" count.
    app.clar_prev = app
        .clarifications
        .iter()
        .map(|c| (c.id.clone(), c.replies.len()))
        .collect();
    app.clar_acked
        .retain(|id, _| app.clarifications.iter().any(|c| &c.id == id));

    if !app.clar_inited {
        // First poll: treat everything as already-seen so it doesn't flash or badge.
        app.clar_acked = app.clar_prev.clone();
        app.clar_inited = true;
        return None;
    }

    if let Some(id) = announcement {
        Some(format!(
            "★ Announcement #{} posted — see Clarifications (3)",
            id
        ))
    } else {
        reply.map(|id| {
            format!(
                "↳ New reply on clarification #{} — see Clarifications (3)",
                id
            )
        })
    }
}

fn clarification_row(c: &ClarificationResponse) -> ClarificationRow {
    ClarificationRow {
        id: c.id.to_string(),
        author: c.author_name.clone(),
        content: c.content.clone(),
        kind: c.clarification_type.parse().unwrap(), // Infallible
        created_at: c.created_at,
        replies: c
            .replies
            .iter()
            .map(|r| ClarReply {
                author: r.author_name.clone(),
                content: r.content.clone(),
                is_public: r.is_public,
                created_at: r.created_at,
            })
            .collect(),
        resolved: c.resolved,
        resolved_at: c.resolved_at,
        resolved_by: c.resolved_by_name.clone(),
    }
}

fn render(f: &mut Frame, app: &AppData) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(size);

    // Countdown turns amber under 5 min, red under 1 min.
    let clock_color = match remaining_seconds(&app.end_time) {
        Some(s) if s <= 60 => THEME.error,
        Some(s) if s <= 300 => THEME.warning,
        Some(_) => THEME.primary,
        None => THEME.muted,
    };
    let mut header_spans = vec![Span::styled(
        format!(" {} — {} ", app.contest_title, app.remaining),
        Style::default()
            .fg(clock_color)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(warning) = app.link.warning() {
        header_spans.push(Span::styled(
            warning,
            Style::default()
                .fg(THEME.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let header =
        Paragraph::new(Line::from(header_spans)).block(Block::default().borders(Borders::ALL));
    f.render_widget(header, chunks[0]);

    // Clarifications tab carries an unread badge, visible from any tab.
    let unread = app.unread_clar_count();
    let clar_tab = if unread > 0 {
        Line::from(vec![
            Span::raw("Clarifications "),
            Span::styled(
                format!("({})", unread),
                Style::default()
                    .fg(THEME.warning)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from("Clarifications")
    };
    let tabs = Tabs::new(vec![
        Line::from("Submissions"),
        Line::from("Problems"),
        clar_tab,
    ])
    .select(match app.selected_tab {
        Tab::MySubmissions => 0,
        Tab::Problems => 1,
        Tab::Clarifications => 2,
    })
    .style(Style::default().fg(THEME.muted))
    .highlight_style(
        Style::default()
            .fg(THEME.primary)
            .add_modifier(Modifier::BOLD),
    )
    .divider("│");
    f.render_widget(tabs, chunks[1]);

    match app.selected_tab {
        Tab::MySubmissions => render_submissions(f, app, chunks[2]),
        Tab::Problems => render_problems(f, app, chunks[2]),
        Tab::Clarifications => render_clarifications(f, app, chunks[2]),
    }

    let footer_text = if app.compose.is_some() {
        " type your question · Enter submit · Esc cancel ".to_string()
    } else if let Some(flash) = app.flash.as_deref() {
        format!(" {} ", flash)
    } else if let Some(ov) = app.overlay.as_ref() {
        // "open in pager" hint only applies to problem statements.
        if matches!(ov, Overlay::Problem { .. }) {
            " ↑↓/jk scroll · g/G top/bottom · o open in pager · Esc back · q quit ".to_string()
        } else {
            " ↑↓/jk scroll · g/G top/bottom · Esc back · q quit ".to_string()
        }
    } else {
        " ↑↓/jk select · Enter open · a ask · r refresh · q quit ".to_string()
    };
    // Only colour the flash red when it signals a problem.
    let footer_color = match app.flash.as_deref() {
        Some(f) if f.starts_with('✓') || f.starts_with("Refreshing") => THEME.success,
        Some(f) if f.starts_with('★') || f.starts_with('↳') => THEME.primary,
        Some(_) => THEME.error,
        None => THEME.muted,
    };
    f.render_widget(
        Paragraph::new(footer_text).style(Style::default().fg(footer_color)),
        chunks[3],
    );

    // Overlay / compose box, drawn last on top; mutually exclusive.
    if app.compose.is_some() {
        render_compose(f, app, size);
    } else if app.overlay.is_some() {
        render_overlay(f, app, size);
    }
}

fn render_compose(f: &mut Frame, app: &AppData, size: Rect) {
    let text = app.compose.as_deref().unwrap_or("");

    // ~70% width, min 20 cols, never over terminal width (clamp(20, w) panics when w < 20).
    let w = ((size.width as f32 * 0.7) as u16).clamp(20.min(size.width), size.width);
    let h = 3u16.min(size.height);
    let x = size.x + (size.width.saturating_sub(w)) / 2;
    let y = size.y + (size.height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(
            Style::default()
                .fg(THEME.primary)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(THEME.overlay_bg).fg(THEME.fg))
        .title(Span::styled(
            " Ask a clarification ",
            Style::default()
                .fg(THEME.primary)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));
    let inner_w = block.inner(area).width as usize;

    // Show the tail when the text overflows, keeping the caret visible.
    let with_cursor = format!("{}▏", text);
    let shown: String = if with_cursor.chars().count() > inner_w {
        with_cursor
            .chars()
            .skip(with_cursor.chars().count() - inner_w)
            .collect()
    } else {
        with_cursor
    };

    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(shown).block(block), area);
}

fn render_submissions(f: &mut Frame, app: &AppData, area: Rect) {
    if app.my_submissions.is_empty() {
        f.render_widget(
            Paragraph::new("No submissions yet.")
                .style(Style::default().fg(THEME.muted))
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }

    let header = Row::new(vec!["ID", "Problem", "Verdict", "Score", "Time", "Memory"]).style(
        Style::default()
            .fg(THEME.primary)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = app
        .my_submissions
        .iter()
        .map(|s| {
            // judged verdict if present, else lifecycle status
            let (label, color) = match s.verdict.as_ref() {
                Some(v) => (v.human().to_string(), v.color()),
                None => (s.status.human().to_string(), s.status.color()),
            };
            let score = s.score.map(fmt::score).unwrap_or_else(|| "—".into());
            let time = s.time_used.map(fmt::time_ms).unwrap_or_else(|| "—".into());
            let memory = s
                .memory_used
                .map(fmt::memory_kb)
                .unwrap_or_else(|| "—".into());
            Row::new(vec![
                s.id.clone(),
                s.problem.clone(),
                label,
                score,
                time,
                memory,
            ])
            .style(Style::default().fg(color))
        })
        .collect();
    let widths = [
        Constraint::Percentage(8),
        Constraint::Percentage(32),
        Constraint::Percentage(24),
        Constraint::Percentage(12),
        Constraint::Percentage(12),
        Constraint::Percentage(12),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Submissions "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.sel_sub));
    f.render_stateful_widget(table, area, &mut state);
}

fn render_problems(f: &mut Frame, app: &AppData, area: Rect) {
    if app.problems.is_empty() {
        f.render_widget(
            Paragraph::new("No problems.")
                .style(Style::default().fg(THEME.muted))
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = app
        .problems
        .iter()
        .map(|p| {
            let mark = if p.solved { "✓" } else { "•" };
            let color = if p.solved { THEME.success } else { THEME.fg };
            ListItem::new(format!(" {}  {}  {}", mark, p.label, p.title))
                .style(Style::default().fg(color))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Problems "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▌");
    let mut state = ListState::default().with_selected(Some(app.sel_prob));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_clarifications(f: &mut Frame, app: &AppData, area: Rect) {
    if app.clarifications.is_empty() {
        f.render_widget(
            Paragraph::new("No clarifications yet. Ask one with `broccoli clarifications ask`.")
                .style(Style::default().fg(THEME.muted))
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = app
        .clarifications
        .iter()
        .map(|c| {
            let (mark, color) = if c.resolved {
                ("✓", THEME.success)
            } else if c.answered() {
                ("↩", THEME.primary)
            } else {
                ("•", THEME.warning)
            };
            let preview: String = c
                .content
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            let badge = if c.answered() && !c.resolved {
                format!(
                    " [{} repl{}]",
                    c.replies.len(),
                    if c.replies.len() == 1 { "y" } else { "ies" }
                )
            } else {
                String::new()
            };
            let mut spans = vec![Span::styled(
                format!(" {}  {}: {}{}", mark, c.author, preview, badge),
                Style::default().fg(color),
            )];
            if app.is_clar_unread(c) {
                spans.push(Span::styled(
                    "  ● new",
                    Style::default()
                        .fg(THEME.warning)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Clarifications "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▌");
    let mut state = ListState::default().with_selected(Some(app.sel_clar));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_overlay(f: &mut Frame, app: &AppData, size: Rect) {
    // Centered modal taking ~85% of the screen.
    let w = (size.width as f32 * 0.85) as u16;
    let h = (size.height as f32 * 0.85) as u16;
    let w = w.min(size.width);
    let h = h.min(size.height);
    let x = size.x.saturating_add(size.width.saturating_sub(w) / 2);
    let y = size.y.saturating_add(size.height.saturating_sub(h) / 2);
    let area = Rect::new(x, y, w, h);

    // Drop shadow: dark rect offset one cell down/right, drawn first.
    if area.right() < size.right() && area.bottom() < size.bottom() {
        let shadow = Rect::new(area.x + 1, area.y + 1, area.width, area.height);
        f.render_widget(Clear, shadow);
        f.render_widget(
            Block::default().style(Style::default().bg(THEME.shadow)),
            shadow,
        );
    }

    f.render_widget(Clear, area);

    let title = overlay_title(app);
    let lines = overlay_lines(app);
    let scroll = app.overlay.as_ref().map(|o| o.scroll()).unwrap_or(0);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(
            Style::default()
                .fg(THEME.primary)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(THEME.overlay_bg).fg(THEME.fg))
        .title(Span::styled(
            title,
            Style::default()
                .fg(THEME.primary)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(Span::styled(
            " j/k scroll · g/G top/bottom · o open · q close ",
            Style::default().fg(THEME.muted),
        )))
        .padding(Padding::horizontal(1));

    // Cache the inner size so key handling can clamp scroll to the wrapped content.
    let inner = block.inner(area);
    app.overlay_viewport.set((inner.width, inner.height));

    let para = Paragraph::new(lines)
        .block(block)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use broccoli_cli_core::api::clarification::ClarificationReplyResponse;
    use broccoli_cli_core::api::shared::ListPagination;
    use broccoli_cli_core::api::submission::SubmissionListItem;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn pagination(total: usize) -> ListPagination {
        ListPagination {
            per_page: 100,
            total: Some(total as u64),
            total_pages: Some(1),
            has_more: false,
            next_cursor: None,
        }
    }

    fn submissions(items: &[(i32, i32, &str, Verdict)]) -> SubmissionListResponse {
        SubmissionListResponse {
            data: items
                .iter()
                .map(|(id, problem_id, title, verdict)| SubmissionListItem {
                    id: *id,
                    language: "cpp".into(),
                    status: SubmissionStatus::Judged,
                    verdict: Some(verdict.clone()),
                    user_id: 7,
                    username: "alice".into(),
                    problem_id: *problem_id,
                    problem_title: title.to_string(),
                    contest_id: Some(1),
                    contest_type: "icpc".into(),
                    judge_epoch: 0,
                    target_worker_id: None,
                    created_at: ts("2026-06-25T10:00:00Z"),
                    score: Some(if verdict.is_accepted() { 100.0 } else { 0.0 }),
                    time_used: Some(41),
                    memory_used: Some(2048),
                })
                .collect(),
            pagination: pagination(items.len()),
        }
    }

    fn clarification(
        id: i32,
        kind: &str,
        content: &str,
        replies: &[&str],
    ) -> ClarificationResponse {
        ClarificationResponse {
            id,
            contest_id: 1,
            author_id: 7,
            author_name: "alice".into(),
            content: content.into(),
            clarification_type: kind.into(),
            recipient_id: None,
            recipient_name: None,
            is_public: true,
            replies: replies
                .iter()
                .enumerate()
                .map(|(i, text)| ClarificationReplyResponse {
                    id: i as i32 + 1,
                    author_id: 1,
                    author_name: "judge".into(),
                    content: text.to_string(),
                    is_public: true,
                    created_at: ts("2026-06-25T10:01:00Z"),
                })
                .collect(),
            resolved: false,
            resolved_at: None,
            resolved_by: None,
            resolved_by_name: None,
            created_at: ts("2026-06-25T10:00:00Z"),
            updated_at: ts("2026-06-25T10:00:00Z"),
        }
    }

    fn clarifications(items: Vec<ClarificationResponse>) -> ClarificationListResponse {
        ClarificationListResponse { data: items }
    }

    fn sample_app() -> AppData {
        AppData {
            contest_id: "1".into(),
            contest_title: "Test Cup 2026".into(),
            end_time: ts("2099-01-01T00:00:00Z"),
            remaining: "2h 30m remaining".into(),
            selected_tab: Tab::MySubmissions,
            my_submissions: vec![
                SubmissionRow {
                    id: "1001".into(),
                    problem_id: "1".into(),
                    problem: "A Plus B".into(),
                    status: SubmissionStatus::Judged,
                    verdict: Some(Verdict::Accepted),
                    score: Some(100.0),
                    time_used: Some(41),
                    memory_used: Some(2048),
                },
                SubmissionRow {
                    id: "1002".into(),
                    problem_id: "2".into(),
                    problem: "Subarrays".into(),
                    status: SubmissionStatus::Judged,
                    verdict: Some(Verdict::WrongAnswer),
                    score: Some(0.0),
                    time_used: Some(33),
                    memory_used: Some(512),
                },
            ],
            problems: vec![
                ProblemRow {
                    label: "A".into(),
                    title: "A Plus B".into(),
                    problem_id: "1".into(),
                    solved: true,
                },
                ProblemRow {
                    label: "B".into(),
                    title: "Subarrays".into(),
                    problem_id: "2".into(),
                    solved: false,
                },
            ],
            clarifications: vec![ClarificationRow {
                id: "1".into(),
                author: "alice".into(),
                content: "Can we use the math library?".into(),
                kind: ClarificationKind::Question,
                created_at: ts("2026-06-25T10:00:00Z"),
                replies: vec![ClarReply {
                    author: "judge".into(),
                    content: "Yes.".into(),
                    is_public: true,
                    created_at: ts("2026-06-25T10:05:00Z"),
                }],
                resolved: true,
                resolved_at: Some(ts("2026-06-25T10:06:00Z")),
                resolved_by: Some("judge".into()),
            }],
            sel_sub: 0,
            sel_prob: 0,
            sel_clar: 0,
            overlay: None,
            submission_detail: None,
            link: Link::Connected,
            overlay_viewport: Cell::new((0, 0)),
            clar_acked: HashMap::new(),
            clar_prev: HashMap::new(),
            clar_inited: true,
            subs_inited: true,
            compose: None,
            flash: None,
            flash_at: None,
        }
    }

    fn line_text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.clone()).collect()
    }

    fn lines_text(lines: &[Line]) -> String {
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn render_to(app: &AppData, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        buffer_text(&terminal)
    }

    #[test]
    fn test_submissions_show_verdict_column() {
        let app = sample_app();
        let content = render_to(&app, 120, 30);
        assert!(
            content.contains("Verdict"),
            "should have a Verdict column header"
        );
        assert!(
            content.contains("Accepted"),
            "should show the Accepted verdict"
        );
        assert!(
            content.contains("Wrong Answer"),
            "should show the Wrong Answer verdict"
        );
        assert!(
            content.contains("41 ms"),
            "should show the humanized time column"
        );
    }

    #[test]
    fn test_render_problems_tab() {
        let mut app = sample_app();
        app.selected_tab = Tab::Problems;
        let content = render_to(&app, 120, 30);
        assert!(content.contains("A Plus B"));
        assert!(content.contains("Subarrays"));
        assert!(content.contains("✓"), "solved marker");
    }

    #[test]
    fn test_render_clarifications_tab() {
        let mut app = sample_app();
        app.selected_tab = Tab::Clarifications;
        let content = render_to(&app, 120, 30);
        assert!(content.contains("alice"));
        assert!(content.contains("math library"));
    }

    #[test]
    fn test_tab_next_prev_cycle() {
        assert!(matches!(Tab::MySubmissions.next(), Tab::Problems));
        assert!(matches!(Tab::Problems.next(), Tab::Clarifications));
        assert!(matches!(Tab::Clarifications.next(), Tab::MySubmissions));
        assert!(matches!(Tab::MySubmissions.prev(), Tab::Clarifications));
        assert!(matches!(Tab::Clarifications.prev(), Tab::Problems));
    }

    #[test]
    fn test_move_selection_clamps() {
        let mut app = sample_app(); // 2 submissions
        assert_eq!(app.sel_sub, 0);
        app.move_selection(-1);
        assert_eq!(app.sel_sub, 0);
        app.move_selection(1);
        assert_eq!(app.sel_sub, 1);
        app.move_selection(5); // clamps to last
        assert_eq!(app.sel_sub, 1);
    }

    #[test]
    fn test_compose_box_renders_input() {
        let mut app = sample_app();
        app.compose = Some("Can we use the math library?".into());
        let content = render_to(&app, 100, 30);
        assert!(
            content.contains("Ask a clarification"),
            "compose box title shown"
        );
        assert!(
            content.contains("math library"),
            "the typed text is shown in the box"
        );
        assert!(content.contains("Enter submit"));
    }

    #[test]
    fn test_modals_no_panic_on_tiny_terminal() {
        // Modal widths must not panic on a terminal narrower than 20 columns
        // (`clamp(20, width)` panics when width < 20).
        let sizes = [(15u16, 5u16), (5, 3), (1, 1), (3, 1)];

        let mut app = sample_app();
        app.compose = Some("你好世界 a very long question that overflows".into());
        for (w, h) in sizes {
            let _ = render_to(&app, w, h);
        }

        app.compose = None;
        app.overlay = Some(Overlay::Problem {
            title: "A Plus B".into(),
            subtitle: "Time limit: 1000 ms".into(),
            body: "read a and b\nprint a+b".into(),
            scroll: 0,
        });
        for (w, h) in sizes {
            let _ = render_to(&app, w, h);
        }
    }

    #[test]
    fn test_overlay_renders_and_scrolls() {
        let mut app = sample_app();
        let body = (0..50)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        app.overlay = Some(Overlay::Problem {
            title: "A Plus B".into(),
            subtitle: "Time limit: 1000 ms   Memory: 262144 KB".into(),
            body,
            scroll: 0,
        });
        let content = render_to(&app, 100, 30);
        assert!(content.contains("A Plus B"), "overlay title shown");
        scroll_overlay(&mut app, 5);
        assert_eq!(app.overlay.as_ref().unwrap().scroll(), 5);
        scroll_overlay(&mut app, i64::MAX); // G: bottom, clamped
        assert!(app.overlay.as_ref().unwrap().scroll() > 5);
        scroll_overlay(&mut app, i64::MIN); // g: top
        assert_eq!(app.overlay.as_ref().unwrap().scroll(), 0);
    }

    #[test]
    fn test_clarification_detail_shows_full_thread() {
        let app = sample_app();
        let joined = lines_text(&build_clarification_lines(&app.clarifications[0]));
        assert!(joined.contains("Question"));
        assert!(joined.contains("math library"));
        assert!(joined.contains("Replies (1)"));
        assert!(joined.contains("judge"));
        assert!(joined.contains("Yes."));
        assert!(joined.contains("resolved"));
        assert!(joined.contains("Resolved"));
    }

    #[test]
    fn clarification_rows_keep_every_threaded_reply() {
        let row = clarification_row(&clarification(
            7,
            "question",
            "Is N <= 1e9?",
            &["Yes.", "Updated: N <= 2e9."],
        ));
        assert_eq!(row.replies.len(), 2, "both replies retained");
        assert!(row.answered());
        assert!(!row.resolved);
    }

    #[test]
    fn test_clarification_overlay_updates_live() {
        let mut app = sample_app();
        app.clarifications[0].replies.clear();
        app.clarifications[0].resolved = false;
        app.overlay = Some(Overlay::Clarification {
            id: "1".into(),
            scroll: 0,
        });
        assert!(lines_text(&overlay_lines(&app)).contains("no answer yet"));

        // Poller delivers a reply.
        app.clarifications[0].replies.push(ClarReply {
            author: "judge".into(),
            content: "Answered now.".into(),
            is_public: true,
            created_at: ts("2026-06-25T11:00:00Z"),
        });
        let joined = lines_text(&overlay_lines(&app));
        assert!(joined.contains("Answered now."));
    }

    #[test]
    fn test_submission_overlay_shows_memory_and_color() {
        let app = sample_app();
        let content = render_to(&app, 120, 30);
        assert!(content.contains("Memory"), "Memory column header present");
    }

    #[test]
    fn test_clarification_unread_badge_and_flash() {
        let mut app = sample_app();
        app.clarifications.clear();
        app.clar_inited = false;
        app.clar_acked.clear();
        app.clar_prev.clear();

        // First poll acks existing items silently.
        let first = clarifications(vec![clarification(
            1,
            "question",
            "Can we use STL?",
            &["Yes"],
        )]);
        update_clarifications(&mut app, &first);
        let flash = notify_new_clar(&mut app);
        assert!(app.clar_inited);
        assert_eq!(app.unread_clar_count(), 0, "existing acked on first poll");
        assert!(flash.is_none());

        // New announcement is unread and flashes.
        let second = clarifications(vec![
            clarification(1, "question", "Can we use STL?", &["Yes"]),
            clarification(2, "announcement", "Time limit doubled for C.", &[]),
        ]);
        update_clarifications(&mut app, &second);
        let flash = notify_new_clar(&mut app);
        assert_eq!(app.unread_clar_count(), 1, "new announcement is unread");
        assert!(flash.as_deref().unwrap().contains("Announcement"));

        // Opening it clears its unread state.
        app.ack_clar("2", 0);
        assert_eq!(app.unread_clar_count(), 0);

        // New reply on an existing thread is unread and flashes.
        let third = clarifications(vec![
            clarification(
                1,
                "question",
                "Can we use STL?",
                &["Yes", "But not <bits/stdc++.h>"],
            ),
            clarification(2, "announcement", "Time limit doubled for C.", &[]),
        ]);
        update_clarifications(&mut app, &third);
        let flash = notify_new_clar(&mut app);
        assert_eq!(app.unread_clar_count(), 1, "new reply on #1 is unread");
        assert!(flash.as_deref().unwrap().contains("reply"));
    }

    #[test]
    fn test_update_submissions_parses_verdict_and_marks_solved() {
        let mut app = sample_app();
        for p in &mut app.problems {
            p.solved = false;
        }
        app.my_submissions.clear();
        let data = submissions(&[(1001, 1, "A Plus B", Verdict::Accepted)]);
        update_submissions(&mut app, &data);
        assert_eq!(app.my_submissions.len(), 1);
        let s = &app.my_submissions[0];
        assert_eq!(s.id, "1001");
        assert_eq!(s.problem_id, "1");
        assert_eq!(s.verdict, Some(Verdict::Accepted));
        assert_eq!(s.time_used, Some(41));
        assert_eq!(s.memory_used, Some(2048));
        assert!(
            app.problems
                .iter()
                .find(|p| p.problem_id == "1")
                .unwrap()
                .solved,
            "accepted submission marks its problem solved"
        );
        assert!(
            !app.problems
                .iter()
                .find(|p| p.problem_id == "2")
                .unwrap()
                .solved,
            "unrelated problem stays unsolved"
        );
    }

    #[test]
    fn test_verdict_rendered_humanized_not_raw_wire() {
        // Wire verdicts are PascalCase; the table must show the spaced human form.
        let mut app = sample_app();
        app.my_submissions.clear();
        let data = submissions(&[(1, 1, "P", Verdict::TimeLimitExceeded)]);
        update_submissions(&mut app, &data);
        let content = render_to(&app, 120, 30);
        assert!(
            content.contains("Time Limit Exceeded"),
            "humanized verdict shown"
        );
        assert!(
            !content.contains("TimeLimitExceeded"),
            "raw wire form must not leak"
        );
    }

    #[test]
    fn test_calculate_remaining() {
        let r = calculate_remaining(&ts("2099-01-01T00:00:00Z"));
        assert!(r.contains("remaining"));
        assert!(r.contains('s'), "countdown shows seconds: {r}");
        assert_eq!(calculate_remaining(&ts("2000-01-01T00:00:00Z")), "Finished");
    }

    #[test]
    fn test_overlay_scroll_reaches_wrapped_bottom() {
        // The wrapped bottom must stay reachable even when the row estimate undercounts.
        let mut app = sample_app();
        let body = (0..40)
            .map(|i| {
                format!("Paragraph {i} has several words that wrap across the modal width here.")
            })
            .collect::<Vec<_>>()
            .join("\n");
        app.overlay = Some(Overlay::Problem {
            title: "P".into(),
            subtitle: String::new(),
            body,
            scroll: 0,
        });
        // Render once to size the viewport, then scroll to the bottom.
        render_to(&app, 50, 18);
        scroll_overlay(&mut app, i64::MAX);
        let content = render_to(&app, 50, 18);
        assert!(
            content.contains("Paragraph 39"),
            "bottom of wrapped content must be reachable"
        );
        assert!(app.overlay.as_ref().unwrap().scroll() > 0);
    }

    #[test]
    fn test_solved_is_monotonic_across_polls() {
        // A later poll without the AC must NOT un-mark a solved problem.
        let mut app = sample_app();
        for p in &mut app.problems {
            p.solved = false;
        }
        let ac = submissions(&[(1, 1, "A Plus B", Verdict::Accepted)]);
        update_submissions(&mut app, &ac);
        assert!(
            app.problems
                .iter()
                .find(|p| p.problem_id == "1")
                .unwrap()
                .solved
        );
        // Next poll: no AC for problem 1 in the returned set.
        let no_ac = submissions(&[(2, 2, "Subarrays", Verdict::WrongAnswer)]);
        update_submissions(&mut app, &no_ac);
        assert!(
            app.problems
                .iter()
                .find(|p| p.problem_id == "1")
                .unwrap()
                .solved,
            "problem 1 stays solved even after its AC scrolls out of the window"
        );
    }

    #[test]
    fn test_render_narrow_no_panic() {
        let app = sample_app();
        let content = render_to(&app, 40, 20);
        assert!(!content.is_empty());
    }

    #[test]
    fn a_rejected_session_is_reported_instead_of_reconnecting() {
        let mut app = sample_app();
        app.link = app.link.after(PollOutcome::Failed);
        assert_eq!(app.link.warning(), None, "one failed cycle stays quiet");
        app.link = app.link.after(PollOutcome::Failed);
        assert!(render_to(&app, 120, 30).contains("reconnecting"));

        app.link = app.link.after(PollOutcome::SessionRejected);
        let screen = render_to(&app, 120, 30);
        assert!(screen.contains("session expired"));
        assert!(!screen.contains("reconnecting"));

        app.link = app.link.after(PollOutcome::Ok);
        assert_eq!(app.link, Link::Connected);
    }

    #[test]
    fn a_cycle_reports_its_worst_request() {
        use PollOutcome::*;
        assert_eq!(Ok.worst(Failed), Failed);
        assert_eq!(Failed.worst(SessionRejected), SessionRejected);
        assert_eq!(Ok.worst(Ok), Ok);
    }

    #[test]
    fn a_different_user_logging_in_clears_the_previous_users_state() {
        let mut app = sample_app();
        update_submissions(
            &mut app,
            &submissions(&[(1, 1, "A Plus B", Verdict::Accepted)]),
        );
        app.subs_inited = true;
        assert!(app.problems.iter().any(|p| p.solved));

        app.forget_user();
        assert!(app.my_submissions.is_empty());
        assert!(!app.subs_inited);
        assert!(app.problems.iter().all(|p| !p.solved));
    }
}
