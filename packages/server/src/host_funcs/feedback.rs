use super::sql::HostDbResponse;
use broccoli_server_sdk::types::{SubmissionFeedbackPublication, SubmissionFeedbackQuery};
use extism::host_fn;
use sea_orm::DatabaseConnection;
use serde::Deserialize;

host_fn!(pub submission_feedback_query(user_data: (String, DatabaseConnection); input: String) -> String {
    let (plugin_id, db) = {
        let guard = user_data.get()?;
        let data = super::lock_or_poison(&guard)?;
        data.clone()
    };
    let query: SubmissionFeedbackQuery = serde_json::from_str(&input)?;
    let records = tokio::runtime::Handle::current().block_on(
        crate::services::submission_feedback::query(&db, &plugin_id, &query));
    match records {
        Ok(records) => HostDbResponse::ok(serde_json::to_value(records)?).to_json_string(),
        Err(error) => HostDbResponse::err(error.to_string()).to_json_string(),
    }
});

#[derive(Deserialize)]
struct PublishInput {
    submission_id: i32,
    publication: SubmissionFeedbackPublication,
}

host_fn!(pub submission_feedback_publish(user_data: (String, DatabaseConnection); input: String) -> String {
    let (plugin_id, db) = {
        let guard = user_data.get()?;
        let data = super::lock_or_poison(&guard)?;
        data.clone()
    };
    let input: PublishInput = serde_json::from_str(&input)?;
    let result = tokio::runtime::Handle::current().block_on(crate::services::submission_feedback::publish(
        &db, &plugin_id, input.submission_id, &input.publication));
    match result {
        Ok(()) => HostDbResponse::ok(serde_json::json!(null)).to_json_string(),
        Err(error) => HostDbResponse::err(error.to_string()).to_json_string(),
    }
});
