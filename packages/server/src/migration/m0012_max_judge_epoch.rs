use sea_orm_migration::prelude::*;

/// Adds `submission.max_judge_epoch`, the per-submission judge-epoch
/// allocator (see `services::judgement_lifecycle`), and backfills it where it
/// matters.
///
/// The allocator takes `GREATEST(max_judge_epoch, judge_epoch) + 1`, so a row
/// whose judgements all sit at or below its own `judge_epoch` allocates
/// correctly with the column's default of 0. Only a submission holding a
/// judgement ABOVE both values (a deferred rejudge) needs the backfill; one
/// aggregate pass over `submission_judgement` finds exactly those and writes
/// nothing else, instead of rewriting every judged submission at boot.
///
/// Idempotent: on a fresh DB the entity `sync()` has already created the
/// column, so `ADD COLUMN IF NOT EXISTS` is a no-op, and the backfill only
/// ever raises the value.
#[derive(DeriveMigrationName)]
pub struct Migration;

const ADD_COLUMN: &str = r#"ALTER TABLE IF EXISTS submission
    ADD COLUMN IF NOT EXISTS max_judge_epoch INTEGER NOT NULL DEFAULT 0"#;

pub(super) const BACKFILL: &str = r#"UPDATE submission s
    SET max_judge_epoch = j.max_epoch
    FROM (
        SELECT submission_id, MAX(judge_epoch) AS max_epoch
        FROM submission_judgement
        GROUP BY submission_id
    ) j
    WHERE j.submission_id = s.id
      AND j.max_epoch > GREATEST(s.max_judge_epoch, s.judge_epoch)"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(ADD_COLUMN).await?;
        db.execute_unprepared(BACKFILL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE IF EXISTS submission DROP COLUMN IF EXISTS max_judge_epoch"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::BACKFILL;
    use crate::entity::{submission, submission_judgement};
    use common::SubmissionStatus;
    use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set};

    /// The backfill writes only the submission whose deferred judgement sits
    /// above its own epoch; a plainly judged submission is left alone.
    #[tokio::test]
    async fn backfill_touches_only_submissions_with_a_higher_judgement_epoch() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (user_id, problem_id) = crate::test_support::seed_user_and_problem(&db).await;
        let now = chrono::Utc::now();
        let mut ids = Vec::new();
        for (sub_epoch, judgement_epochs) in [(5, vec![5]), (3, vec![3, 7])] {
            let sub = submission::ActiveModel {
                files: Set(serde_json::json!({})),
                language: Set("cpp".into()),
                user_id: Set(user_id),
                problem_id: Set(problem_id),
                status: Set(SubmissionStatus::Judged),
                judge_epoch: Set(sub_epoch),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(&db)
            .await
            .expect("submission");
            for (i, epoch) in judgement_epochs.into_iter().enumerate() {
                submission_judgement::ActiveModel {
                    submission_id: Set(sub.id),
                    version: Set(i as i32 + 1),
                    is_current: Set(i == 0),
                    is_finalized: Set(true),
                    status: Set(SubmissionStatus::Judged),
                    judge_epoch: Set(epoch),
                    created_at: Set(now),
                    ..Default::default()
                }
                .insert(&db)
                .await
                .expect("judgement");
            }
            ids.push(sub.id);
        }

        let res = db.execute_unprepared(BACKFILL).await.expect("backfill");
        assert_eq!(res.rows_affected(), 1, "only the deferred one needs it");
        let plain = submission::Entity::find_by_id(ids[0])
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        let deferred = submission::Entity::find_by_id(ids[1])
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(plain.max_judge_epoch, 0);
        assert_eq!(deferred.max_judge_epoch, 7);
        let again = db.execute_unprepared(BACKFILL).await.expect("backfill");
        assert_eq!(again.rows_affected(), 0, "idempotent");
    }
}
