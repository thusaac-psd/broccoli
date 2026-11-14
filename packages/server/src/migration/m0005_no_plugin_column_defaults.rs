use sea_orm_migration::prelude::*;

/// The columns that name a plugin's contest type or problem type have no
/// column default: every insert sets them, to what the request named, and a
/// default here would name one plugin's type in the host's schema.
///
/// Rolling-safe: every build that inserts these rows sets the columns.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0005_no_plugin_column_defaults"
    }
}

const COLUMNS: &[(&str, &str, &str)] = &[
    ("problem", "problem_type", "'batch'"),
    ("problem", "default_contest_type", "'ioi'"),
    ("submission", "contest_type", "'ioi'"),
    ("code_run", "contest_type", "'ioi'"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql: String = COLUMNS
            .iter()
            .map(|(table, column, _)| {
                format!("ALTER TABLE public.{table} ALTER COLUMN {column} DROP DEFAULT;\n")
            })
            .collect();
        manager.get_connection().execute_unprepared(&sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql: String = COLUMNS
            .iter()
            .map(|(table, column, value)| {
                format!(
                    "ALTER TABLE public.{table} ALTER COLUMN {column} SET DEFAULT {value}::character varying;\n"
                )
            })
            .collect();
        manager.get_connection().execute_unprepared(&sql).await?;
        Ok(())
    }
}
