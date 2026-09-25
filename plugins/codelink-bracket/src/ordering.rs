//! `/matches/{match_id}/order` -- each player ranks the OPPONENT's 3
//! problems before a match starts.
//!
//! # The direction is the single most likely bug in this plugin
//!
//! The submitter ranks the OPPONENT's problems, not their own: `submitter`
//! validates against `match_state.group_a` when `submitter == player_b`, and
//! the result is written into `order_a` -- the order imposed ON player A.
//! See the doc comment on [`crate::model::MatchState`] for the full
//! reasoning, and this module's own
//! `player_b_submitting_sets_the_order_imposed_on_player_a` test below for
//! the test that asserts the direction explicitly (a test that only
//! exercises the symmetric case, both players ranking at once, cannot catch
//! the fields being swapped).

use broccoli_server_sdk::prelude::*;
use serde::Deserialize;

use crate::model::{MatchPhase, MatchState};
use crate::storage;

/// The rejection message [`record_order`] returns once a match has left
/// `MatchPhase::Ordering`. A shared constant (rather than an inline literal
/// at each call site) so `handle_order` can pattern-match on it below to
/// distinguish "you lost a race with `/start`" from a genuine internal
/// error, the same technique `judge::ALREADY_DECIDED_MSG` uses for
/// `routes::handle_force_decide`.
pub(crate) const LEFT_ORDERING_PHASE_MSG: &str =
    "the match has left the ordering phase; rankings can no longer be changed";

/// Whether `order` contains exactly the same 3 problem ids as `group`,
/// each exactly once (order of `group` itself is irrelevant).
fn is_permutation_of(order: &[i32; 3], group: &[i32; 3]) -> bool {
    let mut sorted_order = *order;
    let mut sorted_group = *group;
    sorted_order.sort_unstable();
    sorted_group.sort_unstable();
    sorted_order == sorted_group
}

/// Record `submitter`'s ranking of their OPPONENT's 3 problems.
///
/// `submitter` must be one of `match_state.player_a` / `match_state.player_b`
/// -- anyone else is rejected. If `submitter` is player B, `order` must be a
/// permutation of `match_state.group_a` (A's own problems -- B's opponent's
/// problems, from B's point of view), and the result is written into
/// `order_a`, the order imposed ON player A. Symmetrically for player A.
///
/// Resubmitting BEFORE the match starts (`match_state.state ==
/// MatchPhase::Ordering`) replaces the previous ranking -- there is no
/// once-only guard for that case, staff/players may fix a fat-fingered
/// ranking freely up until `/start`. Once the match has left `Ordering`
/// (`InProgress`, `Tiebreak`, `AwaitingJudge`, `Decided`,
/// `NeedsAdjudication`), a resubmission is rejected outright: `order_a`/
/// `order_b` are read LIVE (not snapshotted) by `judge.rs::current_problem`
/// and `gate.rs::check` to decide who is currently playing which problem, so
/// letting either player rewrite them mid-match would let a player reorder
/// remaining problems after seeing an earlier 小局's outcome -- see the QA
/// regression test `defect_ranking_can_be_resubmitted_after_the_match_has_started`.
pub fn record_order(
    match_state: &mut MatchState,
    submitter: i32,
    order: [i32; 3],
) -> Result<(), String> {
    if match_state.state != MatchPhase::Ordering {
        return Err(LEFT_ORDERING_PHASE_MSG.to_string());
    }
    if submitter == match_state.player_b {
        if !is_permutation_of(&order, &match_state.group_a) {
            return Err(
                "order must be a permutation of the opponent's group of 3 problems".to_string(),
            );
        }
        match_state.order_a = Some(order);
        Ok(())
    } else if submitter == match_state.player_a {
        if !is_permutation_of(&order, &match_state.group_b) {
            return Err(
                "order must be a permutation of the opponent's group of 3 problems".to_string(),
            );
        }
        match_state.order_b = Some(order);
        Ok(())
    } else {
        Err(format!(
            "player {submitter} is not a participant in this match"
        ))
    }
}

/// Wire body for `POST /contests/{contest_id}/matches/{match_id}/order`.
#[derive(Debug, Deserialize)]
struct OrderRequest {
    order: [i32; 3],
}

/// Handle `POST /contests/{contest_id}/matches/{match_id}/order`. Kept off
/// the WASM ABI (the `api_order` wrapper in `lib.rs` adapts this) so it
/// unit-tests on the host, matching `handle_setup` in `setup.rs`.
pub fn handle_order(host: &Host, req: &PluginHttpRequest) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let match_id: u8 = req.param("match_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink-bracket")?;
    let submitter = req.require_user_id()?;

    let body = req
        .body
        .clone()
        .ok_or_else(|| PluginHttpResponse::error(400, "Missing request body"))?;
    let body: OrderRequest = serde_json::from_value(body)
        .map_err(|e| PluginHttpResponse::error(400, format!("Invalid request body: {e}")))?;

    let current = storage::load_match(host, contest_id, match_id)?
        .ok_or_else(|| PluginHttpResponse::error(404, "Match not found"))?;

    // Validate against a snapshot BEFORE entering the compare-and-set retry
    // loop below, so a rejection is reported as 400, not masked as a 500 by
    // the loop's `SdkError` return type. player_a/player_b/group_a/group_b
    // are set once at match creation and never change; order_a/order_b
    // (which `record_order` does not read) can change concurrently; and
    // `state` CAN also change concurrently (staff calling `/start` in a
    // race with this request, between this probe and the CAS closure
    // below). This probe therefore does not guarantee a 4xx on its own --
    // see the `map_err` below the CAS closure for how a same-race rejection
    // discovered INSIDE the closure is still kept off the generic 500 path.
    let mut probe = current.clone();
    record_order(&mut probe, submitter, body.order)
        .map_err(|msg| PluginHttpResponse::error(400, msg))?;

    let result = storage::update_match(host, contest_id, match_id, |m| {
        record_order(m, submitter, body.order).map_err(SdkError::Other)
    });
    let updated: MatchState = result.map_err(|e| match e {
        // The probe above passed (the match was still `Ordering` at that
        // snapshot), but a concurrent `/start` won the race and the CAS
        // closure's re-validation caught it: this is a benign lost race,
        // not an internal error, so it gets the same 4xx treatment as the
        // probe would have given it directly -- not `ApiError`'s generic
        // `SdkError` -> 500 conversion. Any OTHER `SdkError` still falls
        // through to that generic conversion unchanged.
        SdkError::Other(ref msg) if msg == LEFT_ORDERING_PHASE_MSG => {
            ApiError::from(PluginHttpResponse::error(409, LEFT_ORDERING_PHASE_MSG))
        }
        other => ApiError::from(other),
    })?;

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({
            "order_a": updated.order_a,
            "order_b": updated.order_b,
        })),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::model::MatchPhase;

    fn group_a() -> [i32; 3] {
        [101, 102, 103]
    }

    fn group_b() -> [i32; 3] {
        [201, 202, 203]
    }

    fn match_in_ordering(a: i32, b: i32, group_a: [i32; 3], group_b: [i32; 3]) -> MatchState {
        MatchState {
            round: 1,
            pos: 0,
            player_a: a,
            player_b: b,
            group_a,
            group_b,
            state: MatchPhase::Ordering,
            ..Default::default()
        }
    }

    #[test]
    fn player_b_submitting_sets_the_order_imposed_on_player_a() {
        // The direction that is easy to get backwards: B ranks A's problems.
        let mut m = match_in_ordering(/* a */ 10, /* b */ 20, group_a(), group_b());
        record_order(&mut m, 20, [103, 101, 102]).unwrap();
        assert_eq!(
            m.order_a,
            Some([103, 101, 102]),
            "B's ranking governs A's order"
        );
        assert_eq!(m.order_b, None, "B has not been ranked yet");
    }

    #[test]
    fn rejects_an_order_that_is_not_a_permutation_of_the_opponents_group() {
        let mut m = match_in_ordering(10, 20, group_a(), group_b());
        // 999 is not in A's group at all.
        let err = record_order(&mut m, 20, [101, 102, 999]).unwrap_err();
        assert!(err.contains("permutation"), "got: {err}");
    }

    #[test]
    fn rejects_a_ranking_from_someone_who_is_not_in_the_match() {
        let mut m = match_in_ordering(10, 20, group_a(), group_b());
        assert!(record_order(&mut m, 77, [101, 102, 103]).is_err());
    }

    #[test]
    fn resubmitting_before_the_match_starts_replaces_the_previous_ranking() {
        let mut m = match_in_ordering(10, 20, group_a(), group_b());
        record_order(&mut m, 20, [101, 102, 103]).unwrap();
        record_order(&mut m, 20, [103, 102, 101]).unwrap();
        assert_eq!(m.order_a, Some([103, 102, 101]));
    }

    // -- state guard on record_order (DEFECT 2 fix) --

    #[test]
    fn rejects_a_resubmission_once_the_match_has_left_ordering() {
        let mut m = match_in_ordering(10, 20, group_a(), group_b());
        m.state = MatchPhase::InProgress;
        m.order_a = Some([101, 102, 103]);
        let err = record_order(&mut m, 20, [103, 102, 101]).unwrap_err();
        assert!(err.contains("ordering phase"), "got: {err}");
        assert_eq!(
            m.order_a,
            Some([101, 102, 103]),
            "a rejected resubmission must not mutate the existing order"
        );
    }

    #[test]
    fn rejects_a_ranking_submitted_to_an_already_decided_match() {
        // Not just InProgress -- every non-Ordering phase must be covered,
        // not merely the one the QA reproduction happens to exercise.
        let mut m = match_in_ordering(10, 20, group_a(), group_b());
        m.state = MatchPhase::Decided;
        assert!(record_order(&mut m, 20, group_a()).is_err());
    }

    // -- handle_order (host wiring) --

    fn queue_bracket_contest_info(host: &Host) {
        host.db.queue_query_result(serde_json::json!([{
            "contest_type": "codelink-bracket",
            "is_public": true,
            "is_active": true,
            "phase": "during",
        }]));
    }

    fn order_request(
        contest_id: i32,
        match_id: u8,
        submitter: i32,
        order: [i32; 3],
    ) -> PluginHttpRequest {
        let mut params = HashMap::new();
        params.insert("contest_id".to_string(), contest_id.to_string());
        params.insert("match_id".to_string(), match_id.to_string());
        PluginHttpRequest {
            method: "POST".into(),
            path: String::new(),
            params,
            query: HashMap::new(),
            headers: HashMap::new(),
            body: Some(serde_json::json!({ "order": order })),
            auth: Some(PluginHttpAuth {
                user_id: submitter,
                username: format!("player{submitter}"),
                roles: vec![],
                permissions: vec![],
            }),
        }
    }

    fn seed_match(host: &Host, contest: i32, match_id: u8, m: &MatchState) {
        host.storage
            .set(&[(
                storage::match_key(contest, match_id).as_str(),
                serde_json::to_string(m).unwrap().as_str(),
            )])
            .unwrap();
    }

    #[test]
    fn handle_order_still_accepts_a_resubmission_while_ordering() {
        // Negative control: the new guard must not over-reach into the
        // still-legitimate before-start case -- see
        // `resubmitting_before_the_match_starts_replaces_the_previous_ranking`
        // above for the same control at the pure-function level.
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let mut m = match_in_ordering(10, 20, group_a(), group_b());
        m.order_a = Some([101, 102, 103]);
        seed_match(&host, 7, 0, &m);

        let resp = handle_order(&host, &order_request(7, 0, 20, [103, 102, 101])).unwrap();
        assert_eq!(resp.status, 200);
        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.order_a, Some([103, 102, 101]));
    }

    #[test]
    fn handle_order_rejects_a_resubmission_after_the_match_has_started() {
        // Plugin-level mirror of the QA regression test
        // `defect_ranking_can_be_resubmitted_after_the_match_has_started`.
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        let mut m = match_in_ordering(10, 20, group_a(), group_b());
        m.state = MatchPhase::InProgress;
        m.order_a = Some([101, 102, 103]);
        m.order_b = Some([201, 202, 203]);
        seed_match(&host, 7, 0, &m);

        let err = handle_order(&host, &order_request(7, 0, 20, [103, 102, 101])).unwrap_err();
        let resp = err.into_response();
        assert!(
            resp.status < 500,
            "must be a reasoned 4xx, not a bare 500: got {}",
            resp.status
        );
        assert_eq!(resp.status, 400);

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(
            reloaded.order_a,
            Some([101, 102, 103]),
            "a rejected resubmission must not mutate the live match"
        );
    }
}
