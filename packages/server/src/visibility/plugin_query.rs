//! Batched plugin visibility query - the point where an UNTRUSTED plugin
//! response enters the host's access-control decision.
//!
//! [`query_plugins`] is **infallible by design**: it returns `Vec<Decision>`,
//! never a `Result`. Every failure mode - a poisoned plugin-registry lock, a
//! plugin timeout/trap/pool-acquire failure, non-JSON or unparseable output,
//! a `decisions` vector whose length doesn't match the resource batch (in
//! EITHER direction), or an unrecognized decision variant - collapses to
//! `Decision::Deny` for every resource in the batch. This mirrors the host's
//! established fail-closed hook stance, already pinned by
//! `plugin_core::hook`'s `non_json_output_fails_closed_reject` and
//! `empty_output_fails_closed_reject` tests: a failed visibility query must
//! become a 404 to the end user, never a 500 (a 500 both leaks that plugin
//! logic exists and takes the scoreboard down mid-contest).
//!
//! The pure mapping ([`decisions_from_output`]) is kept separate from the
//! async plugin call so the failure modes are unit-testable without a
//! plugin host.
//!
//! Multiple plugins may register `[[server.queries]] topic = "visibility"`.
//! Each is queried independently with an identical [`VisibilityQueryInput`],
//! and the per-plugin decision vectors are combined element-wise with
//! [`Decision::meet`] - associative and commutative (Task 2), so the
//! registry's `HashMap`-backed (i.e. unordered) iteration order cannot
//! affect the result.
//!
//! Called from `VisibilityKernel::decide_batch`
//! (`packages/server/src/visibility/mod.rs`, Task 7), alongside
//! `host_rules::host_decide` - only for the resources the host did not
//! already `Deny`.

use broccoli_server_sdk::types::{
    QueryContext, QueryResource, QuerySubject, VisibilityQueryInput, VisibilityQueryOutput,
    WireDecision,
};
use plugin_core::registry::PluginStatus;
use plugin_core::traits::PluginInvokerExt;

use crate::state::AppState;

use super::{Action, Decision, FieldMask, Resource, Subject};

/// One plugin that declared a `[[server.queries]]` entry with
/// `topic = "visibility"` in its manifest: which plugin, and which function
/// to call.
struct VisibilityQuerier {
    plugin_id: String,
    function: String,
}

/// Scan the live plugin registry for every currently-`Loaded` plugin that
/// registered a `[[server.queries]]` entry with `topic = "visibility"`.
///
/// Returns `Err(())` only if the registry's `RwLock` is poisoned (a prior
/// panic while some other code held the write lock) - the caller treats that
/// exactly like any other plugin-side failure and fails the whole batch
/// closed, since a poisoned lock means the set of registered queriers can no
/// longer be established with confidence.
///
/// A plugin that is not `Loaded` (failed to load, crashed, unloaded, ...) is
/// still skipped here exactly as before: zero queriers is deliberately
/// treated as "no plugin has an opinion" and `query_plugins` returns
/// `Decision::Allow` for the whole batch (`Allow` is the `meet` lattice's
/// identity element) - this is correct and must not change, since a contest
/// type with no registered visibility plugin at all relies on the exact same
/// path. What was missing is an alarm for the specific case where a plugin
/// that DECLARED a `topic = "visibility"` query is the one that is not
/// `Loaded`: that plugin's authors expected it to gate every request in its
/// manifest's topic, so its absence silently widens access from "whatever
/// that plugin would have decided" to unconditional Allow, with nothing
/// surfaced anywhere. Log it at `error!` so it is visible without changing
/// the fail-open behavior it is warning about.
fn visibility_queriers(state: &AppState) -> Result<Vec<VisibilityQuerier>, ()> {
    let registry = state.plugins.get_registry().read().map_err(|_| ())?;

    let mut queriers = Vec::new();
    for entry in registry.values() {
        let Some(server) = &entry.manifest.server else {
            continue;
        };
        let declares_visibility_query = server.queries.iter().any(|q| q.topic == "visibility");

        if entry.status != PluginStatus::Loaded {
            if declares_visibility_query {
                tracing::error!(
                    plugin_id = %entry.id,
                    status = ?entry.status,
                    "plugin declares a visibility query but is not Loaded; \
                     its resources will be decided as if it had no opinion \
                     (Allow), not by this plugin's logic"
                );
            }
            continue;
        }

        if declares_visibility_query {
            for query in &server.queries {
                if query.topic == "visibility" {
                    queriers.push(VisibilityQuerier {
                        plugin_id: entry.id.clone(),
                        function: query.function.clone(),
                    });
                }
            }
        }
    }
    Ok(queriers)
}

/// Maximum number of dot-separated segments in one mask path.
///
/// `apply_mask`'s `blank_segments` (`mask.rs`) recurses once per segment that
/// matches real structure (once per non-`*` segment, once per array element
/// for a `*` segment) — an unbounded, plugin-controlled path would let a
/// plugin drive unbounded recursion depth, and a Rust stack overflow is
/// SIGSEGV/abort, not a catchable panic, so the kernel's fail-closed design
/// cannot contain it once it crosses this boundary. The deepest legitimate
/// path in this system today is 4 segments
/// (`result.test_case_results.*.verdict`); 32 leaves generous headroom for
/// deeper DTO shapes a future task might introduce while still bounding
/// worst-case recursion depth to a small, fixed constant.
const MAX_MASK_PATH_SEGMENTS: usize = 32;

/// Maximum byte length of one mask path string.
///
/// Independent of the segment-count check above: a single pathologically
/// long segment containing no `.` would pass the segment check but not this
/// one. 256 bytes comfortably fits every real field/segment name in this
/// codebase (the longest is a few dozen bytes) with generous headroom, while
/// still bounding the string-processing and allocation work spent per path.
const MAX_MASK_PATH_BYTES: usize = 256;

/// Maximum number of field strings any one `Redact` decision may carry for a
/// single resource, enforced TWICE:
///
/// 1. Here, per plugin response, by [`validate_redact_fields`] inside
///    [`decisions_from_output`] - bounds one plugin's own
///    `Vec<String>` before it becomes a `FieldMask`.
/// 2. Again, per resource, on the FINAL decision `query_plugins` returns for
///    that resource, after folding every registered plugin's response
///    together with [`Decision::meet`]. `meet` unions two `Redact` masks
///    (`FieldMask::union`) without revalidating the result, so N plugins each
///    answering right at this cap can fold into one `Redact` carrying up to
///    `N * MAX_MASK_FIELDS` paths - silently past this constant's bound if
///    nothing rechecked the union. `query_plugins` does that recheck (see the
///    loop at the end of that function): the growth there is bounded by the
///    number of plugins that declare `topic = "visibility"`, a trusted,
///    deploy-time-fixed count, not a per-request or attacker-controlled
///    quantity, so this is a precision fix (keeping the constant's meaning
///    accurate for the value that actually ships in a response), not a DoS
///    mitigation.
///
/// Bounds total per-decision validation/allocation work independently of any
/// one path's shape. The largest legitimate `Redact` today (an IOI feedback
/// level) masks a handful of fields; 64 leaves generous headroom without
/// letting a plugin attach an unbounded `Vec<String>` to one decision.
const MAX_MASK_FIELDS: usize = 64;

/// Validate a plugin-supplied `Redact` field list at the trust boundary —
/// the exact point plugin-authored strings are about to become host data via
/// `FieldMask::new`/`apply_mask`. See `MAX_MASK_PATH_SEGMENTS` for why this
/// exists: it must run here, before `FieldMask` is built, not inside
/// `mask.rs`, which stays a pure, total function over already-validated
/// input and must not itself decide access-control failure modes.
///
/// Returns the reason for the first violation found, for logging; the caller
/// treats any violation identically (deny the whole batch), so the reason is
/// diagnostic only, not part of the control flow.
fn validate_redact_fields(fields: &[String]) -> Result<(), &'static str> {
    if fields.len() > MAX_MASK_FIELDS {
        return Err("too many fields in one Redact decision");
    }
    for field in fields {
        if field.len() > MAX_MASK_PATH_BYTES {
            return Err("mask path exceeds max byte length");
        }
        if field.split('.').count() > MAX_MASK_PATH_SEGMENTS {
            return Err("mask path exceeds max segment count");
        }
    }
    Ok(())
}

/// Map a plugin response to decisions. Every failure mode collapses to
/// all-`Deny` — matching the host's established fail-closed hook stance
/// (`plugin_core::hook` tests `non_json_output_fails_closed_reject` and
/// `empty_output_fails_closed_reject`). This includes a `Redact` decision
/// whose `fields` violate `validate_redact_fields` (oversized path, too many
/// segments, or too many fields) — a malformed field mask is a protocol
/// violation exactly like a wrong-length decision vector, not a partial
/// answer to salvage.
fn decisions_from_output(
    plugin_id: &str,
    output: Result<VisibilityQueryOutput, String>,
    expected_len: usize,
) -> Vec<Decision> {
    let deny_all = || vec![Decision::Deny; expected_len];

    let Ok(out) = output else { return deny_all() };
    if out.decisions.len() != expected_len {
        return deny_all();
    }

    for decision in &out.decisions {
        if let WireDecision::Redact { fields } = decision
            && let Err(reason) = validate_redact_fields(fields)
        {
            tracing::error!(
                plugin_id = %plugin_id,
                reason,
                "visibility plugin returned a malformed Redact field mask; denying batch"
            );
            return deny_all();
        }
    }

    out.decisions
        .into_iter()
        .map(|d| match d {
            WireDecision::Allow {} => Decision::Allow,
            WireDecision::Deny {} => Decision::Deny,
            WireDecision::Redact { fields } => Decision::Redact(FieldMask::new(fields)),
        })
        .collect()
}

/// Combine two same-length per-resource decision vectors positionally with
/// [`Decision::meet`]. Split out so order-independence (required because
/// multiple plugins are folded together, and the registry they come from is
/// a `HashMap`) is unit-testable on its own, without needing a plugin host.
///
/// M4: the `debug_assert_eq!` below compiles out in release, but that is
/// safe — it is not the mechanism keeping `acc`/`next` the same length. This
/// function has one call site (`query_plugins`'s fold loop), where `acc`
/// starts as `vec![Decision::Allow; resources.len()]` and `next` is always
/// `decisions_from_output(..., resources.len())`'s return value, which
/// enforces `out.decisions.len() == expected_len` with a plain, unconditional
/// `if` (see `decisions_from_output` above, `deny_all()` on mismatch) — a
/// real check that still runs in release. `zip` would otherwise silently
/// truncate to the shorter length on a mismatch rather than panic, which is
/// exactly the failure mode worth guarding against; the debug assertion
/// exists only to fail loudly, in tests, the moment either of those two
/// real guarantees is ever broken by a future edit — not to provide the
/// guarantee itself.
fn meet_positionally(acc: Vec<Decision>, next: Vec<Decision>) -> Vec<Decision> {
    debug_assert_eq!(acc.len(), next.len());
    acc.into_iter().zip(next).map(|(a, b)| a.meet(b)).collect()
}

/// Batched, per-plugin visibility query. See the module docs for the
/// fail-closed contract. Returns one [`Decision`] per entry of `resources`,
/// in the same order — `Decision::Allow` for every resource when no plugin
/// has registered `topic = "visibility"` (vacuously true: no plugin
/// restricts anything further), which also means no plugin is called at all
/// when the batch is empty or no queriers are registered.
///
/// `contest_id` becomes `QueryContext.contest_id` — the REQUEST's contest
/// scope when the caller's endpoint is itself contest-scoped, `None`
/// otherwise. It is a hint (e.g. useful to a plugin that wants to reject a
/// query outright for an unrecognized contest), never the authority on any
/// individual resource's contest — a batch can span several contests at
/// once, which this single value cannot represent. `resource_contest_ids`
/// (positional with `resources`, same length - see the `debug_assert_eq!`
/// below) is the authority: each entry becomes that resource's own
/// `QueryResource.contest_id` on the wire, resolved by the caller
/// (`VisibilityKernel::decide_batch`,
/// `packages/server/src/visibility/mod.rs`, Task 7) from the resource
/// itself (`Contest`/`Problem`/`Sample`) or from `host_decide`'s submission
/// -> contest_id map (`Submission`). Do not re-derive a batch-wide
/// `contest_id` from `resource_contest_ids` here or anywhere downstream -
/// that is exactly the lossy inference this parameter exists to avoid.
pub(crate) async fn query_plugins(
    state: &AppState,
    subject: &Subject,
    action: Action,
    contest_id: Option<i32>,
    resources: &[Resource],
    resource_contest_ids: &[Option<i32>],
) -> Vec<Decision> {
    if resources.is_empty() {
        return Vec::new();
    }
    // M4: compiled out in release, and that is safe for the same reason as
    // `meet_positionally`'s — this has one call site
    // (`VisibilityKernel::decide_batch`, `mod.rs`), where `target_contest_ids`
    // is built as `plugin_targets.iter().map(...).collect()`: a `.map()` over
    // `plugin_targets` itself is length-preserving by construction,
    // unconditionally, in any build profile — not something a debug
    // assertion is needed to enforce. This assertion exists only to fail
    // loudly, in tests, if a future edit replaces that `.map()` with
    // something that no longer preserves length.
    debug_assert_eq!(
        resources.len(),
        resource_contest_ids.len(),
        "resource_contest_ids must be positional with resources"
    );

    let queriers = match visibility_queriers(state) {
        Ok(queriers) => queriers,
        Err(()) => {
            tracing::error!("visibility plugin registry lock poisoned; denying batch");
            return vec![Decision::Deny; resources.len()];
        }
    };

    if queriers.is_empty() {
        return vec![Decision::Allow; resources.len()];
    }

    let input = VisibilityQueryInput {
        subject: QuerySubject {
            user_id: subject.user_id,
            authenticated: subject.authenticated,
            permissions: subject.permissions.clone(),
        },
        action: action.as_wire().to_string(),
        context: QueryContext { contest_id },
        resources: resources
            .iter()
            .zip(resource_contest_ids.iter())
            .map(|(r, cid)| QueryResource {
                kind: r.wire_kind().to_string(),
                id: r.wire_id(),
                contest_id: *cid,
                problem_id: r.wire_problem_id(),
            })
            .collect(),
    };

    let mut combined = vec![Decision::Allow; resources.len()];
    for querier in &queriers {
        let call_result: Result<VisibilityQueryOutput, _> = state
            .plugins
            .call(&querier.plugin_id, &querier.function, &input)
            .await;

        let output = match call_result {
            Ok(out) => Ok(out),
            Err(e) => {
                tracing::error!(
                    plugin_id = %querier.plugin_id,
                    func = %querier.function,
                    error = %e,
                    "visibility query plugin call failed"
                );
                Err(e.to_string())
            }
        };

        let decisions = decisions_from_output(&querier.plugin_id, output, resources.len());
        combined = meet_positionally(combined, decisions);
    }

    enforce_mask_cap_post_union(combined)
}

/// Reapply `MAX_MASK_FIELDS` to every `Redact` decision AFTER folding all
/// plugins' responses together, since `Decision::meet` unions two `Redact`
/// masks without revalidating the result - see `MAX_MASK_FIELDS`'s doc
/// comment. Any decision whose unioned mask is still within the cap is
/// returned unchanged (the common case: zero or one visibility plugin
/// registered, or several agreeing on overlapping/small masks).
///
/// A decision that ends up over the cap is escalated to `Decision::Deny`,
/// never silently truncated: dropping paths to fit the cap would make the
/// response show MORE than the union of every plugin's `Redact` said should
/// be hidden - an under-redaction, exactly backwards for a fail-closed
/// system. `Deny` is strictly more restrictive than any `Redact`, so this
/// can only narrow what the caller sees, never widen it - consistent with
/// `decisions_from_output`'s existing "any malformed mask denies the whole
/// resource" stance, just applied post-union instead of pre-union.
fn enforce_mask_cap_post_union(decisions: Vec<Decision>) -> Vec<Decision> {
    decisions
        .into_iter()
        .map(|decision| match decision {
            Decision::Redact(mask) if mask.len() > MAX_MASK_FIELDS => {
                tracing::error!(
                    field_count = mask.len(),
                    max = MAX_MASK_FIELDS,
                    "visibility decision's Redact mask exceeds MAX_MASK_FIELDS after \
                     combining multiple plugins' responses; denying this resource"
                );
                Decision::Deny
            }
            other => other,
        })
        .collect()
}

// Re-exported for `super::tests` (`packages/server/src/visibility/mod.rs`,
// Task 7) - see the doc comment on `PanicPluginManager` below.
#[cfg(test)]
pub(crate) use tests::PanicPluginManager;

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    use common::storage::filesystem::FilesystemBlobStore;
    use plugin_core::config::PluginConfig;
    use plugin_core::error::PluginError;
    use plugin_core::host::HostFunctionRegistry;
    use plugin_core::i18n::I18nRegistry;
    use plugin_core::manifest::{
        PluginManifest, ServerConfig as ManifestServerConfig, ServerQuery,
    };
    use plugin_core::registry::{PluginEntry, PluginRegistry};
    use plugin_core::traits::{PluginInvoker, PluginManager};
    use sea_orm::{DatabaseBackend, MockDatabase};

    use crate::config::{
        AppConfig, AuthConfig, BlobStoreConfig, BootstrapConfig, CorsConfig, DatabaseConfig,
        MqAppConfig, ServerConfig, SubmissionConfig,
    };
    use crate::dispatcher::permits::DispatcherSemaphore;
    use crate::registry::{
        CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
        LanguageResolverRegistry, OperationBatches, OperationWaiters,
    };
    use crate::state::RegistryState;

    fn mask(fields: &[&str]) -> FieldMask {
        FieldMask::new(fields.iter().map(|s| s.to_string()))
    }

    // ---------------------------------------------------------------------
    // decisions_from_output - pure, no plugin host needed
    // ---------------------------------------------------------------------

    #[test]
    fn length_mismatch_denies_whole_batch() {
        let out = VisibilityQueryOutput {
            decisions: vec![WireDecision::Allow {}],
        };
        assert_eq!(
            decisions_from_output("test-plugin", Ok(out), 3),
            vec![Decision::Deny, Decision::Deny, Decision::Deny],
            "a short decision vector is a protocol violation, not a partial answer"
        );
    }

    #[test]
    fn decisions_longer_than_resources_denies_whole_batch() {
        // The brief only pins the SHORT case; a plugin that answers for more
        // resources than were asked about is just as much a protocol
        // violation as answering for fewer, and must fail closed the same
        // way - never truncated and treated as a partial answer.
        let out = VisibilityQueryOutput {
            decisions: vec![
                WireDecision::Allow {},
                WireDecision::Allow {},
                WireDecision::Deny {},
            ],
        };
        assert_eq!(
            decisions_from_output("test-plugin", Ok(out), 2),
            vec![Decision::Deny, Decision::Deny],
            "a long decision vector is also a protocol violation, not a partial answer"
        );
    }

    #[test]
    fn plugin_error_denies_whole_batch() {
        assert_eq!(
            decisions_from_output("test-plugin", Err("trap".into()), 2),
            vec![Decision::Deny, Decision::Deny]
        );
    }

    #[test]
    fn well_formed_output_maps_positionally() {
        let out = VisibilityQueryOutput {
            decisions: vec![
                WireDecision::Allow {},
                WireDecision::Redact {
                    fields: vec!["result.verdict".into()],
                },
                WireDecision::Deny {},
            ],
        };
        assert_eq!(
            decisions_from_output("test-plugin", Ok(out), 3),
            vec![
                Decision::Allow,
                Decision::Redact(FieldMask::new(["result.verdict".to_string()])),
                Decision::Deny,
            ]
        );
    }

    // ---------------------------------------------------------------------
    // decisions_from_output - Redact field-mask validation at the trust
    // boundary. A plugin-controlled path drives `apply_mask`'s recursion
    // depth (`mask.rs`), so an oversized/over-segmented path or an
    // oversized field list must fail the whole batch closed here, before
    // `FieldMask::new` ever sees it - never applied partially, never a
    // panic, never a plain error.
    // ---------------------------------------------------------------------

    #[test]
    fn oversized_redact_path_denies_whole_batch() {
        // One byte over MAX_MASK_PATH_BYTES, well within MAX_MASK_PATH_SEGMENTS.
        let long_path = "a".repeat(MAX_MASK_PATH_BYTES + 1);
        let out = VisibilityQueryOutput {
            decisions: vec![WireDecision::Redact {
                fields: vec![long_path],
            }],
        };
        assert_eq!(
            decisions_from_output("test-plugin", Ok(out), 1),
            vec![Decision::Deny],
            "a mask path over the byte limit must deny the batch, not truncate or apply it"
        );
    }

    #[test]
    fn over_segmented_redact_path_denies_whole_batch() {
        // Every segment is one byte, well within MAX_MASK_PATH_BYTES, but
        // there are more of them than MAX_MASK_PATH_SEGMENTS allows.
        let deep_path = vec!["a"; MAX_MASK_PATH_SEGMENTS + 1].join(".");
        let out = VisibilityQueryOutput {
            decisions: vec![WireDecision::Redact {
                fields: vec![deep_path],
            }],
        };
        assert_eq!(
            decisions_from_output("test-plugin", Ok(out), 1),
            vec![Decision::Deny],
            "a mask path over the segment limit must deny the batch, not truncate or apply it"
        );
    }

    #[test]
    fn oversized_redact_field_list_denies_whole_batch() {
        let too_many_fields: Vec<String> = (0..=MAX_MASK_FIELDS).map(|i| format!("f{i}")).collect();
        let out = VisibilityQueryOutput {
            decisions: vec![WireDecision::Redact {
                fields: too_many_fields,
            }],
        };
        assert_eq!(
            decisions_from_output("test-plugin", Ok(out), 1),
            vec![Decision::Deny],
            "a Redact decision with too many fields must deny the batch, not apply a subset"
        );
    }

    #[test]
    fn oversized_redact_field_list_at_non_first_index_denies_whole_batch() {
        // The batch-size-1 sibling test above cannot tell a full scan of
        // `out.decisions` apart from a validator that only ever looks at
        // index 0 - a batch size of 1 has no non-first index. Use a batch
        // of 3 with the oversized Redact at the LAST position.
        let too_many_fields: Vec<String> = (0..=MAX_MASK_FIELDS).map(|i| format!("f{i}")).collect();
        let out = VisibilityQueryOutput {
            decisions: vec![
                WireDecision::Allow {},
                WireDecision::Allow {},
                WireDecision::Redact {
                    fields: too_many_fields,
                },
            ],
        };
        assert_eq!(
            decisions_from_output("test-plugin", Ok(out), 3),
            vec![Decision::Deny, Decision::Deny, Decision::Deny],
            "a Redact decision with too many fields anywhere in the batch - not just at \
             index 0 - must deny the whole batch"
        );
    }

    // ---------------------------------------------------------------------
    // meet_positionally - pure, no plugin host needed
    // ---------------------------------------------------------------------

    #[test]
    fn combining_two_plugin_outputs_is_order_independent() {
        // query_plugins folds one plugin's decisions into another's with
        // `meet`, and the registry it iterates is a HashMap (unordered) -
        // the fold MUST NOT depend on which plugin's output is folded first.
        let a = vec![
            Decision::Allow,
            Decision::Deny,
            Decision::Redact(mask(&["x"])),
        ];
        let b = vec![
            Decision::Redact(mask(&["y"])),
            Decision::Allow,
            Decision::Allow,
        ];

        let a_then_b = meet_positionally(a.clone(), b.clone());
        let b_then_a = meet_positionally(b, a);

        assert_eq!(a_then_b, b_then_a);
        assert_eq!(
            a_then_b,
            vec![
                // meet(Allow, Redact(y)) = Redact(y) - Allow contributes no
                // fields, it's the identity element.
                Decision::Redact(mask(&["y"])),
                // meet(Deny, Allow) = Deny.
                Decision::Deny,
                // meet(Redact(x), Allow) = Redact(x).
                Decision::Redact(mask(&["x"])),
            ]
        );
    }

    // ---------------------------------------------------------------------
    // query_plugins - needs an AppState, so a minimal one is built here.
    // Modeled on `dispatcher::steal::tests::NoopPluginManager` (the only
    // existing precedent in this crate for constructing an AppState in a
    // unit test), but this manager PANICS on any call instead of returning
    // an error, so these tests also serve as a hard check that
    // `query_plugins` does not reach for the plugin host at all when it has
    // no work to do.
    // ---------------------------------------------------------------------

    // `pub(crate)` (rather than the private default every other test-only
    // type in this file uses): Task 7's `VisibilityKernel::decide_batch`
    // tests (`packages/server/src/visibility/mod.rs`) need a plugin-manager
    // double that panics on any call, to prove `admin_override` and a fully
    // host-denied batch never reach for the plugin host - exactly the
    // property this type already exists to prove here. Re-exported above
    // this module (`pub(crate) use tests::PanicPluginManager;`) so `mod.rs`'s
    // own test module can name it as `super::plugin_query::PanicPluginManager`.
    // The fields stay private; construct via `PanicPluginManager::new` instead.
    pub(crate) struct PanicPluginManager {
        registry: PluginRegistry,
        config: PluginConfig,
        host_functions: HostFunctionRegistry,
        i18n: I18nRegistry,
    }

    impl PanicPluginManager {
        pub(crate) fn new(registry: PluginRegistry) -> Self {
            Self {
                registry,
                config: PluginConfig::default(),
                host_functions: HostFunctionRegistry::new(),
                i18n: I18nRegistry::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl PluginInvoker for PanicPluginManager {
        fn get_registry(&self) -> &PluginRegistry {
            &self.registry
        }

        fn get_config(&self) -> &PluginConfig {
            &self.config
        }

        async fn call_raw(
            &self,
            plugin_id: &str,
            func_name: &str,
            _input: Vec<u8>,
        ) -> Result<Vec<u8>, PluginError> {
            panic!(
                "query_plugins must not call plugin '{plugin_id}'::{func_name} here \
                 (no resources, or no registered visibility querier)"
            );
        }
    }

    impl PluginManager for PanicPluginManager {
        fn get_host_functions(&self) -> &HostFunctionRegistry {
            &self.host_functions
        }

        fn get_i18n_registry(&self) -> &I18nRegistry {
            &self.i18n
        }

        fn resolve(&self, _manifest: &PluginManifest) -> Option<(String, Vec<String>)> {
            None
        }
    }

    /// A `Loaded` plugin entry that registers one `visibility` query.
    fn visibility_plugin_entry(plugin_id: &str) -> PluginEntry {
        let manifest = PluginManifest {
            name: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            description: None,
            server: Some(ManifestServerConfig {
                entry: "entry.wasm".to_string(),
                permissions: vec![],
                routes: vec![],
                hooks: vec![],
                queries: vec![ServerQuery {
                    topic: "visibility".to_string(),
                    function: "decide_visibility".to_string(),
                }],
                timers: vec![],
            }),
            worker: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::new(),
        };
        let mut entry = PluginEntry::new(
            plugin_id.to_string(),
            PathBuf::from("/tmp/does-not-need-to-exist"),
            manifest,
        )
        .expect("manifest with no routes builds a valid entry");
        entry.status = PluginStatus::Loaded;
        entry
    }

    /// A `Loaded` plugin entry that registers a server, but no queries at
    /// all (e.g. it only uses `[[server.hooks]]`).
    fn no_query_plugin_entry(plugin_id: &str) -> PluginEntry {
        let manifest = PluginManifest {
            name: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            description: None,
            server: Some(ManifestServerConfig {
                entry: "entry.wasm".to_string(),
                permissions: vec![],
                routes: vec![],
                hooks: vec![],
                queries: vec![],
                timers: vec![],
            }),
            worker: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::new(),
        };
        let mut entry = PluginEntry::new(
            plugin_id.to_string(),
            PathBuf::from("/tmp/does-not-need-to-exist"),
            manifest,
        )
        .expect("manifest with no routes builds a valid entry");
        entry.status = PluginStatus::Loaded;
        entry
    }

    // ---------------------------------------------------------------------
    // query_plugins - the multi-querier fold loop, with calls that SUCCEED.
    //
    // `PanicPluginManager` above proves plugins are never called when they
    // shouldn't be; it cannot exercise the loop that combines two or more
    // *successful* responses, because it panics on any call. `CannedPluginManager`
    // is a second double that answers each `plugin_id` with a canned,
    // pre-serialized response (or a trap), so these tests drive the real
    // `for querier in &queriers` loop in `query_plugins` and the real
    // `HashMap`-backed `PluginRegistry` scan in `visibility_queriers`, not a
    // hand-rolled call to `meet_positionally`.
    // ---------------------------------------------------------------------

    enum CannedResponse {
        Ok(Vec<u8>),
        Trap,
    }

    struct CannedPluginManager {
        registry: PluginRegistry,
        config: PluginConfig,
        host_functions: HostFunctionRegistry,
        i18n: I18nRegistry,
        /// Keyed by `plugin_id` - every `Loaded` entry in `registry` that
        /// registers a `visibility` query MUST have an entry here, or the
        /// call panics (a missing canned response is a test-setup bug, not
        /// a case to fail closed on).
        responses: HashMap<String, CannedResponse>,
    }

    #[async_trait::async_trait]
    impl PluginInvoker for CannedPluginManager {
        fn get_registry(&self) -> &PluginRegistry {
            &self.registry
        }

        fn get_config(&self) -> &PluginConfig {
            &self.config
        }

        async fn call_raw(
            &self,
            plugin_id: &str,
            func_name: &str,
            _input: Vec<u8>,
        ) -> Result<Vec<u8>, PluginError> {
            match self.responses.get(plugin_id) {
                Some(CannedResponse::Ok(bytes)) => Ok(bytes.clone()),
                Some(CannedResponse::Trap) => Err(PluginError::ExecutionFailed {
                    plugin_id: plugin_id.to_string(),
                    func_name: func_name.to_string(),
                    message: "deliberate test trap".to_string(),
                }),
                None => panic!(
                    "CannedPluginManager has no canned response configured for \
                     plugin_id='{plugin_id}' - fix the test"
                ),
            }
        }
    }

    impl PluginManager for CannedPluginManager {
        fn get_host_functions(&self) -> &HostFunctionRegistry {
            &self.host_functions
        }

        fn get_i18n_registry(&self) -> &I18nRegistry {
            &self.i18n
        }

        fn resolve(&self, _manifest: &PluginManifest) -> Option<(String, Vec<String>)> {
            None
        }
    }

    fn canned_output(decisions: Vec<WireDecision>) -> Vec<u8> {
        serde_json::to_vec(&VisibilityQueryOutput { decisions })
            .expect("serialize canned VisibilityQueryOutput")
    }

    /// Builds a fresh `PluginRegistry` by inserting one `visibility_plugin_entry`
    /// per id, in the given sequence. `PluginRegistry` is a `HashMap`, so this
    /// does NOT guarantee the resulting iteration order matches insertion
    /// order (`std`'s `HashMap` never promises that) - which is exactly why
    /// `query_plugins`'s fold must not depend on it. What this DOES guarantee
    /// is that the test drives a registry built independently of any other
    /// test's registry, with the ids inserted in the stated sequence, so a
    /// regression that made the registry/fold insertion-order-sensitive (e.g.
    /// swapping `HashMap` for an order-preserving map plus a `break` after
    /// the first querier) would have a registry here to be sensitive to.
    fn registry_with_order(plugin_ids: &[&str]) -> PluginRegistry {
        let mut map = HashMap::new();
        for id in plugin_ids {
            let entry = visibility_plugin_entry(id);
            map.insert(entry.id.clone(), entry);
        }
        Arc::new(RwLock::new(map))
    }

    /// Mirrors `dispatcher::steal::tests`'s AppState literal (the sole
    /// existing precedent for this in the crate) so field drift between the
    /// two is easy to spot in review.
    async fn test_app_state(plugins: Arc<dyn PluginManager>) -> AppState {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let blob_dir = tempfile::tempdir().expect("create blob tempdir");
        let blob_store = Arc::new(
            FilesystemBlobStore::new(blob_dir.path().join("blobs"), 1024 * 1024)
                .await
                .expect("create blob store"),
        );
        let (metrics, prometheus_registry) =
            common::observability::init_metrics("broccoli-visibility-plugin-query-test");

        let operation_batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches: EvaluateBatches = Arc::new(dashmap::DashMap::new());
        let contest_type_registry: ContestTypeRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let evaluator_registry: EvaluatorRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let checker_stage_registry: CheckerStageRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let language_resolver_registry: LanguageResolverRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        AppState {
            plugins,
            db,
            config: AppConfig {
                server: ServerConfig {
                    host: "127.0.0.1".to_string(),
                    port: 0,
                    cors: CorsConfig {
                        allow_origins: vec![],
                        max_age: 3600,
                    },
                    public_base_url: None,
                    frontend_dist: PathBuf::from("/srv/dist"),
                    trusted_proxies: vec![],
                    rate_limit_auth: false,
                    id: String::new(),
                    expects_multi_replica: false,
                    dispatcher_lease_steal_enabled: true,
                    dispatcher_semaphore_enabled: true,
                    dispatcher_concurrency: 1,
                    dispatcher_admission_queue_max: 0,
                    max_queued_submissions: 0,
                    lease_ttl_secs: 1,
                    lease_refresh_interval_secs: 10,
                    steal_scan_interval_secs: 15,
                    steal_batch_size: 8,
                    sweep_interval_secs: 300,
                    max_dispatch_retries: 5,
                    max_stuck_retries: 5,
                    max_system_error_retries: 50,
                    sweeper_dry_run: true,
                    operation_reaper_enabled: false,
                    operation_reaper_interval_secs: 30,
                    operation_reaper_grace_secs: 30,
                    operation_reaper_max_requeues_per_tick: 1000,
                    operation_reaper_dry_run: false,
                    cancel_primitive_enabled: false,
                    max_blocking_threads: None,
                    batch_evaluator_fanout_concurrency: 64,
                    operation_batch_publish_concurrency: 32,
                    healthz_listen: None,
                    healthz_worker_threads: 2,
                    claim_fiber_enabled: false,
                    claim_poll_interval_ms: 1000,
                    claim_batch_size: 32,
                },
                database: DatabaseConfig {
                    url: "mock://visibility-plugin-query-test".to_string(),
                    max_connections: 1,
                    plugin_max_connections: 1,
                    plugin_privileged_max_connections: 1,
                    plugin_url: None,
                },
                auth: AuthConfig {
                    jwt_secret: "test-secret".to_string(),
                    secure_cookies: false,
                    login_failure_limit: 0,
                    login_failure_window_secs: 60,
                },
                plugin: PluginConfig::default(),
                submission: SubmissionConfig::default(),
                storage: BlobStoreConfig::default(),
                mq: MqAppConfig {
                    enabled: false,
                    ..Default::default()
                },
                observability: common::config::ObservabilityConfig::default(),
                batch_max_age_secs: 600,
                bootstrap: BootstrapConfig::default(),
            },
            mq: None,
            redis_client: None,
            blob_store,
            registries: RegistryState {
                contest_type_registry,
                evaluator_registry,
                checker_stage_registry,
                language_resolver_registry,
                operation_batches,
                operation_waiters,
                evaluate_batches,
                hook_registry: crate::hooks::new_shared_registry(),
            },
            device_codes: Arc::new(dashmap::DashMap::new()),
            metrics,
            prometheus_registry,
            dispatcher_permits: DispatcherSemaphore::new(true, 1, 0),
            login_throttle: Arc::new(crate::utils::login_throttle::LoginThrottle::new(
                0,
                std::time::Duration::from_secs(60),
            )),
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn empty_resource_batch_returns_empty_and_calls_no_plugin() {
        let _guard = crate::metrics_test_lock();

        let entry = visibility_plugin_entry("vis-plugin");
        let mut registry_map = HashMap::new();
        registry_map.insert(entry.id.clone(), entry);

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager {
            registry: Arc::new(RwLock::new(registry_map)),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();

        // A registered visibility querier exists, so if query_plugins reached
        // for the plugin host at all here, PanicPluginManager would panic and
        // fail this test - the assertion below only runs if it did not.
        let result = query_plugins(&state, &subject, Action::Read, None, &[], &[]).await;

        assert_eq!(result, Vec::<Decision>::new());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn no_registered_queriers_allows_without_calling_any_plugin() {
        let _guard = crate::metrics_test_lock();

        let entry = no_query_plugin_entry("no-queries");
        let mut registry_map = HashMap::new();
        registry_map.insert(entry.id.clone(), entry);

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager {
            registry: Arc::new(RwLock::new(registry_map)),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1), Resource::Submission(2)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(result, vec![Decision::Allow, Decision::Allow]);
    }

    /// A plugin that DECLARES a `topic = "visibility"` query but is not
    /// `Loaded` (e.g. it failed to load) must be excluded from `queriers`
    /// exactly like one with no query at all - this is the fail-open
    /// behavior that must NOT change. `visibility_queriers` additionally
    /// logs an `error!` for this specific case (declared-but-not-Loaded, as
    /// opposed to never-declared), but that is an observability addition
    /// only; this test pins that the decision output is unaffected, using
    /// `PanicPluginManager` to also prove the not-`Loaded` plugin is never
    /// called.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn declared_but_not_loaded_querier_is_excluded_and_still_allows() {
        let _guard = crate::metrics_test_lock();

        let mut entry = visibility_plugin_entry("declared-but-failed");
        entry.status = PluginStatus::Failed("deliberate test failure".to_string());
        let mut registry_map = HashMap::new();
        registry_map.insert(entry.id.clone(), entry);

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager {
            registry: Arc::new(RwLock::new(registry_map)),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1), Resource::Submission(2)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(result, vec![Decision::Allow, Decision::Allow]);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn poisoned_registry_lock_denies_whole_batch() {
        let _guard = crate::metrics_test_lock();

        let registry: PluginRegistry = Arc::new(RwLock::new(HashMap::new()));
        {
            let registry = registry.clone();
            // Deliberately poison the lock, mirroring how a panic elsewhere
            // in the process (e.g. mid plugin-load) would leave it.
            let _ = std::thread::spawn(move || {
                let _write_guard = registry.write().unwrap();
                panic!("deliberately poisoning the registry lock for this test");
            })
            .join();
        }
        assert!(registry.is_poisoned());

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1), Resource::Submission(2)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(result, vec![Decision::Deny, Decision::Deny]);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn two_queriers_allow_and_deny_combine_to_deny() {
        let _guard = crate::metrics_test_lock();

        let registry = registry_with_order(&["plugin-allow", "plugin-deny"]);

        let mut responses = HashMap::new();
        responses.insert(
            "plugin-allow".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Allow {}])),
        );
        responses.insert(
            "plugin-deny".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Deny {}])),
        );

        let plugins: Arc<dyn PluginManager> = Arc::new(CannedPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            responses,
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(
            result,
            vec![Decision::Deny],
            "one plugin allowing must not rescue a resource the other denies"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn two_queriers_redact_different_fields_union_the_mask() {
        let _guard = crate::metrics_test_lock();

        let registry = registry_with_order(&["plugin-redact-a", "plugin-redact-b"]);

        let mut responses = HashMap::new();
        responses.insert(
            "plugin-redact-a".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Redact {
                fields: vec!["result.verdict".to_string()],
            }])),
        );
        responses.insert(
            "plugin-redact-b".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Redact {
                fields: vec!["result.score".to_string()],
            }])),
        );

        let plugins: Arc<dyn PluginManager> = Arc::new(CannedPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            responses,
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(
            result,
            vec![Decision::Redact(mask(&["result.verdict", "result.score"]))],
            "two plugins redacting different fields must combine into the UNION \
             of both masks, not either plugin's mask alone"
        );
    }

    // ---------------------------------------------------------------------
    // query_plugins - MAX_MASK_FIELDS enforcement AFTER the fold (I10).
    //
    // `oversized_redact_field_list_denies_whole_batch` above already pins
    // the pre-existing per-response check inside `decisions_from_output`.
    // These two pin the second, separate check: `enforce_mask_cap_post_union`
    // revalidates the cap on the FINAL decision, after `Decision::meet` has
    // unioned every querier's mask together. Each plugin's own answer below
    // is well within the cap individually, so the pre-existing per-response
    // check lets both through - the only thing that can catch an over-cap
    // union is the post-union recheck.
    // ---------------------------------------------------------------------

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn two_queriers_redact_union_over_cap_denies_instead_of_over_redacting() {
        let _guard = crate::metrics_test_lock();

        // Disjoint field sets, 40 fields each (well under MAX_MASK_FIELDS =
        // 64 individually), whose union is 80 fields - over the cap.
        let fields_a: Vec<String> = (0..40).map(|i| format!("a{i}")).collect();
        let fields_b: Vec<String> = (0..40).map(|i| format!("b{i}")).collect();

        let registry = registry_with_order(&["plugin-redact-a", "plugin-redact-b"]);

        let mut responses = HashMap::new();
        responses.insert(
            "plugin-redact-a".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Redact {
                fields: fields_a,
            }])),
        );
        responses.insert(
            "plugin-redact-b".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Redact {
                fields: fields_b,
            }])),
        );

        let plugins: Arc<dyn PluginManager> = Arc::new(CannedPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            responses,
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(
            result,
            vec![Decision::Deny],
            "a unioned Redact mask over MAX_MASK_FIELDS must deny the resource - \
             shipping it as Redact would show MORE fields than either plugin's \
             own answer said should be hidden, an under-redaction"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn two_queriers_redact_union_exactly_at_cap_stays_redact() {
        let _guard = crate::metrics_test_lock();

        // Boundary check for the same fold: a union that lands EXACTLY on
        // MAX_MASK_FIELDS must still ship as `Redact`, not be over-denied by
        // an off-by-one in `enforce_mask_cap_post_union`.
        let fields_a: Vec<String> = (0..32).map(|i| format!("a{i}")).collect();
        let fields_b: Vec<String> = (0..32).map(|i| format!("b{i}")).collect();
        assert_eq!(fields_a.len() + fields_b.len(), MAX_MASK_FIELDS);

        let registry = registry_with_order(&["plugin-redact-a", "plugin-redact-b"]);

        let mut responses = HashMap::new();
        responses.insert(
            "plugin-redact-a".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Redact {
                fields: fields_a,
            }])),
        );
        responses.insert(
            "plugin-redact-b".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Redact {
                fields: fields_b,
            }])),
        );

        let plugins: Arc<dyn PluginManager> = Arc::new(CannedPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            responses,
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        match &result[0] {
            Decision::Redact(mask) => assert_eq!(
                mask.len(),
                MAX_MASK_FIELDS,
                "a union landing exactly on the cap must not be truncated or denied"
            ),
            other => panic!("expected Redact exactly at the cap, got {other:?}"),
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn two_queriers_registered_in_opposite_order_produce_the_same_result() {
        let _guard = crate::metrics_test_lock();

        // Same plugin ids and canned responses as
        // `two_queriers_allow_and_deny_combine_to_deny`, but inserted into
        // the registry in the OPPOSITE sequence. `PluginRegistry` is a
        // `HashMap`, so this does not force a particular physical traversal
        // order - the point is that `query_plugins` must produce the same
        // answer regardless of that order, i.e. a `break`/early-return after
        // the first querier (which would silently drop whichever plugin the
        // registry happens to visit second) is the exact bug this pins.
        let registry = registry_with_order(&["plugin-deny", "plugin-allow"]);

        let mut responses = HashMap::new();
        responses.insert(
            "plugin-allow".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Allow {}])),
        );
        responses.insert(
            "plugin-deny".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Deny {}])),
        );

        let plugins: Arc<dyn PluginManager> = Arc::new(CannedPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            responses,
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(
            result,
            vec![Decision::Deny],
            "registering the same two plugins in the opposite order must give a \
             byte-identical result"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn middle_querier_trap_denies_whole_batch_despite_other_two_allowing() {
        let _guard = crate::metrics_test_lock();

        let registry = registry_with_order(&["plugin-a", "plugin-b-trap", "plugin-c"]);

        let mut responses = HashMap::new();
        responses.insert(
            "plugin-a".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Allow {}])),
        );
        responses.insert("plugin-b-trap".to_string(), CannedResponse::Trap);
        responses.insert(
            "plugin-c".to_string(),
            CannedResponse::Ok(canned_output(vec![WireDecision::Allow {}])),
        );

        let plugins: Arc<dyn PluginManager> = Arc::new(CannedPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            responses,
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1)];

        let resource_contest_ids = vec![Some(1); resources.len()];
        let result = query_plugins(
            &state,
            &subject,
            Action::Read,
            Some(1),
            &resources,
            &resource_contest_ids,
        )
        .await;

        assert_eq!(
            result,
            vec![Decision::Deny],
            "one plugin trapping must deny the whole batch even though the \
             other two allowed - the third querier's response cannot rescue it"
        );
    }
}
