use std::sync::Arc;

use extism::host_fn;

host_fn!(pub log_info(user_data: Arc<String>; msg: String) -> () {
    let plugin_id = super::HostCall::enter(&user_data, "log_info")?;
    tracing::info!(plugin = %*plugin_id, "{}", msg);
    Ok(())
});
