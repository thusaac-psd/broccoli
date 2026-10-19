use broccoli_server_sdk::types::PublishNotification;
use extism::host_fn;
use sea_orm::DatabaseConnection;

host_fn!(pub notification_publish(user_data: (String, DatabaseConnection); input: String) -> String {
    let guard = user_data.get()?;
    let data = super::lock_or_poison(&guard)?;
    let (plugin_id, db) = &*data;
    let span = super::host_fn_span("notification_publish", plugin_id);
    let _enter = span.enter();
    let message: PublishNotification = serde_json::from_str(&input)?;
    tokio::runtime::Handle::current()
        .block_on(crate::services::notifications::publish(db, plugin_id, &message))?;
    Ok("{}".to_string())
});
