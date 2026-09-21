//! `[[server.queries]] topic = "visibility"` -- per-viewer problem and
//! submission visibility for the bracket.
//!
//! # This is the reason the visibility kernel exists
//!
//! For one `Resource::Problem` row, the opponent of the group's owner gets
//! `Allow` while the owner themself gets `Deny` -- the SAME resource,
//! OPPOSITE answers, decided entirely by who is asking. No global
//! "problem is hidden" boolean on the problem itself can express this; see
//! [`decide_problem`]'s doc comment and
//! `the_same_problem_is_allowed_to_the_opponent_and_denied_to_its_owner`
//! below.
//!
//! Rules, in order (quoted from
//! `docs/superpowers/specs/2026-09-19-afternoon-bracket-design.md`):
//!
//! > Let G be the group containing the problem, O the player who owns G, and
//! > V the viewer.
//! >
//! > | Condition | Decision |
//! > |---|---|
//! > | V holds `submission:view_all` | `Allow` |
//! > | V is O's opponent, and the match has reached the ordering phase |
//! >   `Allow` -- V must read it to rank it |
//! > | V is O, and the problem's position in V's imposed order <= current
//! >   小局 index, and the 小局 has opened | `Allow` |
//! > | Problem is the round's active 附加赛 problem and 附加赛 has started |
//! >   `Allow` to both players |
//! > | otherwise | `Deny` |
//!
//! `Resource::Submission`: `Allow` iff the viewer owns it or holds
//! `submission:view_all`; otherwise `Deny`. This single rule also delivers
//! "eliminated players see only their own past matches" with no separate
//! elimination branch: an eliminated player has no current 小局 anywhere, so
//! every problem query of theirs falls through to `Deny` while their own
//! submission history stays visible.

use broccoli_server_sdk::prelude::*;

use crate::model::MatchPhase;

// The rest of this file's imports are used only by `decide_visibility_decisions`
// and its exclusive helpers below, which are themselves gated
// `#[cfg(any(target_arch = "wasm32", test))]` -- a plain (non-wasm32,
// non-test) `cargo build`/`clippy` pass never sees that function, so an
// ungated import here would be flagged unused in exactly that pass. Mirrors
// `plugins/icpc/src/lib.rs`'s per-item cfg gating.
#[cfg(any(target_arch = "wasm32", test))]
use std::collections::HashMap;

#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::permissions as perm;
#[cfg(any(target_arch = "wasm32", test))]
use serde::Deserialize;

#[cfg(any(target_arch = "wasm32", test))]
use crate::model::{MatchState, RoundDef};
#[cfg(any(target_arch = "wasm32", test))]
use crate::storage;

/// Everything [`decide_problem`] needs about ONE match to answer a
/// visibility question, deliberately smaller than [`crate::model::MatchState`]:
/// it holds only what the rules above read, already resolved to the shape
/// the rules are stated against (e.g. "current 小局 index", not the raw
/// `xiaoju` log). Built from a real match by `ctx_from_match` in the host
/// wiring below; tests construct it directly.
#[derive(Debug, Clone)]
pub struct VisibilityCtx {
    pub player_a: i32,
    pub player_b: i32,
    /// Player A's own 3 problems this round.
    pub group_a: [i32; 3],
    /// Player B's own 3 problems this round.
    pub group_b: [i32; 3],
    /// The order player A must solve their own group in, as ranked by B.
    pub order_a: Option<[i32; 3]>,
    /// The order player B must solve their own group in, as ranked by A.
    pub order_b: Option<[i32; 3]>,
    pub state: MatchPhase,
    /// Index of the most recently OPENED 小局, if any has opened yet.
    /// `None` before the first 小局 opens (e.g. during `Ordering`).
    pub current_xiaoju_index: Option<u8>,
    /// The 附加赛 problem currently in play, if `state == Tiebreak`.
    pub tiebreak_problem: Option<i32>,
}

/// Decide whether `viewer` may see `problem_id` in this match. See the
/// module doc comment for the rule table this implements.
///
/// # Why this function, not a `problem.is_public` flag
///
/// A player's own group is hidden from them but visible to their opponent
/// (who must read it to rank it) -- the two viewers disagree about the SAME
/// row. A boolean stored on the problem cannot hold two different answers
/// for two different viewers; a per-(problem, viewer) decision can. That is
/// the entire reason this plugin calls into a visibility kernel instead of
/// setting a flag.
pub fn decide_problem(
    ctx: &VisibilityCtx,
    problem_id: i32,
    viewer: i32,
    can_view_all: bool,
) -> WireDecision {
    if can_view_all {
        return WireDecision::Allow {};
    }

    let (owner, opponent, owners_order) = if ctx.group_a.contains(&problem_id) {
        (ctx.player_a, ctx.player_b, ctx.order_a)
    } else if ctx.group_b.contains(&problem_id) {
        (ctx.player_b, ctx.player_a, ctx.order_b)
    } else {
        // Not in either player's group: only the active 附加赛 problem can
        // still be Allow, and then to both players.
        let is_active_tiebreak_problem =
            ctx.state == MatchPhase::Tiebreak && ctx.tiebreak_problem == Some(problem_id);
        if is_active_tiebreak_problem && (viewer == ctx.player_a || viewer == ctx.player_b) {
            return WireDecision::Allow {};
        }
        return WireDecision::Deny {};
    };

    // The opponent must read the group to rank it, and (assumption #2 in the
    // spec) keeps that access for the rest of the match once granted -- so
    // this checks "reached ordering or later", not "currently in ordering".
    if viewer == opponent && ctx.state != MatchPhase::Pending {
        return WireDecision::Allow {};
    }

    // The owner sees their own problems up to (not just at) the current 小局
    // index -- a solved problem must not vanish -- but never ahead of it
    // (assumption #3: ordering is private until each 小局 opens).
    if viewer == owner
        && let Some(current_index) = ctx.current_xiaoju_index
        && let Some(order) = owners_order
        && let Some(pos) = order.iter().position(|&p| p == problem_id)
        && pos as u8 <= current_index
    {
        return WireDecision::Allow {};
    }

    WireDecision::Deny {}
}

/// Decide whether `viewer` may see a submission owned by `owner`. Players
/// never see each other's submissions, during or after a match.
pub fn decide_submission(owner: i32, viewer: i32, can_view_all: bool) -> WireDecision {
    if can_view_all || viewer == owner {
        WireDecision::Allow {}
    } else {
        WireDecision::Deny {}
    }
}

/// Build a [`VisibilityCtx`] from a real match and the `RoundDef` that owns
/// it.
#[cfg(any(target_arch = "wasm32", test))]
fn ctx_from_match(m: &MatchState, round_def: &RoundDef) -> VisibilityCtx {
    VisibilityCtx {
        player_a: m.player_a,
        player_b: m.player_b,
        group_a: m.group_a,
        group_b: m.group_b,
        order_a: m.order_a,
        order_b: m.order_b,
        state: m.state,
        current_xiaoju_index: m.xiaoju.last().map(|x| x.index),
        tiebreak_problem: round_def.tiebreak.get(m.tiebreak_index).copied(),
    }
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Deserialize)]
struct ContestTypeRow {
    contest_id: i32,
    contest_type: Option<String>,
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Deserialize)]
struct SubmissionOwnerRow {
    submission_id: i32,
    user_id: i32,
    contest_type: Option<String>,
}

// `find_players_match` moved to `storage.rs`: it operates purely on
// `storage::load_all_matches`'s return shape and is shared with `gate.rs`'s
// submission gating, which needs the identical "resolve a problem to a
// round, then find the viewer's match in that round" lookup.

/// Core decision logic for the `visibility` query topic. Exercised directly
/// by tests via `Host::mock()` (no wasm32 target required); the thin
/// `decide_visibility` wrapper below adapts it to the WASM ABI.
///
/// `decisions` is POSITIONAL and always exactly as long as
/// `req.resources` -- a resource this plugin has no opinion about (any kind
/// other than `problem`/`submission`, or one belonging to a non-bracket
/// contest) defaults to `Allow`, the identity element of the host's `meet`,
/// so "no opinion" genuinely composes rather than silently denying.
#[cfg(any(target_arch = "wasm32", test))]
fn decide_visibility_decisions(
    host: &Host,
    req: &VisibilityQueryInput,
) -> Result<Vec<WireDecision>, SdkError> {
    if req
        .subject
        .permissions
        .iter()
        .any(|p| p == perm::SUBMISSION_VIEW_ALL)
    {
        return Ok(req
            .resources
            .iter()
            .map(|_| WireDecision::Allow {})
            .collect());
    }
    let viewer = req.subject.user_id;

    // -- Problem resources: resolve contest_type, then setup + matches, in
    // ONE batched read per distinct contest actually referenced. --
    let mut problem_contest_ids: Vec<i32> = req
        .resources
        .iter()
        .filter(|r| r.kind == "problem")
        .filter_map(|r| r.contest_id)
        .collect();
    problem_contest_ids.sort_unstable();
    problem_contest_ids.dedup();

    let mut bracket_contests: HashMap<i32, ()> = HashMap::new();
    if !problem_contest_ids.is_empty() {
        let mut p = Params::new();
        let placeholders: Vec<String> = problem_contest_ids.iter().map(|id| p.bind(*id)).collect();
        let sql = format!(
            "SELECT id AS contest_id, contest_type FROM contest WHERE id IN ({})",
            placeholders.join(",")
        );
        let rows: Vec<ContestTypeRow> = host.db.query_with_args(&sql, &p.into_args())?;
        for row in rows {
            if row.contest_type.as_deref() == Some("afternoon-bracket") {
                bracket_contests.insert(row.contest_id, ());
            }
        }
    }

    let mut setups: HashMap<i32, crate::model::Setup> = HashMap::new();
    let mut matches: HashMap<i32, Vec<(u8, MatchState)>> = HashMap::new();
    for &contest_id in bracket_contests.keys() {
        if let Some(setup) = storage::load_setup(host, contest_id)? {
            setups.insert(contest_id, setup);
        }
        matches.insert(contest_id, storage::load_all_matches(host, contest_id)?);
    }

    // -- Submission resources: owner + the owning contest's type, in ONE
    // batched SQL query for the whole request. --
    let mut submission_ids: Vec<i32> = req
        .resources
        .iter()
        .filter(|r| r.kind == "submission")
        .filter_map(|r| r.id.parse::<i32>().ok())
        .collect();
    submission_ids.sort_unstable();
    submission_ids.dedup();

    let mut submission_owners: HashMap<i32, (i32, bool)> = HashMap::new();
    if !submission_ids.is_empty() {
        let mut p = Params::new();
        let placeholders: Vec<String> = submission_ids.iter().map(|id| p.bind(*id)).collect();
        let sql = format!(
            "SELECT s.id AS submission_id, s.user_id, c.contest_type \
             FROM submission s JOIN contest c ON c.id = s.contest_id \
             WHERE s.id IN ({})",
            placeholders.join(",")
        );
        let rows: Vec<SubmissionOwnerRow> = host.db.query_with_args(&sql, &p.into_args())?;
        for row in rows {
            let is_bracket = row.contest_type.as_deref() == Some("afternoon-bracket");
            submission_owners.insert(row.submission_id, (row.user_id, is_bracket));
        }
    }

    let decisions = req
        .resources
        .iter()
        .map(|resource| match resource.kind.as_str() {
            "problem" => {
                let (Some(contest_id), Some(problem_id)) =
                    (resource.contest_id, resource.problem_id)
                else {
                    return WireDecision::Allow {};
                };
                if !bracket_contests.contains_key(&contest_id) {
                    return WireDecision::Allow {};
                }
                // From here on this plugin DOES have an opinion: a missing
                // setup, an unresolvable round, or no match for this viewer
                // all fail hidden (Deny) rather than leak -- see the module
                // doc comment's "eliminated player" reasoning.
                let Some(v) = viewer else {
                    return WireDecision::Deny {};
                };
                let Some(setup) = setups.get(&contest_id) else {
                    return WireDecision::Deny {};
                };
                let Some(round_index) = setup.rounds.iter().position(|r| {
                    r.group_a.contains(&problem_id)
                        || r.group_b.contains(&problem_id)
                        || r.tiebreak.contains(&problem_id)
                }) else {
                    return WireDecision::Deny {};
                };
                let round_def = &setup.rounds[round_index];
                let round = (round_index + 1) as u8;
                let Some(contest_matches) = matches.get(&contest_id) else {
                    return WireDecision::Deny {};
                };
                let Some(m) = storage::find_players_match(contest_matches, round, v) else {
                    return WireDecision::Deny {};
                };
                let ctx = ctx_from_match(m, round_def);
                decide_problem(&ctx, problem_id, v, false)
            }
            "submission" => {
                let Ok(sub_id) = resource.id.parse::<i32>() else {
                    return WireDecision::Allow {};
                };
                let Some(&(owner, is_bracket)) = submission_owners.get(&sub_id) else {
                    // Fail hidden: a submission id this plugin was asked
                    // about but cannot resolve is treated as if it might be
                    // one of ours.
                    return WireDecision::Deny {};
                };
                if !is_bracket {
                    return WireDecision::Allow {};
                }
                let Some(v) = viewer else {
                    return WireDecision::Deny {};
                };
                decide_submission(owner, v, false)
            }
            _ => WireDecision::Allow {},
        })
        .collect();

    Ok(decisions)
}

#[cfg(target_arch = "wasm32")]
#[extism_pdk::plugin_fn]
pub fn decide_visibility(input: String) -> extism_pdk::FnResult<String> {
    let host = Host::new();
    let req: VisibilityQueryInput = serde_json::from_str(&input)?;
    let decisions = decide_visibility_decisions(&host, &req)?;
    Ok(serde_json::to_string(&VisibilityQueryOutput { decisions })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_in_ordering(a: i32, b: i32) -> VisibilityCtx {
        VisibilityCtx {
            player_a: a,
            player_b: b,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: None,
            order_b: None,
            state: MatchPhase::Ordering,
            current_xiaoju_index: None,
            tiebreak_problem: None,
        }
    }

    fn ctx_in_progress_at_xiaoju(index: u8, order_a: [i32; 3]) -> VisibilityCtx {
        VisibilityCtx {
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some(order_a),
            order_b: Some([203, 201, 202]),
            state: MatchPhase::InProgress,
            current_xiaoju_index: Some(index),
            tiebreak_problem: None,
        }
    }

    fn ctx_in_tiebreak(tiebreak_problem: i32) -> VisibilityCtx {
        VisibilityCtx {
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some([101, 102, 103]),
            order_b: Some([201, 202, 203]),
            state: MatchPhase::Tiebreak,
            current_xiaoju_index: Some(2),
            tiebreak_problem: Some(tiebreak_problem),
        }
    }

    /// Some player from a different match entirely -- not `10`/`20`. Used
    /// both for "uninvolved spectator" and "eliminated in an earlier round"
    /// cases: both are, from THIS match's point of view, simply "not a
    /// participant", which is exactly the point of
    /// `an_eliminated_player_sees_no_ongoing_match`'s comment below.
    fn eliminated_player() -> i32 {
        30
    }

    #[test]
    fn the_same_problem_is_allowed_to_the_opponent_and_denied_to_its_owner() {
        // THE test for this plugin. One problem row, two viewers, opposite
        // answers - the thing a global "problem is hidden" flag cannot express,
        // and the reason the visibility kernel exists. A regression to any
        // per-problem boolean fails here and nowhere else.
        let ctx = ctx_in_ordering(/* a */ 10, /* b */ 20);
        let a_problem = 101; // belongs to A's group

        assert_eq!(
            decide_problem(&ctx, a_problem, 20, false),
            WireDecision::Allow {},
            "B must read A's problems to rank them"
        );
        assert_eq!(
            decide_problem(&ctx, a_problem, 10, false),
            WireDecision::Deny {},
            "A must NOT see their own group during ordering"
        );
    }

    #[test]
    fn a_player_cannot_see_their_own_later_problems() {
        // 无法提前看到自己组后面的题目 - the rule that makes the imposed order
        // meaningful. Without it a player could plan all three in advance.
        let ctx = ctx_in_progress_at_xiaoju(0, /* order imposed on A */ [103, 101, 102]);
        assert_eq!(
            decide_problem(&ctx, 103, 10, false),
            WireDecision::Allow {},
            "current"
        );
        assert_eq!(
            decide_problem(&ctx, 101, 10, false),
            WireDecision::Deny {},
            "next"
        );
        assert_eq!(
            decide_problem(&ctx, 102, 10, false),
            WireDecision::Deny {},
            "last"
        );
    }

    #[test]
    fn a_players_earlier_problems_stay_visible() {
        // Position <= current, not == current: a solved problem should not vanish.
        let ctx = ctx_in_progress_at_xiaoju(1, [103, 101, 102]);
        assert_eq!(decide_problem(&ctx, 103, 10, false), WireDecision::Allow {});
        assert_eq!(decide_problem(&ctx, 101, 10, false), WireDecision::Allow {});
        assert_eq!(decide_problem(&ctx, 102, 10, false), WireDecision::Deny {});
    }

    #[test]
    fn view_all_sees_everything_including_unopened_problems() {
        let ctx = ctx_in_ordering(10, 20);
        for p in [101, 102, 103, 201, 202, 203] {
            assert_eq!(
                decide_problem(&ctx, p, 999, true),
                WireDecision::Allow {},
                "spectators watch live: problem {p}"
            );
        }
    }

    #[test]
    fn an_uninvolved_player_sees_nothing_from_this_match() {
        let ctx = ctx_in_progress_at_xiaoju(0, [103, 101, 102]);
        for p in [101, 102, 103, 201, 202, 203] {
            assert_eq!(decide_problem(&ctx, p, 555, false), WireDecision::Deny {});
        }
    }

    #[test]
    fn an_eliminated_player_sees_no_ongoing_match() {
        // Falls out of the rules above with no elimination branch: an eliminated
        // player has no current 小局 anywhere, so every problem query reaches Deny.
        let ctx = ctx_in_progress_at_xiaoju(0, [103, 101, 102]);
        assert_eq!(
            decide_problem(&ctx, 101, eliminated_player(), false),
            WireDecision::Deny {}
        );
    }

    #[test]
    fn the_active_tiebreak_problem_is_allowed_to_both_players_during_tiebreak() {
        // Supplementary: the plan's test list does not exercise this branch of
        // the rule table, but the spec requires it explicitly.
        let ctx = ctx_in_tiebreak(319);
        assert_eq!(decide_problem(&ctx, 319, 10, false), WireDecision::Allow {});
        assert_eq!(decide_problem(&ctx, 319, 20, false), WireDecision::Allow {});
        assert_eq!(
            decide_problem(&ctx, 319, 555, false),
            WireDecision::Deny {},
            "an uninvolved viewer is still denied the tiebreak problem"
        );
    }

    #[test]
    fn a_submission_is_visible_to_its_owner() {
        assert_eq!(decide_submission(10, 10, false), WireDecision::Allow {});
    }

    #[test]
    fn a_submission_is_hidden_from_a_non_owner_without_view_all() {
        assert_eq!(decide_submission(10, 20, false), WireDecision::Deny {});
    }

    #[test]
    fn view_all_sees_a_submission_it_does_not_own() {
        assert_eq!(decide_submission(10, 20, true), WireDecision::Allow {});
    }

    fn subject(user_id: Option<i32>, can_view_all: bool) -> QuerySubject {
        QuerySubject {
            user_id,
            authenticated: user_id.is_some(),
            permissions: if can_view_all {
                vec![perm::SUBMISSION_VIEW_ALL.to_string()]
            } else {
                vec![]
            },
        }
    }

    fn round_def() -> RoundDef {
        RoundDef {
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            tiebreak: vec![319],
        }
    }

    fn seed_bracket_contest(host: &Host, contest_id: i32) {
        host.db.queue_query_result(serde_json::json!([
            { "contest_id": contest_id, "contest_type": "afternoon-bracket" }
        ]));
        let setup = crate::model::Setup {
            rounds: vec![round_def()],
            xiaoju_seconds: 1_800,
            round_intermission_seconds: 600,
        };
        host.storage
            .set(&[(
                storage::setup_key(contest_id).as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            )])
            .unwrap();
        host.storage
            .set(&[(
                storage::match_key(contest_id, 0).as_str(),
                serde_json::to_string(&MatchState {
                    round: 1,
                    pos: 0,
                    player_a: 10,
                    player_b: 20,
                    group_a: [101, 102, 103],
                    group_b: [201, 202, 203],
                    order_a: Some([101, 102, 103]),
                    state: MatchPhase::Ordering,
                    ..Default::default()
                })
                .unwrap()
                .as_str(),
            )])
            .unwrap();
    }

    fn problem_resource(contest_id: i32, problem_id: i32) -> QueryResource {
        QueryResource {
            kind: "problem".to_string(),
            id: problem_id.to_string(),
            contest_id: Some(contest_id),
            problem_id: Some(problem_id),
        }
    }

    #[test]
    fn decide_visibility_resolves_the_asymmetric_case_end_to_end() {
        // The same wire-level check as the pure-function test above, but
        // through the whole host entry point: real DB rows and real
        // storage, not a hand-built `VisibilityCtx`.
        let host = Host::mock();
        seed_bracket_contest(&host, 7);
        let req = VisibilityQueryInput {
            subject: subject(Some(20), false),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(7),
            },
            resources: vec![problem_resource(7, 101)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert_eq!(decisions.len(), 1);
        assert!(matches!(decisions[0], WireDecision::Allow {}));

        // Each call to `decide_visibility_decisions` issues its own
        // contest-type lookup -- `DbMock`'s queue is FIFO and consumed once
        // per call, so a second end-to-end call needs a second queued row,
        // not a reuse of the one `seed_bracket_contest` already consumed.
        host.db.queue_query_result(serde_json::json!([
            { "contest_id": 7, "contest_type": "afternoon-bracket" }
        ]));
        let req_owner = VisibilityQueryInput {
            subject: subject(Some(10), false),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(7),
            },
            resources: vec![problem_resource(7, 101)],
        };
        let decisions = decide_visibility_decisions(&host, &req_owner).unwrap();
        assert!(matches!(decisions[0], WireDecision::Deny {}));
    }

    #[test]
    fn decide_visibility_allows_resources_from_a_non_bracket_contest_without_an_opinion() {
        let host = Host::mock();
        host.db.queue_query_result(serde_json::json!([
            { "contest_id": 7, "contest_type": "icpc" }
        ]));
        let req = VisibilityQueryInput {
            subject: subject(Some(999), false),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(7),
            },
            resources: vec![problem_resource(7, 101)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(matches!(decisions[0], WireDecision::Allow {}));
    }

    #[test]
    fn decide_visibility_decisions_are_positional_and_match_resources_len() {
        let host = Host::mock();
        seed_bracket_contest(&host, 7);
        let req = VisibilityQueryInput {
            subject: subject(Some(10), false),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(7),
            },
            resources: vec![
                problem_resource(7, 101),
                QueryResource {
                    kind: "contest".to_string(),
                    id: "7".to_string(),
                    contest_id: Some(7),
                    problem_id: None,
                },
            ],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert_eq!(decisions.len(), req.resources.len());
        assert!(
            matches!(decisions[1], WireDecision::Allow {}),
            "no opinion on Resource::Contest"
        );
    }

    #[test]
    fn decide_visibility_admin_bypass_allows_everything_without_querying() {
        let host = Host::mock();
        let req = VisibilityQueryInput {
            subject: subject(Some(1), true),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(7),
            },
            resources: vec![problem_resource(7, 101)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(matches!(decisions[0], WireDecision::Allow {}));
        assert!(host.db.queries().is_empty());
    }
}
