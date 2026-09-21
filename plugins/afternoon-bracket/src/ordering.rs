//! `/matches/{match_id}/order` -- each player ranks the OPPONENT's 3
//! problems before a match starts.
//!
//! # The direction is the single most likely bug in this plugin
//!
//! The submitter ranks the OPPONENT's problems, not their own: `submitter`
//! validates against `match_state.group_a` when `submitter == player_b`, and
//! the result is written into `order_a` -- the order imposed ON player A.
//! See the doc comment on [`crate::model::MatchState`] for the full
//! reasoning, and [`player_b_submitting_sets_the_order_imposed_on_player_a`]
//! below for the test that asserts the direction explicitly (a test that
//! only exercises the symmetric case, both players ranking at once, cannot
//! catch the fields being swapped).

use broccoli_server_sdk::prelude::*;
use serde::Deserialize;

use crate::model::MatchState;
use crate::storage;

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
/// Resubmitting before the match starts replaces the previous ranking; there
/// is no once-only guard here because nothing in this function has enough
/// information to know whether the match has started -- that is the route
/// handler's responsibility.
pub fn record_order(
    match_state: &mut MatchState,
    submitter: i32,
    order: [i32; 3],
) -> Result<(), String> {
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
    info.require_type("afternoon-bracket")?;
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
    // the loop's `SdkError` return type. This is safe because the fields
    // `record_order` depends on (player_a, player_b, group_a, group_b) are
    // set once at match creation and never change; only order_a/order_b
    // (which `record_order` does not read) can change concurrently.
    let mut probe = current.clone();
    record_order(&mut probe, submitter, body.order)
        .map_err(|msg| PluginHttpResponse::error(400, msg))?;

    let updated = storage::update_match(host, contest_id, match_id, |m| {
        record_order(m, submitter, body.order).map_err(SdkError::Other)
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
}
