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
    /// The plugin is enabled but cannot answer on this server right now (it
    /// is loading, reloading or busy). A retry may succeed.
    Unavailable,
}

impl PluginOutage {
    /// The outage of two plugins at once: a disabled plugin outlasts an
    /// unavailable one, so its outage is the one to report. Commutative.
    pub fn worst(self, other: PluginOutage) -> PluginOutage {
        match (self, other) {
            (PluginOutage::Disabled { plugin_id: a }, PluginOutage::Disabled { plugin_id: b }) => {
                PluginOutage::Disabled {
                    plugin_id: a.min(b),
                }
            }
            (d @ PluginOutage::Disabled { .. }, _) | (_, d @ PluginOutage::Disabled { .. }) => d,
            (PluginOutage::Unavailable, PluginOutage::Unavailable) => PluginOutage::Unavailable,
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
}
