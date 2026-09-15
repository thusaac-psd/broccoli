//! Per-viewer reachability kernel. Every contest-scoped read and write
//! resolves through here. See
//! `docs/superpowers/specs/2026-09-15-visibility-kernel-design.md`.

mod decision;
mod host_rules;
mod mask;
mod subject;

pub use decision::{Decision, FieldMask};
pub use mask::apply_mask;
// Not yet called from any handler - Task 7 wires this in. Remove the allow
// once that lands.
#[allow(unused_imports)]
pub(crate) use host_rules::host_decide;
pub use subject::{Action, Resource, Subject};
