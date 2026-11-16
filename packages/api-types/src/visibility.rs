//! Markers for what a response leaves out because a plugin decides it.

use serde::{Deserialize, Serialize};

/// Why a plugin cannot serve what it provides right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PluginOutage {
    /// An administrator disabled the plugin. Nothing changes until it is
    /// enabled again.
    Disabled {
        #[schema(example = "ioi")]
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
