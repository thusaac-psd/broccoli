use std::sync::Arc;

use extism::host_fn;

host_fn!(pub log_info(user_data: Arc<String>; msg: String) -> () {
    let plugin_id = super::HostCall::enter(&user_data, "log_info")?;
    plugin_core::guest_log::emit(&plugin_id, tracing::Level::INFO, &msg);
    Ok(())
});
