//! Schema bootstrap run by `init` on every replica, so the plugin needs no
//! separate migration step.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;

const STATEMENTS: &[&str] = &[
    r#"
    CREATE TABLE IF NOT EXISTS print_job (
        id              BIGSERIAL PRIMARY KEY,
        contest_id      INTEGER,
        user_id         INTEGER NOT NULL,
        username        TEXT    NOT NULL,
        display_name    TEXT,
        problem_label   TEXT,
        submission_id   INTEGER,
        language        TEXT    NOT NULL DEFAULT 'text',
        filename        TEXT    NOT NULL,
        source          TEXT    NOT NULL,
        pages_est       INTEGER,
        location        TEXT,
        target_printer  TEXT,
        status          TEXT    NOT NULL DEFAULT 'pending',
        claimed_by      TEXT,
        claimed_printer TEXT,
        pages           INTEGER,
        error           TEXT,
        created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        claimed_at      TIMESTAMPTZ,
        printed_at      TIMESTAMPTZ
    )
    "#,
    "CREATE INDEX IF NOT EXISTS print_job_claimable_idx ON print_job (status, contest_id, created_at)",
    "CREATE INDEX IF NOT EXISTS print_job_user_idx ON print_job (user_id, created_at)",
    r#"
    CREATE TABLE IF NOT EXISTS print_station (
        name        TEXT PRIMARY KEY,
        location    TEXT,
        printers    JSONB NOT NULL DEFAULT '[]'::jsonb,
        version     TEXT,
        queue_seen  INTEGER,
        last_seen   TIMESTAMPTZ NOT NULL DEFAULT NOW()
    )
    "#,
];

pub fn create_tables(host: &Host) -> Result<(), SdkError> {
    host.db.ensure_schema("print", STATEMENTS)
}
