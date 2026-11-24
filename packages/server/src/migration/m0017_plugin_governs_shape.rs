use sea_orm_migration::prelude::*;

/// What `plugin.governs` holds has one shape, the one
/// `plugin_lifecycle::Governs` reads: an object with exactly the keys
/// `visibility` (a boolean), `contest_types`, `problem_types` and `checkers`
/// (arrays). The SQL that reads the column and the Rust that deserializes it
/// therefore never read a row differently: a row missing a key is refused
/// when it is written instead of being read as "no such provider" by one
/// side and dropped by the other.
///
/// Forward-only like every migration here.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0017_plugin_governs_shape"
    }
}

const UP: &str = r#"
ALTER TABLE public.plugin ADD CONSTRAINT plugin_governs_shape CHECK (
    governs IS NULL OR (
        jsonb_typeof(governs) = 'object'
        AND governs ?& ARRAY['visibility', 'contest_types', 'problem_types', 'checkers']
        AND governs - ARRAY['visibility', 'contest_types', 'problem_types', 'checkers'] = '{}'::jsonb
        AND jsonb_typeof(governs -> 'visibility') = 'boolean'
        AND jsonb_typeof(governs -> 'contest_types') = 'array'
        AND jsonb_typeof(governs -> 'problem_types') = 'array'
        AND jsonb_typeof(governs -> 'checkers') = 'array'
    )
);
"#;

const DOWN: &str = r#"
ALTER TABLE public.plugin DROP CONSTRAINT plugin_governs_shape;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
