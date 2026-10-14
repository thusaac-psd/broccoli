//! The admission capability works with opaque data and independent plugin policies.
use crate::common::TestApp;
use broccoli_server_sdk::types::*;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::json;
use server::entity::{plugin_storage, submission, user};
use server::services::submission_feedback;

#[tokio::test]
async fn opaque_admission_is_atomic_plugin_scoped_and_invalidates_decision_snapshots() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("admission_admin", "pass1234", "admin")
        .await;
    let user_id = user::Entity::find()
        .filter(user::Column::Username.eq("admission_admin"))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id;
    let problem_id = app.create_problem(&token, "Opaque admission").await;
    let contest_id = app
        .create_contest(&token, "Opaque admission", true, false)
        .await;
    let owner = "opaque-format";
    let key = "arbitrary-document";
    let original = "opaque bytes, not a JSON object";
    plugin_storage::ActiveModel {
        plugin_id: Set(owner.into()),
        collection: Set("default".into()),
        key: Set(key.into()),
        data: Set(json!(original)),
        revision: Set(0),
        created_at: Set(Utc::now()),
    }
    .insert(&app.db)
    .await
    .unwrap();
    let admission = SubmissionAdmission {
        guard: SubmissionStorageGuard {
            key: key.into(),
            expected_value: original.into(),
        },
        feedback_key: "attempt-group".into(),
        metadata: json!({"format_owned": [1, "anything"]}),
        slot: Some(SubmissionSlot {
            key: "custom-slot".into(),
            generation: None,
        }),
        latest_arrival_ms: None,
    };
    let new = submission::ActiveModel {
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("standard".into()),
        status: Set(common::SubmissionStatus::Queued),
        files: Set(json!([])),
        language: Set("cpp".into()),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    let requests =
        futures::future::join_all((0..8).map(|_| {
            submission_feedback::insert_admitted(&app.state, new.clone(), &admission, owner)
        }))
        .await;
    assert_eq!(requests.iter().filter(|r| r.is_ok()).count(), 1);
    let admitted = requests.into_iter().find_map(Result::ok).unwrap();
    let stored = plugin_storage::Entity::find_by_id((owner.into(), "default".into(), key.into()))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.data, json!(original));
    assert_eq!(stored.revision, 1);
    let record = submission_feedback::query(&app.db, owner, &SubmissionFeedbackQuery::default())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(record.metadata, admission.metadata);
    assert!(record.withheld && record.slot_held);
    assert!(
        submission_feedback::query(&app.db, "other-owner", &SubmissionFeedbackQuery::default())
            .await
            .unwrap()
            .is_empty()
    );
    let public = SubmissionFeedbackPublication {
        withheld: false,
        release_slot: true,
        result: Some(SubmissionFeedbackResult {
            verdict: Verdict::Other("CustomOutcome".into()),
            score: 17.0,
        }),
    };
    submission_feedback::publish(&app.db, "other-owner", admitted.id, &public)
        .await
        .unwrap();
    assert!(
        submission_feedback::query(&app.db, owner, &SubmissionFeedbackQuery::default())
            .await
            .unwrap()[0]
            .withheld
    );
    submission_feedback::publish(&app.db, owner, admitted.id, &public)
        .await
        .unwrap();
    let feedback = submission_feedback::load(&app.state, &[admitted.id])
        .await
        .unwrap()
        .remove(&admitted.id)
        .unwrap();
    let mut value = json!({"status":"Judged","result":{"verdict":"Accepted","score":1}});
    submission_feedback::project(&mut value, feedback);
    assert_eq!(value["result"]["verdict"], "CustomOutcome");
    assert_eq!(value["result"]["score"], 17.0);
    assert!(
        submission_feedback::insert_admitted(&app.state, new.clone(), &admission, owner)
            .await
            .is_err(),
        "released slots must still reject stale admission hooks"
    );
    let mut next = admission.clone();
    next.slot.as_mut().unwrap().generation = Some(admitted.id);
    submission_feedback::insert_admitted(&app.state, new.clone(), &next, owner)
        .await
        .unwrap();
    // The same contestant may hold another slot when their format requests it.
    next.slot.as_mut().unwrap().key = "independent-slot".into();
    next.slot.as_mut().unwrap().generation = None;
    submission_feedback::insert_admitted(&app.state, new.clone(), &next, owner)
        .await
        .unwrap();
    // Two callers can share a guard when their slot namespaces differ.
    let mut opponent = next.clone();
    opponent.slot.as_mut().unwrap().key = "opponent-slot".into();
    submission_feedback::insert_admitted(&app.state, new.clone(), &opponent, owner)
        .await
        .unwrap();
    let mut expired = next.clone();
    expired.slot = None;
    expired.latest_arrival_ms = Some(0);
    assert!(
        submission_feedback::insert_admitted(&app.state, new, &expired, owner)
            .await
            .is_err()
    );
    let stored = plugin_storage::Entity::find_by_id((owner.into(), "default".into(), key.into()))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.revision, 4,
        "expired admission must roll back its revision bump"
    );
    let data = json!({"long_key": 1, "a": [2, 3]});
    let mut object: plugin_storage::ActiveModel = stored.into();
    object.data = Set(data.clone());
    object.update(&app.db).await.unwrap();
    let cas = server::services::plugin_storage::compare_and_set;
    assert!(
        !cas(
            &app.db,
            owner,
            key,
            Some("arbitrary opaque bytes"),
            "new",
            Some(4)
        )
        .await
        .unwrap(),
        "non-JSON expectations must return a mismatch, not a database error"
    );
    assert!(
        !cas(&app.db, owner, key, Some(&data.to_string()), "new", Some(3))
            .await
            .unwrap(),
        "stale revisions must not replace the document"
    );
    assert!(
        cas(
            &app.db,
            owner,
            key,
            Some(&data.to_string()),
            "opaque replacement",
            Some(4)
        )
        .await
        .unwrap()
    );
    assert!(
        !cas(
            &app.db,
            "other-owner",
            key,
            Some("opaque replacement"),
            "new",
            Some(5)
        )
        .await
        .unwrap()
    );
    // Exercise an upgrade of existing host feedback records, rather than only
    // creating an empty feedback table on a fresh installation.
    use sea_orm::ConnectionTrait;
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    app.db
        .execute_unprepared(&format!(
            "DELETE FROM submission_feedback WHERE submission_id <> {}",
            admitted.id
        ))
        .await
        .unwrap();
    app.db.execute_unprepared("ALTER TABLE submission_feedback ADD COLUMN scope_key TEXT NOT NULL DEFAULT 'opaque-scope';
        ALTER TABLE submission_feedback ADD COLUMN delay_seconds BIGINT NOT NULL DEFAULT 11;
        ALTER TABLE submission_feedback ADD COLUMN effective_at_us BIGINT NOT NULL DEFAULT 1234;
        ALTER TABLE submission_feedback DROP COLUMN metadata;
        ALTER TABLE submission_feedback DROP COLUMN slot_key;
        ALTER TABLE submission_feedback DROP COLUMN public_result;
        ALTER TABLE submission_feedback DROP CONSTRAINT submission_feedback_outcome_check;
        ALTER TABLE submission_feedback ADD CONSTRAINT submission_feedback_outcome_check CHECK (outcome IN ('pending','retired','released','too_late'));
        UPDATE submission_feedback SET outcome='too_late'").await.unwrap();
    let migrations = server::migration::Migrator::migrations();
    migrations
        .last()
        .unwrap()
        .up(&SchemaManager::new(&app.db))
        .await
        .unwrap();
    let upgraded = submission_feedback::query(&app.db, owner, &SubmissionFeedbackQuery::default())
        .await
        .unwrap()
        .remove(0);
    assert!(upgraded.withheld && !upgraded.slot_held);
    assert_eq!(upgraded.metadata["scope_key"], "opaque-scope");
    assert_eq!(upgraded.metadata["delay_seconds"], 11);
    assert_eq!(upgraded.metadata["effective_at_us"], 1234);
    assert_eq!(upgraded.metadata["previous_outcome"], "too_late");
}
