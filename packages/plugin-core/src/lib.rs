// `#[async_trait]` stamps its own `#[must_use]` on each rewritten async method
// even though the boxed future it returns is already `#[must_use]`, so clippy's
// `double_must_use` fires on macro output we don't control. Both hits are the
// `PluginInvoker` / `PluginInvokerExt` traits in `traits.rs`; suppress crate-wide.
#![allow(clippy::double_must_use)]

pub mod call_gate;
pub mod config;
pub mod error;
pub mod hook;
pub mod host;
pub mod host_context;
pub mod http;
pub mod i18n;
pub mod manager;
pub mod manifest;
pub mod pool;
pub mod registry;
pub mod retry;
pub mod traits;
