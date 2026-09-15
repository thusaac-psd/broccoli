mod attachment;
mod auth;
mod clarification;
mod code_run;
mod common;
mod contest;
mod dlq;
#[cfg(feature = "bundled-stress-test")]
mod downloads;
#[cfg(not(feature = "bundled-stress-test"))]
mod downloads_slim;
mod health;
mod meta;
mod plugin;
mod plugin_config;
mod problem;
mod regression_guards;
mod scaling;
mod submission;
mod user;
mod visibility_matrix;
mod visibility_plugin;
