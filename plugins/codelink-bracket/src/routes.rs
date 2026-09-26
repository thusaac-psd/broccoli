//! The four routes not already covered by `setup.rs` (`/setup`) and
//! `ordering.rs` (`/matches/{id}/order`):
//!
//! - `POST /matches/{id}/start` -- staff only, begins a match once both
//!   players have ranked each other's problems and the round has opened.
//! - `POST /matches/{id}/force-decide` -- staff only; see `judge.rs`'s
//!   module doc comment for the two distinct things this does.
//! - `GET /bracket` -- the whole bracket, visibility-filtered per viewer.
//! - `GET /matches/{id}` -- one match, visibility-filtered per viewer.
//!
//! Kept off the WASM ABI, like `setup::handle_setup` and
//! `ordering::handle_order` -- the `api_*` wrappers in `lib.rs` adapt these.

use broccoli_server_sdk::permissions as perm;
use broccoli_server_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bracket;
use crate::decide;
use crate::judge;
use crate::model::{MatchPhase, MatchState, RoundDef, Setup};
use crate::storage;
use crate::visibility::{self, VisibilityCtx};

/// Staff override: start `m` now. Matches normally start by themselves
/// (see `bracket` and `judge::try_autostart`); this is for a match stuck on a
/// player who never ranks. A missing ranking becomes the listed order, and
/// the players' breaks are not waited for - staff asked for now.
fn force_start(m: &mut MatchState) -> Result<(), &'static str> {
    if m.state != MatchPhase::Ordering {
        return Err("match is not awaiting start");
    }
    bracket::fill_missing_orders(m);
    m.state = MatchPhase::InProgress;
    Ok(())
}

/// Handle `POST /matches/{match_id}/start`. Staff only (`contest:manage`).
pub fn handle_start(host: &Host, req: &PluginHttpRequest) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let match_id: u8 = req.param("match_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink-bracket")?;
    if !req.has_permission(perm::CONTEST_MANAGE) {
        return Err(
            PluginHttpResponse::error(403, "Starting a match requires contest:manage").into(),
        );
    }

    let setup = storage::load_setup(host, contest_id)?
        .ok_or_else(|| PluginHttpResponse::error(400, "The bracket has not been set up yet"))?;
    let current = storage::load_match(host, contest_id, match_id)?
        .ok_or_else(|| PluginHttpResponse::error(404, "Match not found"))?;
    if info.phase == "before" {
        return Err(PluginHttpResponse::error(400, "The contest has not started yet").into());
    }

    // Validate against a snapshot first so a rejection is a 400, not the
    // 500 `SdkError`'s blanket `ApiError` conversion would give it.
    let mut probe = current.clone();
    force_start(&mut probe).map_err(|msg| PluginHttpResponse::error(400, msg))?;

    let updated = storage::update_match(host, contest_id, match_id, |m| {
        let now = judge::now_ms(host)?;
        force_start(m).map_err(|e| SdkError::Other(e.to_string()))?;
        judge::open_xiaoju(host, contest_id, match_id, &setup, now, m)
    })?;
    // A start timer may still be pending; it will find the match started
    // and do nothing, but cancel it rather than leave it to fire.
    let _ = host
        .timer
        .cancel(&judge::start_timer_key(contest_id, match_id));

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({ "state": updated.state })),
    })
}

/// Wire body for `POST /matches/{match_id}/force-decide`. `winner` absent
/// or `null` means "force expiry" (just re-run `advance` at the current
/// real time); present means "resolve adjudication" / "award the match" --
/// see `judge.rs`'s module doc comment for why these are different paths.
#[derive(Debug, Default, Deserialize)]
struct ForceDecideRequest {
    #[serde(default)]
    winner: Option<i32>,
}

/// Handle `POST /matches/{match_id}/force-decide`. Staff only
/// (`contest:manage`).
pub fn handle_force_decide(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let match_id: u8 = req.param("match_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink-bracket")?;
    if !req.has_permission(perm::CONTEST_MANAGE) {
        return Err(PluginHttpResponse::error(
            403,
            "Force-deciding a match requires contest:manage",
        )
        .into());
    }

    let current = storage::load_match(host, contest_id, match_id)?
        .ok_or_else(|| PluginHttpResponse::error(404, "Match not found"))?;

    let body: ForceDecideRequest = match req.body.clone() {
        Some(v) => serde_json::from_value(v)
            .map_err(|e| PluginHttpResponse::error(400, format!("Invalid request body: {e}")))?,
        None => ForceDecideRequest::default(),
    };

    match body.winner {
        None => {
            judge::advance(host, contest_id, match_id)?;
        }
        Some(winner) => {
            // Validate against a snapshot first, same rationale as
            // `handle_start` above: a rejection here must be a 400, not the
            // 500 `force_decide`'s `SdkError` return type would otherwise
            // produce via `ApiError`'s blanket conversion.
            let mut probe = current.clone();
            judge::apply_force_decide(&mut probe, winner, 0)
                .map_err(|msg| PluginHttpResponse::error(400, msg))?;

            // The probe above passed (the match was not yet `Decided` at
            // that snapshot), but a concurrent force-decide can still win
            // the race before `force_decide`'s CAS retry closure re-runs
            // `apply_force_decide` against the freshly-reloaded state -- see
            // `map_force_decide_error`'s doc comment.
            judge::force_decide(host, contest_id, match_id, winner)
                .map_err(map_force_decide_error)?;
        }
    }

    let updated = storage::load_match(host, contest_id, match_id)?
        .ok_or_else(|| PluginHttpResponse::error(404, "Match not found"))?;
    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({
            "state": updated.state,
            "winner": updated.winner,
        })),
    })
}

/// Map a [`judge::force_decide`] failure to the right HTTP response. That
/// function's CAS retry closure can lose a genuine race against another
/// concurrent force-decide (two staff clicking the same button, or a
/// retried request): it re-validates `apply_force_decide` against the
/// freshly-reloaded state on every retry, and returns exactly
/// `SdkError::Other(judge::ALREADY_DECIDED_MSG)` when it discovers the match
/// was decided out from under it. That is a benign lost race, not an
/// internal error, so it becomes a 409 Conflict here instead of falling
/// through to `ApiError`'s generic `SdkError` -> 500 conversion. Any OTHER
/// `SdkError` (the bracket was never set up, a genuine storage/host
/// failure, ...) still falls through to that generic conversion unchanged
/// -- see this function's negative-control test below.
///
/// Pulled into its own function, rather than inlined as a `map_err`
/// closure, so it can be unit tested directly against a synthetic error:
/// the real race it exists for cannot be reproduced deterministically
/// against `Host::mock()`'s single-threaded (`RefCell`-backed) storage, so
/// `judge::force_decide_surfaces_the_already_decided_message_when_it_loses_a_race`
/// (`judge.rs`) tests that `force_decide` produces this exact error, and
/// the two tests below test that THIS function maps it correctly.
fn map_force_decide_error(e: SdkError) -> ApiError {
    match e {
        SdkError::Other(ref msg) if msg == judge::ALREADY_DECIDED_MSG => {
            ApiError::from(PluginHttpResponse::error(409, judge::ALREADY_DECIDED_MSG))
        }
        other => ApiError::from(other),
    }
}

/// One match, as returned by `GET /bracket` and `GET /matches/{id}` --
/// every problem-id-bearing field has already been masked for the
/// requesting viewer (see [`mask_problem`]). Structural fields (who is
/// playing whom, the round, scores, phase, winner) are never masked: the
/// spec's visibility rules are about PROBLEMS, not about bracket shape.
#[derive(Debug, Serialize)]
struct MatchView {
    id: u8,
    round: u8,
    pos: u8,
    player_a: i32,
    player_b: i32,
    /// Display names for `player_a` / `player_b`, resolved in one batched
    /// lookup per request (see [`attach_player_names`]). Who plays whom is
    /// structural and public, like the ids themselves, so never masked.
    /// `None` only if the lookup failed; the frontend falls back to the id.
    player_a_name: Option<String>,
    player_b_name: Option<String>,
    group_a: [Option<i32>; 3],
    group_b: [Option<i32>; 3],
    order_a: Option<[Option<i32>; 3]>,
    order_b: Option<[Option<i32>; 3]>,
    tiebreak_problem: Option<i32>,
    score_a: u8,
    score_b: u8,
    state: MatchPhase,
    winner: Option<i32>,
    decided_at_ms: i64,
    /// Mirrors `MatchState::awaiting_submission_id`: set while
    /// `state == MatchPhase::AwaitingJudge`, and KEPT when that block times out
    /// into `NeedsAdjudication` (so staff know which submission to rejudge;
    /// its absence there means the tiebreak list ran out instead). Never
    /// masked -- like `state` itself, this is structural, not a problem id. Lets staff reading
    /// `GET /matches/{id}` distinguish "still solving" (`InProgress`) from
    /// "blocked on a platform-side judge" (`AwaitingJudge`) without having
    /// to diff two responses over time.
    awaiting_submission_id: Option<i32>,
    /// The last 小局's index, deadline and open time, taken from
    /// `MatchState::xiaoju.last()`.
    ///
    /// All three are `None` only BEFORE the first 小局 opens, while
    /// `xiaoju` is still empty. They are NOT cleared when the match is
    /// decided: nothing in production ever removes an entry from `xiaoju`
    /// (every `xiaoju.clear()` in this crate is `#[cfg(test)]`-only), so a
    /// `Decided` or `NeedsAdjudication` match keeps reporting its final
    /// 小局's now-past deadline.
    ///
    /// **A consumer must therefore derive "is this match still running"
    /// from `state`, never from the presence of these fields.** An earlier
    /// version of this comment claimed they go `None` once decided; that
    /// was simply false, and a client that trusted it would show a live
    /// countdown ticking against a stale deadline on a finished match. The
    /// frontend derives from `state` for exactly this reason.
    ///
    /// Structural, like `state` -- not a problem id, so not masked. Both
    /// players already know when their own 小局 ends, and a spectator is
    /// entitled to the same clock, so there is nothing here to withhold.
    ///
    /// Present because a countdown MUST be driven by the server's deadline.
    /// Without these the frontend can only guess, and a guessed clock that
    /// reaches zero before the server has decided would announce a result
    /// the server has not made -- in front of an audience. The frontend
    /// deliberately renders "live timing unavailable" rather than
    /// fabricating one, so these fields are what turn the countdown on.
    current_xiaoju_index: Option<u8>,
    current_xiaoju_deadline_ms: Option<i64>,
    current_xiaoju_opened_at_ms: Option<i64>,
    /// Every 小局 opened so far, in order - the per-game history staff and
    /// spectators need (who won which game, on which problems, and when). A
    /// game that has not opened is simply absent, and each problem id is
    /// masked by the same [`mask_problem`] rule as the rest of this view, so
    /// this adds no visibility that `order_a`/`order_b`/`tiebreak_problem`
    /// do not already grant.
    games: Vec<GameView>,
    /// While `Ordering`: when the match will start by itself (contest start
    /// or the end of a player's break), or `None` while a ranking is still
    /// missing. Always `None` once started. Structural, never masked.
    starts_at_ms: Option<i64>,
}

/// One opened 小局 as seen by the requesting viewer. Regular games (index
/// 0-2) give each player their own problem from the opponent-chosen order;
/// a tiebreak (index 3+) gives both players the same problem.
#[derive(Debug, Clone, Serialize, PartialEq)]
struct GameView {
    index: u8,
    tiebreak: bool,
    problem_a: Option<i32>,
    problem_b: Option<i32>,
    opened_at_ms: i64,
    deadline_ms: i64,
    winner: Option<i32>,
    decided: bool,
}

/// Whether `viewer` may see `problem_id` in `ctx`, per
/// `visibility::decide_problem` -- but adapted to `viewer: Option<i32>`
/// (an unauthenticated GET has no viewer at all) rather than that
/// function's `i32`. Mirrors `visibility::decide_visibility_decisions`'s
/// own `let Some(v) = viewer else { return Deny }` handling of that case,
/// generalised so `can_view_all` is still checked first (an admin needs no
/// viewer identity to see everything).
fn mask_problem(
    ctx: &VisibilityCtx,
    problem_id: i32,
    viewer: Option<i32>,
    can_view_all: bool,
) -> Option<i32> {
    if !can_view_all && viewer.is_none() {
        return None;
    }
    match visibility::decide_problem(ctx, problem_id, viewer.unwrap_or_default(), can_view_all) {
        WireDecision::Allow {} => Some(problem_id),
        // `decide_problem` (this plugin's own function, not the wire
        // protocol in general) only ever returns `Allow`/`Deny` -- it has
        // no partial-redaction concept for a single problem id, which is an
        // atomic "may see" / "may not see" question. `Redact` is matched
        // here only because `WireDecision` is a shared wire type with a
        // third variant used by OTHER plugins; treating it the same as
        // `Deny` fails closed rather than assuming a shape this function
        // never produces.
        WireDecision::Deny {} | WireDecision::Redact { .. } => None,
    }
}

/// Apply [`mask_problem`] to each entry of a group/order triple.
fn mask_group(
    ctx: &VisibilityCtx,
    group: [i32; 3],
    viewer: Option<i32>,
    can_view_all: bool,
) -> [Option<i32>; 3] {
    group.map(|pid| mask_problem(ctx, pid, viewer, can_view_all))
}

/// Build one match's visibility-filtered view. `round_def` must be the
/// `RoundDef` that owns `m.round` -- callers resolve it from `Setup.rounds`
/// the same way `visibility.rs`/`gate.rs` do.
fn match_view(
    id: u8,
    m: &MatchState,
    round_def: &RoundDef,
    viewer: Option<i32>,
    can_view_all: bool,
) -> MatchView {
    // The open 小局, if any: `None` before the first opens and after the
    // match is decided. Read once here so the three timing fields below
    // cannot disagree with each other.
    let current = m.xiaoju.last();
    let ctx = VisibilityCtx {
        player_a: m.player_a,
        player_b: m.player_b,
        group_a: m.group_a,
        group_b: m.group_b,
        order_a: m.order_a,
        order_b: m.order_b,
        state: m.state,
        current_xiaoju_index: m.xiaoju.last().map(|x| x.index),
        tiebreak_problem: round_def.tiebreak.get(m.tiebreak_index).copied(),
    };

    MatchView {
        id,
        round: m.round,
        pos: m.pos,
        player_a: m.player_a,
        player_b: m.player_b,
        player_a_name: None,
        player_b_name: None,
        group_a: mask_group(&ctx, m.group_a, viewer, can_view_all),
        group_b: mask_group(&ctx, m.group_b, viewer, can_view_all),
        order_a: m.order_a.map(|o| mask_group(&ctx, o, viewer, can_view_all)),
        order_b: m.order_b.map(|o| mask_group(&ctx, o, viewer, can_view_all)),
        tiebreak_problem: ctx
            .tiebreak_problem
            .and_then(|pid| mask_problem(&ctx, pid, viewer, can_view_all)),
        score_a: m.score_a,
        score_b: m.score_b,
        state: m.state,
        winner: m.winner,
        decided_at_ms: m.decided_at_ms,
        awaiting_submission_id: m.awaiting_submission_id,
        current_xiaoju_index: current.map(|x| x.index),
        current_xiaoju_deadline_ms: current.map(|x| x.deadline_ms),
        current_xiaoju_opened_at_ms: current.map(|x| x.opened_at_ms),
        games: m
            .xiaoju
            .iter()
            .map(|x| {
                let (a, b) = game_problems(m, round_def, x.index);
                let mask = |pid: Option<i32>| {
                    pid.and_then(|pid| mask_problem(&ctx, pid, viewer, can_view_all))
                };
                GameView {
                    index: x.index,
                    tiebreak: !decide::is_regular_xiaoju_index(x.index),
                    problem_a: mask(a),
                    problem_b: mask(b),
                    opened_at_ms: x.opened_at_ms,
                    deadline_ms: x.deadline_ms,
                    winner: x.winner,
                    decided: x.decided,
                }
            })
            .collect(),
        starts_at_ms: None,
    }
}

/// Fill in `starts_at_ms` for every view (see [`bracket::start_at_ms`]).
/// Best-effort like the names: a failed contest lookup leaves it `None`.
fn attach_start_times(
    host: &Host,
    contest: i32,
    setup: &Setup,
    matches: &[(u8, MatchState)],
    views: &mut [MatchView],
) {
    let Ok(contest_start) = judge::contest_start_ms(host, contest) else {
        return;
    };
    for v in views.iter_mut() {
        if let Some((_, m)) = matches.iter().find(|(id, _)| *id == v.id) {
            v.starts_at_ms = bracket::start_at_ms(setup, matches, m, contest_start);
        }
    }
}

/// The problems each player faces in 小局 `index`: position `index` of the
/// order imposed on them for a regular game, the round's `index - 3`th
/// tiebreak problem (the same for both) for a tiebreak.
fn game_problems(m: &MatchState, round_def: &RoundDef, index: u8) -> (Option<i32>, Option<i32>) {
    let i = index as usize;
    if decide::is_regular_xiaoju_index(index) {
        (m.order_a.map(|o| o[i]), m.order_b.map(|o| o[i]))
    } else {
        let t = round_def
            .tiebreak
            .get(i - decide::REGULAR_XIAOJU_COUNT)
            .copied();
        (t, t)
    }
}

#[derive(Deserialize)]
struct PlayerNameRow {
    id: i32,
    username: String,
}

/// Fill in `player_{a,b}_name` for every view with ONE query - the same
/// `"user"` lookup the morning round (codelink-qualifier) uses for its standings, so
/// both halves of the event show contestants the same way. Best-effort: names
/// are cosmetic, so a failed lookup logs and leaves them `None` rather than
/// failing the whole bracket view mid-contest.
fn attach_player_names(host: &Host, views: &mut [MatchView]) {
    let mut ids: Vec<i32> = views
        .iter()
        .flat_map(|v| [v.player_a, v.player_b])
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return;
    }
    let mut p = Params::new();
    let placeholders: Vec<String> = ids.iter().map(|id| p.bind(*id)).collect();
    let sql = format!(
        "SELECT id, username FROM \"user\" WHERE id IN ({})",
        placeholders.join(",")
    );
    let rows: Vec<PlayerNameRow> = match host.db.query_with_args(&sql, &p.into_args()) {
        Ok(rows) => rows,
        Err(e) => {
            let _ = host.log.info(&format!(
                "codelink-bracket: player name lookup failed: {e:?}"
            ));
            return;
        }
    };
    let names: std::collections::HashMap<i32, String> =
        rows.into_iter().map(|r| (r.id, r.username)).collect();
    for v in views.iter_mut() {
        v.player_a_name = names.get(&v.player_a).cloned();
        v.player_b_name = names.get(&v.player_b).cloned();
    }
}

/// Handle `GET /bracket`: every match created so far, visibility-filtered
/// for the requesting viewer. Any contest participant (or, for a public
/// contest, anyone) may call this -- `contest::check_access` is the only
/// gate; per-problem visibility narrows what each viewer actually sees
/// within the response, same division of labour as the `visibility` query
/// topic itself.
pub fn handle_get_bracket(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink-bracket")?;

    let Some(setup) = storage::load_setup(host, contest_id)? else {
        // Before /setup has run there is nothing to show yet -- an empty
        // bracket, not an error: a spectator polling before staff has
        // configured the contest should see "nothing yet", not a 404.
        return Ok(PluginHttpResponse {
            status: 200,
            headers: None,
            body: Some(serde_json::json!({ "matches": [] })),
        });
    };
    let matches = storage::load_all_matches(host, contest_id)?;

    let viewer = req.user_id();
    let can_view_all = req.has_permission(perm::SUBMISSION_VIEW_ALL);
    let mut views: Vec<MatchView> = matches
        .iter()
        .filter_map(|(id, m)| {
            let round_def = setup.rounds.get(m.round.saturating_sub(1) as usize)?;
            Some(match_view(*id, m, round_def, viewer, can_view_all))
        })
        .collect();
    attach_player_names(host, &mut views);
    attach_start_times(host, contest_id, &setup, &matches, &mut views);

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({ "matches": views })),
    })
}

/// Handle `GET /matches/{match_id}`: one match, visibility-filtered for the
/// requesting viewer. See [`handle_get_bracket`] for the access-gate
/// rationale, identical here.
pub fn handle_get_match(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let match_id: u8 = req.param("match_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink-bracket")?;

    let setup = storage::load_setup(host, contest_id)?
        .ok_or_else(|| PluginHttpResponse::error(404, "The bracket has not been set up yet"))?;
    let m = storage::load_match(host, contest_id, match_id)?
        .ok_or_else(|| PluginHttpResponse::error(404, "Match not found"))?;
    let round_def = setup
        .rounds
        .get(m.round.saturating_sub(1) as usize)
        .ok_or_else(|| PluginHttpResponse::error(500, "Match references an undefined round"))?;

    let viewer = req.user_id();
    let can_view_all = req.has_permission(perm::SUBMISSION_VIEW_ALL);
    let mut view = [match_view(match_id, &m, round_def, viewer, can_view_all)];
    attach_player_names(host, &mut view);
    if m.state == MatchPhase::Ordering {
        let matches = storage::load_all_matches(host, contest_id)?;
        attach_start_times(host, contest_id, &setup, &matches, &mut view);
    }
    let [view] = view;

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::to_value(view)?),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::model::{RoundDef, Setup, XiaojuState};

    fn setup_two_rounds() -> Setup {
        Setup {
            rounds: vec![
                RoundDef {
                    group_a: [101, 102, 103],
                    group_b: [201, 202, 203],
                    tiebreak: vec![301, 302],
                },
                RoundDef {
                    group_a: [401, 402, 403],
                    group_b: [501, 502, 503],
                    tiebreak: vec![601],
                },
            ],
            xiaoju_seconds: 1_800,
            round_intermission_seconds: 600,
            escalation_grace_seconds: 120,
        }
    }

    fn match_in_ordering() -> MatchState {
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            state: MatchPhase::Ordering,
            ..Default::default()
        }
    }

    fn queue_now(host: &Host, now: i64) {
        host.db
            .queue_query_result(serde_json::json!([{ "now_ms": now as f64 }]));
    }

    fn queue_bracket_contest_info(host: &Host) {
        host.db.queue_query_result(serde_json::json!([{
            "contest_type": "codelink-bracket",
            "is_public": true,
            "is_active": true,
            "phase": "during",
        }]));
    }

    fn seed(host: &Host, contest: i32, setup: &Setup, match_id: u8, m: &MatchState) {
        host.storage
            .set(&[(
                storage::setup_key(contest).as_str(),
                serde_json::to_string(setup).unwrap().as_str(),
            )])
            .unwrap();
        host.storage
            .set(&[(
                storage::match_key(contest, match_id).as_str(),
                serde_json::to_string(m).unwrap().as_str(),
            )])
            .unwrap();
    }

    // -- force_start (pure) --

    #[test]
    fn the_staff_override_starts_a_match_whose_ranking_never_came() {
        // A player who never ranks must not stall the bracket forever: the
        // override fills the missing order with the listed group.
        let mut m = match_in_ordering();
        m.order_a = Some([103, 101, 102]);
        force_start(&mut m).unwrap();
        assert_eq!(m.state, MatchPhase::InProgress);
        assert_eq!(m.order_b, Some(m.group_b));
        assert_eq!(m.order_a, Some([103, 101, 102]), "a real ranking is kept");
    }

    #[test]
    fn the_staff_override_refuses_a_match_that_is_not_waiting_to_start() {
        let mut m = match_in_ordering();
        m.state = MatchPhase::InProgress;
        assert!(force_start(&mut m).is_err());
    }

    // -- handle_start (host wiring) --

    fn request(contest_id: i32, match_id: u8, auth: Option<PluginHttpAuth>) -> PluginHttpRequest {
        let mut params = HashMap::new();
        params.insert("contest_id".to_string(), contest_id.to_string());
        params.insert("match_id".to_string(), match_id.to_string());
        PluginHttpRequest {
            method: "POST".into(),
            path: String::new(),
            params,
            query: HashMap::new(),
            headers: HashMap::new(),
            body: None,
            auth,
        }
    }

    fn staff_auth() -> PluginHttpAuth {
        PluginHttpAuth {
            user_id: 1,
            username: "staff".into(),
            roles: vec![],
            permissions: vec![perm::CONTEST_MANAGE.to_string()],
        }
    }

    fn player_auth(user_id: i32) -> PluginHttpAuth {
        PluginHttpAuth {
            user_id,
            username: format!("player{user_id}"),
            roles: vec![],
            permissions: vec![],
        }
    }

    #[test]
    fn handle_start_rejects_a_non_staff_caller() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.order_a = Some([103, 101, 102]);
        m.order_b = Some([203, 201, 202]);
        seed(&host, 7, &setup, storage::match_id_for(1, 0), &m);

        // Non-staff rejections come back as `Err(ApiError::Response(..))`, not
        // `Ok(..)` -- see `ordering::handle_order`'s staff-check convention.
        let err = handle_start(
            &host,
            &request(7, storage::match_id_for(1, 0), Some(player_auth(10))),
        )
        .unwrap_err();
        assert_eq!(err.into_response().status, 403);
    }

    #[test]
    fn handle_start_overrides_a_missing_ranking_with_the_listed_order() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.order_a = Some([103, 101, 102]);
        // order_b missing: player A never ranked B's problems.
        let match_id = storage::match_id_for(1, 0);
        seed(&host, 7, &setup, match_id, &m);
        queue_now(&host, 1_000);

        let resp = handle_start(&host, &request(7, match_id, Some(staff_auth()))).unwrap();
        assert_eq!(resp.status, 200);

        let reloaded = storage::load_match(&host, 7, match_id).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::InProgress);
        assert_eq!(reloaded.order_b, Some(reloaded.group_b));
        assert_eq!(reloaded.order_a, Some([103, 101, 102]));
    }

    #[test]
    fn handle_start_opens_the_first_xiaoju_and_schedules_its_deadline() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.order_a = Some([103, 101, 102]);
        m.order_b = Some([203, 201, 202]);
        let match_id = storage::match_id_for(1, 0);
        seed(&host, 7, &setup, match_id, &m);

        // `handle_start` reads the clock once, inside the CAS closure.
        queue_now(&host, 1_000);

        let resp = handle_start(&host, &request(7, match_id, Some(staff_auth()))).unwrap();
        assert_eq!(resp.status, 200);

        let reloaded = storage::load_match(&host, 7, match_id).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::InProgress);
        assert_eq!(reloaded.xiaoju.len(), 1);
        assert_eq!(reloaded.xiaoju[0].index, 0);
        let key = judge::xiaoju_timer_key(7, match_id, 0);
        assert!(host.timer.is_scheduled(&key));
    }

    // -- handle_force_decide (host wiring) --

    #[test]
    fn handle_force_decide_rejects_a_non_staff_caller() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.state = MatchPhase::NeedsAdjudication;
        seed(&host, 7, &setup, 0, &m);

        let err = handle_force_decide(&host, &request(7, 0, Some(player_auth(10)))).unwrap_err();
        assert_eq!(err.into_response().status, 403);
    }

    #[test]
    fn handle_force_decide_rejects_a_winner_who_is_not_a_participant() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.state = MatchPhase::NeedsAdjudication;
        seed(&host, 7, &setup, 0, &m);

        let mut req = request(7, 0, Some(staff_auth()));
        req.body = Some(serde_json::json!({ "winner": 999 }));
        let err = handle_force_decide(&host, &req).unwrap_err();
        assert_eq!(err.into_response().status, 400);
    }

    #[test]
    fn handle_force_decide_with_an_explicit_winner_resolves_adjudication() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.state = MatchPhase::NeedsAdjudication;
        seed(&host, 7, &setup, 0, &m);

        let mut req = request(7, 0, Some(staff_auth()));
        req.body = Some(serde_json::json!({ "winner": 10 }));
        queue_now(&host, 9_000_000);
        let resp = handle_force_decide(&host, &req).unwrap();
        assert_eq!(resp.status, 200);

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Decided);
        assert_eq!(reloaded.winner, Some(10));
    }

    // -- map_force_decide_error (DEFECT 4 fix) --

    #[test]
    fn map_force_decide_error_turns_a_lost_race_into_409() {
        let err = map_force_decide_error(SdkError::Other(judge::ALREADY_DECIDED_MSG.to_string()));
        let resp = err.into_response();
        assert_eq!(
            resp.status, 409,
            "the losing side of a concurrent force-decide must be a 409, not a bare 500"
        );
    }

    #[test]
    fn map_force_decide_error_negative_control_other_sdk_errors_still_surface_as_500() {
        // Negative control: an SdkError with any OTHER message (a genuine
        // internal error, e.g. the bracket was never set up) must NOT be
        // caught by the 409 branch -- only the exact lost-race message is.
        let err = map_force_decide_error(SdkError::Other(
            "the bracket has not been set up yet".to_string(),
        ));
        let resp = err.into_response();
        assert_eq!(
            resp.status, 500,
            "a genuine internal error must not be masked as a 409 conflict"
        );
    }

    #[test]
    fn handle_force_decide_with_no_winner_falls_through_to_advance() {
        // "Force expiry": a 小局 whose deadline has already passed, with no
        // AC recorded, must be decided scoreless by the plain `advance`
        // path -- exactly as if the timer had just fired.
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.state = MatchPhase::InProgress;
        m.order_a = Some([103, 101, 102]);
        m.order_b = Some([203, 201, 202]);
        m.xiaoju = vec![XiaojuState {
            index: 0,
            opened_at_ms: 0,
            deadline_ms: 1_000_000,
            winner: None,
            decided: false,
        }];
        seed(&host, 7, &setup, 0, &m);

        queue_now(&host, 1_000_000);
        host.db.queue_query_result(serde_json::Value::Array(vec![])); // no subs

        let resp = handle_force_decide(&host, &request(7, 0, Some(staff_auth()))).unwrap();
        assert_eq!(resp.status, 200);

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert!(reloaded.xiaoju[0].decided);
        assert_eq!(reloaded.xiaoju[0].winner, None);
    }

    // -- MatchView 小局 timing: what drives the client countdown --

    #[test]
    fn games_list_each_opened_game_and_mask_it_like_the_rest_of_the_view() {
        let mut m = match_in_ordering();
        m.state = MatchPhase::InProgress;
        m.order_a = Some([103, 101, 102]);
        m.order_b = Some([203, 201, 202]);
        m.xiaoju = vec![
            XiaojuState {
                index: 0,
                opened_at_ms: 0,
                deadline_ms: 60_000,
                winner: Some(10),
                decided: true,
            },
            XiaojuState {
                index: 1,
                opened_at_ms: 60_000,
                deadline_ms: 120_000,
                winner: None,
                decided: false,
            },
        ];
        let setup = setup_two_rounds();
        let round_def = &setup.rounds[0];

        // Staff see both players' problems for every opened game, and no
        // entry at all for the game that has not opened.
        let staff = match_view(0, &m, round_def, None, true);
        assert_eq!(staff.games.len(), 2);
        assert_eq!(
            (staff.games[0].problem_a, staff.games[0].problem_b),
            (Some(103), Some(203))
        );
        assert_eq!(
            (staff.games[1].problem_a, staff.games[1].problem_b),
            (Some(101), Some(201))
        );
        assert_eq!(staff.games[0].winner, Some(10));
        assert!(!staff.games[1].tiebreak);

        // Each game's problems obey exactly the same masking as the orders.
        for viewer in [Some(10), Some(20), Some(99), None] {
            let v = match_view(0, &m, round_def, viewer, false);
            for g in &v.games {
                let i = g.index as usize;
                assert_eq!(
                    g.problem_a,
                    v.order_a.unwrap()[i],
                    "viewer {viewer:?} game {i} a"
                );
                assert_eq!(
                    g.problem_b,
                    v.order_b.unwrap()[i],
                    "viewer {viewer:?} game {i} b"
                );
            }
        }
    }

    #[test]
    fn match_view_exposes_the_open_xiaoju_timing_for_the_countdown() {
        // The frontend must never invent a deadline. If these are absent it
        // renders "live timing unavailable" rather than running a guessed
        // clock that could hit zero and announce a result the server has not
        // made. So a missing field here is not cosmetic - it silently
        // disables the countdown for a live, spectated match.
        let mut m = match_in_ordering();
        m.state = MatchPhase::InProgress;
        m.order_a = Some([103, 101, 102]);
        m.order_b = Some([203, 201, 202]);
        m.xiaoju = vec![XiaojuState {
            index: 1,
            opened_at_ms: 5_000,
            deadline_ms: 65_000,
            winner: None,
            decided: false,
        }];
        let setup = setup_two_rounds();

        let view = match_view(0, &m, &setup.rounds[0], Some(m.player_a), false);

        assert_eq!(view.current_xiaoju_index, Some(1));
        assert_eq!(view.current_xiaoju_opened_at_ms, Some(5_000));
        assert_eq!(view.current_xiaoju_deadline_ms, Some(65_000));
    }

    #[test]
    fn match_view_reports_no_xiaoju_timing_before_the_first_one_opens() {
        // Boundary the naive `unwrap_or(0)` gets wrong: 0 is a legitimate
        // epoch value, so "no 小局 open" must be absent, not zero. A client
        // told `deadline_ms: 0` would show a countdown that expired in 1970.
        let m = match_in_ordering();
        let setup = setup_two_rounds();

        let view = match_view(0, &m, &setup.rounds[0], Some(m.player_a), false);

        assert_eq!(view.current_xiaoju_index, None);
        assert_eq!(view.current_xiaoju_opened_at_ms, None);
        assert_eq!(view.current_xiaoju_deadline_ms, None);
    }

    #[test]
    fn match_view_still_reports_the_final_xiaoju_timing_after_the_match_is_decided() {
        // Pins the ACTUAL contract, which is not the intuitive one. Nothing
        // in production removes an entry from `xiaoju` -- every
        // `xiaoju.clear()` in this crate is `#[cfg(test)]`-only -- so a
        // decided match keeps reporting its last 小局's now-past deadline
        // rather than going `None`.
        //
        // This test exists because an earlier version of the field's doc
        // comment asserted the opposite. A client trusting that would run a
        // live countdown against a stale deadline on a finished match. The
        // rule for consumers is: derive "still running" from `state`, never
        // from the presence of these fields.
        let mut m = match_in_ordering();
        m.state = MatchPhase::Decided;
        m.winner = Some(m.player_a);
        m.order_a = Some([103, 101, 102]);
        m.order_b = Some([203, 201, 202]);
        m.xiaoju = vec![XiaojuState {
            index: 2,
            opened_at_ms: 10_000,
            deadline_ms: 70_000,
            winner: Some(10),
            decided: true,
        }];
        let setup = setup_two_rounds();

        let view = match_view(0, &m, &setup.rounds[0], Some(m.player_a), false);

        assert_eq!(view.state, MatchPhase::Decided);
        assert_eq!(
            view.current_xiaoju_index,
            Some(2),
            "a decided match still reports its final 小局 -- consumers must key off `state`"
        );
        assert_eq!(view.current_xiaoju_deadline_ms, Some(70_000));
    }

    // -- GET /bracket, GET /matches/{id}: the visibility-masking gap --

    fn seed_match_in_progress(host: &Host, contest: i32) {
        let setup = setup_two_rounds();
        let m = MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some([103, 101, 102]),
            order_b: Some([203, 201, 202]),
            state: MatchPhase::InProgress,
            xiaoju: vec![XiaojuState {
                index: 0,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: None,
                decided: false,
            }],
            ..Default::default()
        };
        seed(host, contest, &setup, 0, &m);
    }

    fn get_request(
        contest_id: i32,
        match_id: u8,
        auth: Option<PluginHttpAuth>,
    ) -> PluginHttpRequest {
        let mut params = HashMap::new();
        params.insert("contest_id".to_string(), contest_id.to_string());
        params.insert("match_id".to_string(), match_id.to_string());
        PluginHttpRequest {
            method: "GET".into(),
            path: String::new(),
            params,
            query: HashMap::new(),
            headers: HashMap::new(),
            body: None,
            auth,
        }
    }

    #[test]
    fn get_match_hides_a_players_own_current_problem_from_that_same_player() {
        // THE regression test for the gap this task closes: without
        // per-viewer masking, a GET response would leak `group_a`
        // (including a problem the owner has not reached yet) to the owner
        // themself.
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        seed_match_in_progress(&host, 7);

        let resp = handle_get_match(&host, &get_request(7, 0, Some(player_auth(10)))).unwrap();
        assert_eq!(resp.status, 200);
        let body = resp.body.unwrap();
        // Player A's own group is masked entry-by-entry against `group_a`'s
        // OWN array order (not `order_a`'s ranking order): only the problem
        // at the current ranking position (103, sitting at `group_a[2]`) is
        // visible, so it is the third element here, not the first.
        assert_eq!(body["group_a"], serde_json::json!([null, null, 103]));
    }

    #[test]
    fn get_match_shows_the_opponents_group_in_full() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        seed_match_in_progress(&host, 7);

        let resp = handle_get_match(&host, &get_request(7, 0, Some(player_auth(20)))).unwrap();
        let body = resp.body.unwrap();
        assert_eq!(body["group_a"], serde_json::json!([101, 102, 103]));
    }

    #[test]
    fn get_match_hides_everything_from_an_uninvolved_anonymous_viewer() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        seed_match_in_progress(&host, 7);

        let resp = handle_get_match(&host, &get_request(7, 0, None)).unwrap();
        let body = resp.body.unwrap();
        assert_eq!(body["group_a"], serde_json::json!([null, null, null]));
        assert_eq!(body["group_b"], serde_json::json!([null, null, null]));
        // Structural fields are never masked.
        assert_eq!(body["player_a"], 10);
        assert_eq!(body["state"], "in_progress");
    }

    #[test]
    fn get_match_view_all_sees_everything() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        seed_match_in_progress(&host, 7);

        let mut auth = player_auth(999);
        auth.permissions = vec![perm::SUBMISSION_VIEW_ALL.to_string()];
        let resp = handle_get_match(&host, &get_request(7, 0, Some(auth))).unwrap();
        let body = resp.body.unwrap();
        assert_eq!(body["group_a"], serde_json::json!([101, 102, 103]));
        assert_eq!(body["group_b"], serde_json::json!([201, 202, 203]));
    }

    #[test]
    fn get_match_returns_404_for_a_match_that_was_never_created() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        host.storage
            .set(&[(
                storage::setup_key(7).as_str(),
                serde_json::to_string(&setup_two_rounds()).unwrap().as_str(),
            )])
            .unwrap();
        let err = handle_get_match(&host, &get_request(7, 0, Some(player_auth(10)))).unwrap_err();
        assert_eq!(err.into_response().status, 404);
    }

    fn bracket_request(contest_id: i32, auth: Option<PluginHttpAuth>) -> PluginHttpRequest {
        let mut params = HashMap::new();
        params.insert("contest_id".to_string(), contest_id.to_string());
        PluginHttpRequest {
            method: "GET".into(),
            path: String::new(),
            params,
            query: HashMap::new(),
            headers: HashMap::new(),
            body: None,
            auth,
        }
    }

    #[test]
    fn get_bracket_returns_an_empty_list_before_setup_has_run() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let resp = handle_get_bracket(&host, &bracket_request(7, Some(player_auth(10)))).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body.unwrap()["matches"], serde_json::json!([]));
    }

    #[test]
    fn get_bracket_masks_each_match_for_the_requesting_viewer() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        seed_match_in_progress(&host, 7);

        let resp = handle_get_bracket(&host, &bracket_request(7, Some(player_auth(10)))).unwrap();
        let matches = resp.body.unwrap()["matches"].clone();
        assert_eq!(matches.as_array().unwrap().len(), 1);
        // See `get_match_hides_a_players_own_current_problem_from_that_same_player`
        // for why the visible entry lands at `group_a`'s own index (2), not
        // at the front.
        assert_eq!(matches[0]["group_a"], serde_json::json!([null, null, 103]));
    }
    #[test]
    fn get_bracket_shows_player_usernames() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        seed_match_in_progress(&host, 7);
        host.db.queue_query_result(serde_json::json!([
            { "id": 10, "username": "alice" },
            { "id": 20, "username": "bob" },
        ]));

        let resp = handle_get_bracket(&host, &bracket_request(7, Some(player_auth(10)))).unwrap();
        let m = resp.body.unwrap()["matches"][0].clone();
        assert_eq!(m["player_a_name"], "alice");
        assert_eq!(m["player_b_name"], "bob");
    }

    #[test]
    fn get_bracket_still_renders_when_the_name_lookup_returns_nothing() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        seed_match_in_progress(&host, 7);
        host.db.queue_query_result(serde_json::json!([]));

        let resp = handle_get_bracket(&host, &bracket_request(7, Some(player_auth(10)))).unwrap();
        assert_eq!(resp.status, 200);
        let m = resp.body.unwrap()["matches"][0].clone();
        assert_eq!(m["player_a"], 10);
        assert!(m["player_a_name"].is_null());
    }
}
