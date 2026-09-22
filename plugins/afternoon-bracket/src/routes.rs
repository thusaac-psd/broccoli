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
use crate::judge;
use crate::model::{MatchPhase, MatchState, RoundDef};
use crate::storage;
use crate::visibility::{self, VisibilityCtx};

/// Reject a `/start` unless both players have submitted their ranking; the
/// match BLOCKS until both rankings are in (spec: "Match started with
/// orders missing -- `/start` rejects; blocking until both rankings are in
/// is the decided behaviour"). Delegates the phase + intermission-timing
/// check to `bracket::start_match_at`, which deliberately leaves this
/// precondition to its caller -- see that function's doc comment.
fn start_match(m: &mut MatchState, now: i64, opens: i64) -> Result<(), &'static str> {
    if m.order_a.is_none() || m.order_b.is_none() {
        return Err("both players must submit their ranking before the match can start");
    }
    bracket::start_match_at(m, now, opens)
}

/// Handle `POST /matches/{match_id}/start`. Staff only (`contest:manage`).
pub fn handle_start(host: &Host, req: &PluginHttpRequest) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let match_id: u8 = req.param("match_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("afternoon-bracket")?;
    if !req.has_permission(perm::CONTEST_MANAGE) {
        return Err(
            PluginHttpResponse::error(403, "Starting a match requires contest:manage").into(),
        );
    }

    let setup = storage::load_setup(host, contest_id)?
        .ok_or_else(|| PluginHttpResponse::error(400, "The bracket has not been set up yet"))?;
    let current = storage::load_match(host, contest_id, match_id)?
        .ok_or_else(|| PluginHttpResponse::error(404, "Match not found"))?;

    // Cheap, DB-free precondition, checked first: a request rejected for
    // missing orders should not also pay for a round-gate lookup (below) or
    // a `now()` query it will never use. Mirrors `start_match`'s own check
    // order (see that function) -- this can never disagree with what the
    // CAS closure ultimately enforces, since `start_match` re-checks the
    // same thing against the live document.
    if current.order_a.is_none() || current.order_b.is_none() {
        return Err(PluginHttpResponse::error(
            400,
            "both players must submit their ranking before the match can start",
        )
        .into());
    }

    // The round-intermission boundary is computed once, outside the CAS
    // retry loop below, from a snapshot of the whole bracket: a match's own
    // round-opening time does not depend on anything the retry loop itself
    // writes, so recomputing it on every retry (as `step`'s `now_ms` call
    // deliberately IS recomputed) would only add redundant reads, not
    // correctness.
    let opens = if current.round <= 1 {
        0
    } else {
        let matches = storage::load_all_matches(host, contest_id)?;
        let Some(previous_round_ended) = bracket::round_ended_at_ms(&matches, current.round - 1)
        else {
            return Err(PluginHttpResponse::error(
                409,
                "The previous round has not finished for every match yet",
            )
            .into());
        };
        bracket::round_opens_at_ms(&setup, current.round, previous_round_ended)
    };

    // Validate against a snapshot first, mirroring `ordering::handle_order`,
    // so a rejection is reported as 400 rather than masked as 500 by
    // `SdkError`'s blanket `ApiError` conversion.
    let now_probe = judge::now_ms(host)?;
    let mut probe = current.clone();
    start_match(&mut probe, now_probe, opens).map_err(|msg| PluginHttpResponse::error(400, msg))?;

    let updated = storage::update_match(host, contest_id, match_id, |m| {
        let now = judge::now_ms(host)?;
        start_match(m, now, opens).map_err(|e| SdkError::Other(e.to_string()))?;
        // The moment a match leaves `Ordering`, its first regular 小局 must
        // open -- nothing else in this plugin opens 小局 index 0 (`step`
        // only ever opens the NEXT one once the current one decides).
        judge::open_xiaoju(host, contest_id, match_id, &setup, now, m)
    })?;

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
    info.require_type("afternoon-bracket")?;
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
            judge::force_decide(host, contest_id, match_id, winner)?;
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
    /// Present (and matches `MatchState::awaiting_submission_id`) only while
    /// `state == MatchPhase::AwaitingJudge`. Never masked -- like `state`
    /// itself, this is structural, not a problem id. Lets staff reading
    /// `GET /matches/{id}` distinguish "still solving" (`InProgress`) from
    /// "blocked on a platform-side judge" (`AwaitingJudge`) without having
    /// to diff two responses over time.
    awaiting_submission_id: Option<i32>,
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
    info.require_type("afternoon-bracket")?;

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
    let views: Vec<MatchView> = matches
        .iter()
        .filter_map(|(id, m)| {
            let round_def = setup.rounds.get(m.round.saturating_sub(1) as usize)?;
            Some(match_view(*id, m, round_def, viewer, can_view_all))
        })
        .collect();

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
    info.require_type("afternoon-bracket")?;

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
    let view = match_view(match_id, &m, round_def, viewer, can_view_all);

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
            "contest_type": "afternoon-bracket",
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

    // -- start_match (pure) --

    #[test]
    fn start_is_rejected_while_an_order_is_missing() {
        // Decided behaviour: the match BLOCKS until both rankings are in.
        let mut m = match_in_ordering();
        m.order_a = Some([103, 101, 102]);
        // order_b is still None -- only one side has ranked.
        let err = start_match(&mut m, 0, 0).unwrap_err();
        assert!(err.contains("ranking"), "got: {err}");
        assert_eq!(
            m.state,
            MatchPhase::Ordering,
            "a rejected start must not mutate state"
        );
    }

    #[test]
    fn start_succeeds_once_both_orders_are_in_and_the_round_has_opened() {
        let mut m = match_in_ordering();
        m.order_a = Some([103, 101, 102]);
        m.order_b = Some([203, 201, 202]);
        start_match(&mut m, 100, 0).unwrap();
        assert_eq!(m.state, MatchPhase::InProgress);
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
    fn handle_start_rejects_when_an_order_is_missing() {
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let setup = setup_two_rounds();
        let mut m = match_in_ordering();
        m.order_a = Some([103, 101, 102]);
        // order_b missing.
        seed(&host, 7, &setup, storage::match_id_for(1, 0), &m);

        let err = handle_start(
            &host,
            &request(7, storage::match_id_for(1, 0), Some(staff_auth())),
        )
        .unwrap_err();
        assert_eq!(err.into_response().status, 400);

        let reloaded = storage::load_match(&host, 7, storage::match_id_for(1, 0))
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.state, MatchPhase::Ordering);
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

        // `handle_start` probe-validates with one `now_ms` query, then a
        // second inside the CAS closure -- both need a queued row.
        queue_now(&host, 1_000);
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
}
