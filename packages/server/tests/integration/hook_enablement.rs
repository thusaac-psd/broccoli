//! Which plugins' resource-scoped hooks run for a submission
//! (`hooks::resource_enablements`): config-enabled plugins everywhere, plus
//! the plugin that owns the contest's type for contest submissions only.

use crate::common::TestApp;
use sea_orm::{ActiveModelTrait, Set};
use server::entity::{contest, plugin_config};
use server::hooks::resource_enablements;
use server::registry::{ContestTypeHandlers, ContestTypeRegistry};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

const PROBLEM: i32 = 41;

fn registry_owning(contest_type: &str, plugin_id: &str) -> ContestTypeRegistry {
    let mut map = HashMap::new();
    map.insert(
        contest_type.to_string(),
        ContestTypeHandlers {
            plugin_id: plugin_id.to_string(),
            submission_fn: "handle_submission".into(),
            code_run_fn: "handle_code_run".into(),
            submission_admission: false,
        },
    );
    Arc::new(RwLock::new(map))
}

async fn contest_of_type(app: &TestApp, contest_type: Option<&str>) -> i32 {
    let now = chrono::Utc::now();
    contest::ActiveModel {
        title: Set("Hook enablement".into()),
        description: Set(String::new()),
        start_time: Set(now),
        end_time: Set(now + chrono::Duration::hours(1)),
        contest_type: Set(contest_type.map(str::to_string)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert contest")
    .id
}

async fn config_row(app: &TestApp, contest_id: i32, plugin: &str, enabled: bool) {
    plugin_config::ActiveModel {
        scope: Set("contest".to_string()),
        ref_id: Set(contest_id.to_string()),
        namespace: Set(format!("{plugin}:before_submission")),
        config: Set(serde_json::json!({})),
        enabled: Set(Some(enabled)),
        position: Set(3),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert plugin_config row");
}

#[tokio::test]
async fn the_contest_type_owner_runs_for_its_own_contests_only() {
    let app = TestApp::spawn().await;
    let types = registry_owning("fmt-a", "format-a");
    let own = contest_of_type(&app, Some("fmt-a")).await;
    let other = contest_of_type(&app, Some("fmt-b")).await;

    let enabled = resource_enablements(&app.db, &types, PROBLEM, Some(own))
        .await
        .unwrap();
    assert!(
        enabled.contains_key("format-a"),
        "owner must run with no config: {enabled:?}"
    );

    let enabled = resource_enablements(&app.db, &types, PROBLEM, Some(other))
        .await
        .unwrap();
    assert!(
        !enabled.contains_key("format-a"),
        "not another format's contest: {enabled:?}"
    );

    let enabled = resource_enablements(&app.db, &types, PROBLEM, None)
        .await
        .unwrap();
    assert!(
        !enabled.contains_key("format-a"),
        "not practice: {enabled:?}"
    );
}

/// A contest with no type is judged under a fallback type (the
/// alphabetically first registered one), which is not a choice of format.
/// Its owner's rules must not be imposed: a format gate would otherwise
/// reject every submission to the contest.
#[tokio::test]
async fn a_contest_without_a_type_gets_no_owner() {
    let app = TestApp::spawn().await;
    let types = registry_owning("fmt-a", "format-a");
    let untyped = contest_of_type(&app, None).await;

    let enabled = resource_enablements(&app.db, &types, PROBLEM, Some(untyped))
        .await
        .unwrap();
    assert!(enabled.is_empty(), "{enabled:?}");
}

#[tokio::test]
async fn opt_in_plugins_still_need_config_and_the_owner_cannot_be_switched_off() {
    let app = TestApp::spawn().await;
    let types = registry_owning("fmt-a", "format-a");
    let configured = contest_of_type(&app, Some("fmt-a")).await;
    let unconfigured = contest_of_type(&app, Some("fmt-a")).await;
    config_row(&app, configured, "cooldown", true).await;
    config_row(&app, configured, "format-a", false).await;

    let enabled = resource_enablements(&app.db, &types, PROBLEM, Some(configured))
        .await
        .unwrap();
    assert_eq!(
        enabled.get("cooldown"),
        Some(&3),
        "opt-in plugin keeps its config"
    );
    assert!(
        enabled.contains_key("format-a"),
        "a format's own rules are not optional for its contests: {enabled:?}"
    );

    let enabled = resource_enablements(&app.db, &types, PROBLEM, Some(unconfigured))
        .await
        .unwrap();
    assert!(!enabled.contains_key("cooldown"), "{enabled:?}");
}
