use super::sql::HostDbResponse;
use broccoli_server_sdk::types::{SubmissionFeedbackAction, SubmissionFeedbackQuery};
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
struct UpdateInput {
    submission_id: i32,
    action: SubmissionFeedbackAction,
}

host_fn!(pub submission_feedback_update(user_data: (String, DatabaseConnection); input: String) -> String {
    let (plugin_id, db) = {
        let guard = user_data.get()?;
        let data = super::lock_or_poison(&guard)?;
        data.clone()
    };
    let input: UpdateInput = serde_json::from_str(&input)?;
    let result = tokio::runtime::Handle::current().block_on(crate::services::submission_feedback::update(
        &db, &plugin_id, input.submission_id, &input.action));
    match result {
        Ok(outcome) => HostDbResponse::ok(serde_json::to_value(outcome)?).to_json_string(),
        Err(error) => HostDbResponse::err(error.to_string()).to_json_string(),
    }
});

host_fn!(pub submission_slot_observe(user_data: (String, DatabaseConnection); input: String) -> String {
    let (plugin_id, db) = {
        let guard = user_data.get()?;
        let data = super::lock_or_poison(&guard)?;
        data.clone()
    };
    let key: String = serde_json::from_str(&input)?;
    let observation = tokio::runtime::Handle::current().block_on(
        crate::services::submission_feedback::observe_slot(&db, &plugin_id, &key));
    match observation {
        Ok(observation) => HostDbResponse::ok(serde_json::to_value(observation)?).to_json_string(),
        Err(error) => HostDbResponse::err(error.to_string()).to_json_string(),
    }
});
