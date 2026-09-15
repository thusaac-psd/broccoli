//! Per-viewer reachability kernel. Every contest-scoped read and write
//! resolves through here. See
//! `docs/superpowers/specs/2026-09-15-visibility-kernel-design.md`.

mod decision;
mod host_rules;
mod mask;
mod plugin_query;
mod subject;

pub use decision::{Decision, FieldMask};
pub use mask::apply_mask;
// Not yet called from any handler - Task 7 wires this in. Remove the allow
// once that lands.
#[allow(unused_imports)]
pub(crate) use host_rules::host_decide;
// Not yet called from any handler - Task 7 wires this in alongside
// `host_decide`. Remove the allow once that lands.
#[allow(unused_imports)]
pub(crate) use plugin_query::query_plugins;
pub use subject::{Action, Resource, Subject};
