use extism_pdk::{FnResult, host_fn, plugin_fn};

// Imports `timer_schedule` without the manifest declaring the `"timer"`
// permission. The host only links this host function into a plugin's WASM
// instance when the permission is present (`host_funcs/mod.rs`'s
// `hr.register("timer", ...)`), so this module's import can never resolve -
// instantiation itself fails, before any route handler runs. This is the
// fixture for `a_plugin_without_the_timer_permission_cannot_schedule`: the
// gate is the host-function registration, not a runtime permission check
// inside `timer_schedule` that a future change could accidentally drop.
#[host_fn]
extern "ExtismHost" {
    fn timer_schedule(input: String) -> String;
}

#[plugin_fn]
pub fn schedule(_input: String) -> FnResult<String> {
    let res = unsafe { timer_schedule("{}".to_string()) }?;
    Ok(res)
}
