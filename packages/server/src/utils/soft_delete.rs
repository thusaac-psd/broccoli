use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, PrimaryKeyTrait, QueryFilter, QuerySelect,
    QueryTrait, Select,
};

use crate::error::AppError;

pub trait SoftDeletable: EntityTrait {
    type DeletedAtColumn: ColumnTrait;

    fn deleted_at() -> Self::DeletedAtColumn;

    fn find_active() -> Select<Self> {
        Self::find().filter(Self::deleted_at().is_null())
    }

    fn find_active_by_id<V>(values: V) -> Select<Self>
    where
        V: Into<<Self::PrimaryKey as PrimaryKeyTrait>::ValueType>,
    {
        Self::find_by_id(values).filter(Self::deleted_at().is_null())
    }

    /// `EXISTS (SELECT 1 FROM <self> WHERE id = .. AND deleted_at IS NULL)`.
    ///
    /// Filter for scoping a child lookup to a live parent inside the child's
    /// own statement, so the gate costs no extra round trip.
    fn active_exists<V>(values: V) -> Expr
    where
        V: Into<<Self::PrimaryKey as PrimaryKeyTrait>::ValueType>,
    {
        Expr::exists(
            Self::find_active_by_id(values)
                .select_only()
                .expr(Expr::val(1))
                .into_query(),
        )
    }
}

/// Find child row `id` of `E` that belongs to `parent_id` of the soft-deletable
/// parent `P` (through `parent_column`), and only while that parent is active.
///
/// Child resources (test cases, attachments, additional files,
/// clarifications) have no `deleted_at` of their own: they disappear with
/// their parent. Looking them up through this helper makes every caller
/// inherit the parent's soft delete, instead of each handler having to
/// remember a separate parent check (the gap that let a soft-deleted
/// problem's test data stay readable). A row of another parent and a row
/// whose parent is deleted both answer `404 "<what> not found"`.
pub async fn find_active_child<P, E, V, C>(
    db: &C,
    parent_column: E::Column,
    parent_id: i32,
    id: V,
    what: &str,
) -> Result<E::Model, AppError>
where
    P: SoftDeletable,
    i32: Into<<P::PrimaryKey as PrimaryKeyTrait>::ValueType>,
    E: EntityTrait,
    V: Into<<E::PrimaryKey as PrimaryKeyTrait>::ValueType>,
    C: ConnectionTrait,
{
    E::find_by_id(id)
        .filter(parent_column.eq(parent_id))
        .filter(P::active_exists(parent_id))
        .one(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("{what} not found")))
}
