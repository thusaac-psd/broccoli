use sea_orm_migration::prelude::*;

/// What `plugin.governs` holds has one shape, the one
/// `plugin_lifecycle::Governs` reads: an object with exactly the keys
/// `visibility` (a boolean), `contest_types`, `problem_types`,
/// `checker_formats`, `languages` and `unchecked_problem_types` (arrays). The SQL that reads the column and the Rust
/// that deserializes it therefore never read a row differently: a row
/// missing a key is refused when it is written instead of being read as "no
/// such provider" by one side and dropped by the other.
///
/// A row an earlier build recorded is brought to that shape first, keeping
/// what it says: the released build records `{visibility, contest_types}`, so
/// such a row keeps both and gains empty `problem_types` and
/// `checker_formats`, which the SQL reads as "no plugin recorded the key",
/// as it read their absence. A row in no shape either reader knows is
/// cleared; the replica that loads the plugin records it again. A disabled
/// plugin keeps the contest types it governed, so their work stays parked.
///
/// Not additive (release/docs/upgrade.md): it is a constraint the rows
/// servers of the previous release write do not meet, so they stop before it
/// runs.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0017_plugin_governs_shape"
    }
}

const UP: &str = r#"
UPDATE public.plugin SET governs = CASE
    WHEN jsonb_typeof(governs) = 'object'
        AND jsonb_typeof(governs -> 'visibility') = 'boolean'
        AND jsonb_typeof(governs -> 'contest_types') = 'array'
    THEN jsonb_build_object(
        'visibility', governs -> 'visibility',
        'contest_types', governs -> 'contest_types',
        'problem_types', CASE WHEN jsonb_typeof(governs -> 'problem_types') = 'array'
            THEN governs -> 'problem_types' ELSE '[]'::jsonb END,
        'checker_formats', CASE WHEN jsonb_typeof(governs -> 'checker_formats') = 'array'
            THEN governs -> 'checker_formats' ELSE '[]'::jsonb END)
    END
WHERE governs IS NOT NULL;
ALTER TABLE public.plugin ADD CONSTRAINT plugin_governs_shape CHECK (
    governs IS NULL OR (
        jsonb_typeof(governs) = 'object'
        AND governs ?& ARRAY['visibility', 'contest_types', 'problem_types', 'checker_formats',
                             'languages', 'unchecked_problem_types']
        AND governs - ARRAY['visibility', 'contest_types', 'problem_types', 'checker_formats',
                            'languages', 'unchecked_problem_types'] = '{}'::jsonb
        AND jsonb_typeof(governs -> 'visibility') = 'boolean'
        AND jsonb_typeof(governs -> 'contest_types') = 'array'
        AND jsonb_typeof(governs -> 'problem_types') = 'array'
        AND jsonb_typeof(governs -> 'checker_formats') = 'array'
        AND jsonb_typeof(governs -> 'languages') = 'array'
        AND jsonb_typeof(governs -> 'unchecked_problem_types') = 'array'
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
