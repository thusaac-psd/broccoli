// Tests are clients: they read the wall clock the way a contestant's machine
// does. The server itself keeps time by the platform clock.
#![allow(clippy::disallowed_methods)]

mod additional_file;
mod attachment;
mod auth;
mod bootstrap_admin;
mod claim_in_flight;
mod claim_wake;
mod clarification;
mod code_run;
mod codelink_bracket;
mod codelink_bracket_gating;
mod codelink_bracket_timing;
mod common;
mod contest;
mod device_flow;
mod display_name;
mod dlq;
#[cfg(feature = "bundled-stress-test")]
mod downloads;
#[cfg(not(feature = "bundled-stress-test"))]
mod downloads_slim;
mod health;
mod hook_enablement;
mod idempotency;
mod meta;
mod nul_text;
mod permission_fixture;
mod platform_clock;
mod plugin;
mod plugin_config;
mod plugin_config_cache;
mod plugin_fleet;
mod plugin_flood;
mod plugin_timer;
mod plugin_upload;
mod privilege_ceiling;
mod problem;
mod rejudge_visibility;
mod scaling;
mod shared_problem_contests;
mod source_invariants;
mod submission;
mod submission_admission;
mod system;
mod system_admin_guard;
mod user;
mod visibility_clarification_contents;
mod visibility_matrix;
mod visibility_plugin;
mod visibility_submission_count_matrix;
