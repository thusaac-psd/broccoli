//! Claim wake-ups: Postgres notifications that tell claim fibers when there
//! is work to claim or room to claim it.
//!
//! A trigger on each dispatch-unit table sends them from the transaction that
//! makes the change, so every path is covered by construction (submission,
//! rejudge, DLQ retry, requeue, system-error retry, code run) and the submit
//! path pays no extra statement. A notification is delivered after its
//! transaction commits, so the woken fiber sees the change.
//!
//! On [`CLAIM_CHANNEL`]:
//!
//! * [`QUEUED`] when a unit becomes `Queued` (inserted or requeued). Postgres
//!   folds identical notifications of one transaction into one, so a bulk
//!   rejudge or requeue sends it once.
//! * `freed:<server id>` when a unit owned by that server leaves flight
//!   (finalized or requeued), so a fiber held back by its in-flight cap claims
//!   again as soon as room opens.

/// The channel claim fibers `LISTEN` on.
pub const CLAIM_CHANNEL: &str = "broccoli_claim";

/// Payload of the notification that queued work exists.
pub const QUEUED: &str = "queued";

const FREED_PREFIX: &str = "freed:";

/// What a claim notification says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimWake<'a> {
    /// Some unit became `Queued`.
    Queued,
    /// A unit owned by this server left flight.
    Freed { server_id: &'a str },
}

impl<'a> ClaimWake<'a> {
    /// Parses a payload on [`CLAIM_CHANNEL`]; `None` for one it does not know.
    pub fn parse(payload: &'a str) -> Option<Self> {
        if payload == QUEUED {
            return Some(Self::Queued);
        }
        payload
            .strip_prefix(FREED_PREFIX)
            .map(|server_id| Self::Freed { server_id })
    }
}

/// The trigger definitions the listener relies on, as code. The migrations
/// create them in every database; the schema test
/// `the_migrations_create_the_schema_the_code_expects` runs [`install`] against
/// a migrated database and fails if it changes anything, so a change here
/// ships with a migration that makes the same change.
#[cfg(test)]
pub(crate) mod expected {
    use common::StatusPhase;
    use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

    use super::super::unit::Unit;
    use super::{CLAIM_CHANNEL, FREED_PREFIX, QUEUED};

    const FUNCTION: &str = "broccoli_notify_claim";

    /// The trigger function. All the behaviour lives here, so replacing it
    /// changes what the triggers do without touching the tables.
    fn function_ddl() -> String {
        format!(
            "CREATE OR REPLACE FUNCTION {FUNCTION}() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
               IF NEW.status IN {queued} \
                  AND (TG_OP = 'INSERT' OR OLD.status NOT IN {queued}) THEN \
                 PERFORM pg_notify('{CLAIM_CHANNEL}', '{QUEUED}'); \
               END IF; \
               IF TG_OP = 'UPDATE' AND OLD.owner_server_id IS NOT NULL \
                  AND OLD.status IN {in_flight} AND NEW.status NOT IN {in_flight} THEN \
                 PERFORM pg_notify('{CLAIM_CHANNEL}', '{FREED_PREFIX}' || OLD.owner_server_id); \
               END IF; \
               RETURN NULL; \
             END $$",
            queued = StatusPhase::Queued.sql_list(),
            in_flight = StatusPhase::InFlight.sql_list(),
        )
    }

    /// Replaces the trigger function and creates each table's trigger if it
    /// is missing.
    pub async fn install<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
        db.execute_unprepared(&function_ddl()).await?;
        for unit in [Unit::Judgement, Unit::CodeRun] {
            let exists = db
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 FROM pg_trigger WHERE tgname = $1 AND tgrelid = $2::regclass",
                    [FUNCTION.into(), unit.table().into()],
                ))
                .await?
                .is_some();
            if !exists {
                db.execute_unprepared(&format!(
                    "CREATE TRIGGER {FUNCTION} AFTER INSERT OR UPDATE OF status ON {table} \
                     FOR EACH ROW EXECUTE FUNCTION {FUNCTION}()",
                    table = unit.table(),
                ))
                .await?;
            }
        }
        Ok(())
    }
}
