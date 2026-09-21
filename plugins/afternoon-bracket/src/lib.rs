//! 下午场 (afternoon session) bracket plugin.
//!
//! Implements the 16-player single-elimination bracket format described in
//! `docs/superpowers/specs/2026-09-19-afternoon-bracket-design.md`. All
//! bracket state, ordering, visibility and judging logic lives in this
//! plugin; the host knows nothing about brackets, rounds, matches or 小局.

#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};

#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::prelude::*;

// -- Plugin entry points -------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn init() -> FnResult<String> {
    let host = Host::new();
    host.registry.register_contest_type(
        "afternoon-bracket",
        "handle_afternoon_bracket_submission",
        "handle_afternoon_bracket_code_run",
    )?;
    host.log
        .info("afternoon-bracket contest plugin registered")?;
    Ok("ok".into())
}
