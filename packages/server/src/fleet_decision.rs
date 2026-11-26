//! The one decision over the fleet's plugins: which replicas take browsers
//! (`GET /readyz`, the probe the shipped gateways health-check), and what a
//! request on a replica that lacks a plugin answers. Readiness, admission
//! (`PluginLifecycle::outage`) and the admin overview read this one
//! [`Decision`], so a request is refused as missing here exactly while the
//! gateway can send it to a ready replica that has the plugin.
//!
//! - A replica serves a plugin only in the version the fleet published: its
//!   report says `loaded` for it. An outdated or mismatched copy runs, but
//!   is not the plugin the fleet published, so it counts as missing.
//! - A replica that has not yet applied a plugin's newest publish (its report
//!   has no state for the plugin, one of an older generation, or says it is
//!   not loaded yet) is settling:
//!   its absence counts for nothing until it has, so a publish or an enable
//!   never takes the replicas still loading it out of rotation.
//! - A replica is ready while it lacks no enabled plugin another replica
//!   serves. A fleet whose replicas serve different plugins is misconfigured
//!   ([`Decision::mixed`]); so that it still serves, when that leaves no
//!   replica ready, the one serving the most plugins (then the least server
//!   id) is ready.

use std::collections::{BTreeMap, BTreeSet};

use crate::plugin_fleet::{ReplicaPluginStatus, ReplicaReport};

/// One replica as the decision reads it: its id and its latest report.
#[derive(Debug, Clone, Copy)]
pub struct Replica<'a> {
    pub server_id: &'a str,
    pub report: &'a ReplicaReport,
}

/// One replica's part of the [`Decision`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaDecision {
    pub server_id: String,
    /// The enabled plugins it serves in their published version.
    pub serves: BTreeSet<String>,
    /// The enabled plugins whose newest publish it has not applied yet.
    pub settling: BTreeSet<String>,
    /// The enabled plugins it lacks, settled, while another replica serves
    /// them, sorted.
    pub missing: BTreeSet<String>,
    /// Whether it takes browsers.
    pub ready: bool,
}

/// The fleet decision over `replicas` (this replica and its live peers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    /// Each replica, by server id.
    pub replicas: Vec<ReplicaDecision>,
}

impl Decision {
    /// Decide over `replicas`, with `enabled` each enabled plugin and the
    /// generation the fleet last published it at.
    pub fn of(enabled: &BTreeMap<String, i64>, replicas: &[Replica<'_>]) -> Self {
        let mut decided: Vec<ReplicaDecision> = replicas
            .iter()
            .map(|replica| {
                let mut serves = BTreeSet::new();
                let mut settling = BTreeSet::new();
                for (plugin_id, &generation) in enabled {
                    match replica.report.get(plugin_id) {
                        Some(state)
                            if state.generation < generation
                                || state.status == ReplicaPluginStatus::Unloaded =>
                        {
                            settling.insert(plugin_id.clone());
                        }
                        Some(state) if state.status == ReplicaPluginStatus::Loaded => {
                            serves.insert(plugin_id.clone());
                        }
                        Some(_) => {}
                        None => {
                            settling.insert(plugin_id.clone());
                        }
                    }
                }
                ReplicaDecision {
                    server_id: replica.server_id.to_string(),
                    serves,
                    settling,
                    missing: BTreeSet::new(),
                    ready: false,
                }
            })
            .collect();
        decided.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        let served: Vec<BTreeSet<String>> = decided.iter().map(|r| r.serves.clone()).collect();
        for (i, replica) in decided.iter_mut().enumerate() {
            replica.missing = served
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, theirs)| theirs)
                .filter(|id| !replica.serves.contains(*id) && !replica.settling.contains(*id))
                .cloned()
                .collect();
            replica.ready = replica.missing.is_empty();
        }
        if !decided.iter().any(|r| r.ready) {
            let best = decided
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    a.serves
                        .len()
                        .cmp(&b.serves.len())
                        .then_with(|| b.server_id.cmp(&a.server_id))
                })
                .map(|(i, _)| i);
            if let Some(best) = best {
                decided[best].ready = true;
            }
        }
        Decision { replicas: decided }
    }

    /// `server_id`'s part of the decision.
    pub fn get(&self, server_id: &str) -> Option<&ReplicaDecision> {
        self.replicas.iter().find(|r| r.server_id == server_id)
    }

    /// Whether a ready replica other than `server_id` serves `plugin_id`:
    /// the gateway can send a request `server_id` cannot serve to it.
    pub fn served_by_ready_peer(&self, server_id: &str, plugin_id: &str) -> bool {
        self.replicas
            .iter()
            .any(|r| r.server_id != server_id && r.ready && r.serves.contains(plugin_id))
    }

    /// Whether the replicas serve different enabled plugins, settled: a
    /// misconfigured fleet, since a browser pinned to one loses what it
    /// lacks.
    pub fn mixed(&self) -> bool {
        self.replicas.iter().any(|r| !r.missing.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_fleet::ReplicaPluginState;

    fn report(entries: &[(&str, ReplicaPluginStatus, i64)]) -> ReplicaReport {
        entries
            .iter()
            .map(|(id, status, generation)| {
                (
                    id.to_string(),
                    ReplicaPluginState {
                        status: *status,
                        error: None,
                        generation: *generation,
                        retry_in_ms: None,
                        version: None,
                    },
                )
            })
            .collect()
    }

    fn enabled(ids: &[&str]) -> BTreeMap<String, i64> {
        ids.iter().map(|id| (id.to_string(), 1)).collect()
    }

    fn ready(decision: &Decision) -> Vec<(&str, bool)> {
        decision
            .replicas
            .iter()
            .map(|r| (r.server_id.as_str(), r.ready))
            .collect()
    }

    use ReplicaPluginStatus::{Failed, Loaded, Mismatched, NotInstalled, Outdated};

    /// Replicas that each serve a plugin the other lacks are not both ready
    /// while each refuses the other's plugin: one decision makes the one
    /// that serves more (then the least id) ready, and a request for a
    /// plugin only an unready replica serves is not sent there.
    #[test]
    fn replicas_serving_different_plugins_leave_one_ready_by_a_stable_tie_break() {
        let d = report(&[("a", Loaded, 1), ("b", NotInstalled, 1)]);
        let e = report(&[("a", NotInstalled, 1), ("b", Loaded, 1)]);
        let decision = Decision::of(
            &enabled(&["a", "b"]),
            &[
                Replica {
                    server_id: "e",
                    report: &e,
                },
                Replica {
                    server_id: "d",
                    report: &d,
                },
            ],
        );
        assert_eq!(ready(&decision), [("d", true), ("e", false)]);
        assert!(decision.mixed());
        assert!(decision.served_by_ready_peer("e", "a"), "e sends a to d");
        assert!(
            !decision.served_by_ready_peer("d", "b"),
            "no ready server has b"
        );

        // The one serving more is ready, whatever its id.
        let f = report(&[("a", Loaded, 1), ("b", NotInstalled, 1), ("c", Loaded, 1)]);
        let g = report(&[
            ("a", NotInstalled, 1),
            ("b", Loaded, 1),
            ("c", NotInstalled, 1),
        ]);
        let decision = Decision::of(
            &enabled(&["a", "b", "c"]),
            &[
                Replica {
                    server_id: "a-first",
                    report: &g,
                },
                Replica {
                    server_id: "z-last",
                    report: &f,
                },
            ],
        );
        assert_eq!(ready(&decision), [("a-first", false), ("z-last", true)]);
    }

    /// A replica that lacks any enabled plugin another serves is not ready;
    /// one serving everything the others do is.
    #[test]
    fn a_replica_lacking_a_plugin_another_serves_is_not_ready() {
        let full = report(&[("a", Loaded, 1), ("b", Loaded, 1)]);
        let short = report(&[("a", Loaded, 1), ("b", Failed, 1)]);
        let decision = Decision::of(
            &enabled(&["a", "b"]),
            &[
                Replica {
                    server_id: "full",
                    report: &full,
                },
                Replica {
                    server_id: "short",
                    report: &short,
                },
            ],
        );
        assert_eq!(ready(&decision), [("full", true), ("short", false)]);
        assert_eq!(
            decision.get("short").unwrap().missing,
            ["b".to_string()].into()
        );
        // A plugin no replica serves leaves every replica ready.
        let decision = Decision::of(
            &enabled(&["a", "b"]),
            &[
                Replica {
                    server_id: "x",
                    report: &short,
                },
                Replica {
                    server_id: "y",
                    report: &short,
                },
            ],
        );
        assert_eq!(ready(&decision), [("x", true), ("y", true)]);
        assert!(!decision.mixed());
        // A disabled plugin counts for nothing.
        let decision = Decision::of(
            &enabled(&["a"]),
            &[
                Replica {
                    server_id: "full",
                    report: &full,
                },
                Replica {
                    server_id: "short",
                    report: &short,
                },
            ],
        );
        assert_eq!(ready(&decision), [("full", true), ("short", true)]);
    }

    /// A replica still applying a plugin's newest publish stays ready: an
    /// enable or a publish never takes the replicas loading it out of
    /// rotation. Once it has applied it, its absence counts.
    #[test]
    fn a_replica_still_applying_a_publish_stays_ready() {
        let published = BTreeMap::from([("a".to_string(), 2)]);
        let applied = report(&[("a", Loaded, 2)]);
        let behind = report(&[("a", NotInstalled, 1)]);
        let unseen = report(&[]);
        let decision = Decision::of(
            &published,
            &[
                Replica {
                    server_id: "applied",
                    report: &applied,
                },
                Replica {
                    server_id: "behind",
                    report: &behind,
                },
                Replica {
                    server_id: "unseen",
                    report: &unseen,
                },
            ],
        );
        assert_eq!(
            ready(&decision),
            [("applied", true), ("behind", true), ("unseen", true)]
        );
        assert_eq!(
            decision.get("behind").unwrap().settling,
            ["a".to_string()].into()
        );
        let failed = report(&[("a", Failed, 2)]);
        let decision = Decision::of(
            &published,
            &[
                Replica {
                    server_id: "applied",
                    report: &applied,
                },
                Replica {
                    server_id: "failed",
                    report: &failed,
                },
            ],
        );
        assert_eq!(ready(&decision), [("applied", true), ("failed", false)]);
    }

    /// An outdated or mismatched copy runs, but is not the version the fleet
    /// published: it serves nothing in the decision.
    #[test]
    fn an_outdated_or_mismatched_copy_is_not_serving_the_published_plugin() {
        let current = report(&[("a", Loaded, 1)]);
        for stale in [Outdated, Mismatched] {
            let old = report(&[("a", stale, 1)]);
            let decision = Decision::of(
                &enabled(&["a"]),
                &[
                    Replica {
                        server_id: "current",
                        report: &current,
                    },
                    Replica {
                        server_id: "old",
                        report: &old,
                    },
                ],
            );
            assert_eq!(
                ready(&decision),
                [("current", true), ("old", false)],
                "{stale:?}"
            );
        }
    }
}
