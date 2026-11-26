//! Markers for what a response leaves out because a plugin decides it.

use serde::{Deserialize, Serialize};

/// Why a plugin cannot serve what it provides right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PluginOutage {
    /// An administrator disabled the plugin. Nothing changes until it is
    /// enabled again.
    Disabled {
        #[schema(example = "my-plugin")]
        plugin_id: String,
    },
    /// The plugin is enabled and loaded on no live server (it failed to load
    /// everywhere, or the fleet is reloading it). Nothing changes until a
    /// server that loads it is live.
    NotLoaded {
        #[schema(example = "my-plugin")]
        plugin_id: String,
    },
    /// The plugin is enabled and served by another live server, but not
    /// loaded on this one (it failed to load here, or this server is behind
    /// the fleet). The gateway moves this server's browsers to one that
    /// serves it once this server reports not ready, so a retry soon lands
    /// there.
    NotLoadedHere {
        #[schema(example = "my-plugin")]
        plugin_id: String,
    },
    /// The plugin is enabled but cannot answer on this server right now (it
    /// is loading, reloading or busy). A retry may succeed.
    Unavailable,
}

impl PluginOutage {
    /// How long the outage lasts, longest first: a disabled plugin waits for
    /// an administrator, a plugin loaded nowhere for a server to load it, a
    /// plugin missing here for the gateway to move the browser, and an
    /// unavailable one for this server.
    fn rank(&self) -> u8 {
        match self {
            PluginOutage::Disabled { .. } => 0,
            PluginOutage::NotLoaded { .. } => 1,
            PluginOutage::NotLoadedHere { .. } => 2,
            PluginOutage::Unavailable => 3,
        }
    }

    /// Whether some replica enforces what the plugin governs despite the
    /// outage: another serves it (`NotLoadedHere`), or this one is loading
    /// it (`Unavailable`). A disabled plugin, or one loaded nowhere, is
    /// enforced by none.
    pub fn enforced_elsewhere(&self) -> bool {
        matches!(
            self,
            PluginOutage::NotLoadedHere { .. } | PluginOutage::Unavailable
        )
    }

    /// The plugin the outage names, if it names one.
    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            PluginOutage::Disabled { plugin_id }
            | PluginOutage::NotLoaded { plugin_id }
            | PluginOutage::NotLoadedHere { plugin_id } => Some(plugin_id),
            PluginOutage::Unavailable => None,
        }
    }

    /// The outage of two plugins at once: the one that lasts longer (a
    /// disabled plugin, then one loaded nowhere, then one missing here, then
    /// an unavailable one),
    /// and of two alike the plugin whose id sorts first.
    /// Commutative.
    pub fn worst(self, other: PluginOutage) -> PluginOutage {
        let key = |o: &PluginOutage| (o.rank(), o.plugin_id().map(str::to_owned));
        if key(&other) < key(&self) {
            other
        } else {
            self
        }
    }
}

impl From<ClosedBy> for PluginOutage {
    /// What a plugin that closes work ([`ClosedBy`]) makes of what it
    /// provides: the outage of the same plugin, for the same reason.
    fn from(closed: ClosedBy) -> Self {
        match closed.reason {
            ClosedReason::Disabled => PluginOutage::Disabled {
                plugin_id: closed.plugin_id,
            },
            ClosedReason::NotLoaded => PluginOutage::NotLoaded {
                plugin_id: closed.plugin_id,
            },
        }
    }
}

/// What a unit of work (a submission or a code run) needs a plugin to
/// provide to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provided {
    /// Its contest type: the plugin judges the unit.
    ContestType,
    /// Its problem type: the plugin's evaluator runs the unit's test cases.
    ProblemType,
    /// Its problem's checker format: the plugin's checker checks the unit's
    /// output. Needed only when the unit's evaluator uses a checker.
    CheckerFormat,
    /// Its language: the plugin's resolver says how to compile and run it.
    Language,
    /// The language its problem's checker compiles the checker source in: a
    /// language key like [`Self::Language`], provided by the same plugins,
    /// needed when the checker format's provider declares that its checker
    /// compiles the source. Its own role, so what is waited for is said as
    /// the checker's language, not the submission's.
    CheckerLanguage,
}

/// The plugin that keeps new work of a resource from running, and why
/// (`reason`): every plugin that provides what the work needs (`provides`)
/// is disabled fleet-wide, and this is the first of them by id; or this is
/// the key's provider, enabled but loaded on no live replica. A submission it
/// closes is accepted by no submit path and, if already queued, waits
/// (parked) until a provider can run it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ClosedBy {
    #[schema(example = "my-plugin")]
    pub plugin_id: String,
    pub provides: Provided,
    pub reason: ClosedReason,
}

/// Why a plugin keeps work from running ([`ClosedBy`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClosedReason {
    /// It and every other provider of the key are disabled: the work waits
    /// until an administrator enables one.
    Disabled,
    /// It is the key's provider, enabled, and loaded on no live replica (it
    /// failed to load everywhere, or the fleet is reloading it): the work
    /// waits until a replica that loads it is live.
    NotLoaded,
}

/// A language new work may be written in that no enabled plugin provides
/// now: work in it is refused, and what is queued in it waits, until
/// `closed_by` (or another provider) is enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ClosedLanguage {
    #[schema(example = "cpp")]
    pub language: String,
    pub closed_by: ClosedBy,
}

/// Why the plugin-governed fields of a resource are left out (`null`) of a
/// response. Every field the host decides alone is still present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Withheld {
    /// The plugin that governs them hides them from this viewer, for now (a
    /// scoreboard freeze, results released later).
    Hidden,
    /// The plugin that governs them cannot decide right now.
    PluginOutage { outage: PluginOutage },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_serialize_as_tagged_objects() {
        let disabled = PluginOutage::Disabled {
            plugin_id: "ioi".into(),
        };
        assert_eq!(
            serde_json::to_value(Withheld::PluginOutage {
                outage: disabled.clone()
            })
            .unwrap(),
            serde_json::json!({"reason": "plugin_outage", "outage": {"state": "disabled", "plugin_id": "ioi"}})
        );
        assert_eq!(
            serde_json::to_value(Withheld::Hidden).unwrap(),
            serde_json::json!({"reason": "hidden"})
        );
        assert_eq!(
            serde_json::to_value(PluginOutage::Unavailable).unwrap(),
            serde_json::json!({"state": "unavailable"})
        );
    }

    #[test]
    fn a_disabled_plugin_is_the_worse_outage_from_either_side() {
        let disabled = |id: &str| PluginOutage::Disabled {
            plugin_id: id.into(),
        };
        assert_eq!(
            PluginOutage::Unavailable.worst(disabled("a")),
            disabled("a")
        );
        assert_eq!(
            disabled("a").worst(PluginOutage::Unavailable),
            disabled("a")
        );
        assert_eq!(disabled("b").worst(disabled("a")), disabled("a"));
        assert_eq!(disabled("a").worst(disabled("b")), disabled("a"));
    }

    #[test]
    fn a_plugin_loaded_nowhere_outlasts_an_unavailable_one_and_not_a_disabled_one() {
        let not_loaded = |id: &str| PluginOutage::NotLoaded {
            plugin_id: id.into(),
        };
        let disabled = PluginOutage::Disabled {
            plugin_id: "z".into(),
        };
        for (a, b, worst) in [
            (not_loaded("a"), PluginOutage::Unavailable, not_loaded("a")),
            (not_loaded("a"), disabled.clone(), disabled.clone()),
            (not_loaded("b"), not_loaded("a"), not_loaded("a")),
            (
                PluginOutage::NotLoadedHere {
                    plugin_id: "a".into(),
                },
                not_loaded("b"),
                not_loaded("b"),
            ),
            (
                PluginOutage::NotLoadedHere {
                    plugin_id: "a".into(),
                },
                PluginOutage::Unavailable,
                PluginOutage::NotLoadedHere {
                    plugin_id: "a".into(),
                },
            ),
        ] {
            assert_eq!(a.clone().worst(b.clone()), worst);
            assert_eq!(b.worst(a), worst);
        }
        assert_eq!(
            serde_json::to_value(not_loaded("ioi")).unwrap(),
            serde_json::json!({"state": "not_loaded", "plugin_id": "ioi"})
        );
        // A plugin that closes work is out for the same reason.
        assert_eq!(
            PluginOutage::from(ClosedBy {
                plugin_id: "ioi".into(),
                provides: Provided::ContestType,
                reason: ClosedReason::NotLoaded,
            }),
            not_loaded("ioi")
        );
    }
}
