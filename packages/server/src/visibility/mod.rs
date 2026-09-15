//! Per-viewer reachability kernel. Every contest-scoped read and write
//! resolves through here. See
//! `docs/superpowers/specs/2026-09-15-visibility-kernel-design.md`.

mod decision;
mod mask;
mod subject;

pub use decision::{Decision, FieldMask};
pub use mask::apply_mask;
pub use subject::{Action, Resource, Subject};
