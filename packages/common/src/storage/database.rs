use std::io::Cursor;

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QuerySelect, Schema, Statement,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::error::StorageError;
use super::hash::ContentHash;
use super::traits::{BlobStore, BoxReader};

/// True when a Postgres error came from two connections concurrently
/// running `CREATE TABLE IF NOT EXISTS` against the same fresh database.
/// Postgres' DDL path through `pg_type` is not internally serialized,
/// so the loser surfaces a unique-violation on `pg_type_typname_nsp_index`
/// or a "type ... already exists" / "relation ... already exists" error even
/// though `IF NOT EXISTS` was specified.
pub fn is_concurrent_create_race(err: &DbErr) -> bool {
    let msg = err.to_string();
    msg.contains("pg_type_typname_nsp_index") || msg.contains("already exists")
}

mod blob_data {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "blob_data")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub content_hash: String,
        pub data: Vec<u8>,
        pub size: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub struct DatabaseBlobStore {
    db: DatabaseConnection,
    max_size: u64,
}

impl DatabaseBlobStore {
    pub fn new(db: DatabaseConnection, max_size: u64) -> Self {
        Self { db, max_size }
    }

    pub async fn ensure_table(db: &DatabaseConnection) -> Result<(), StorageError> {
        let backend = db.get_database_backend();
        let schema = Schema::new(backend);
        let mut create_stmt = schema.create_table_from_entity(blob_data::Entity);
        create_stmt.if_not_exists();

        if let Err(e) = db.execute(&create_stmt).await {
            // Postgres `CREATE TABLE IF NOT EXISTS` is not race-safe. When two
            // workers run init concurrently against a fresh database, one wins
            // and the other fails inserting into the type catalog. Treat that
            // as success since the table is now present.
            if is_concurrent_create_race(&e) {
                tracing::debug!(error = %e, "concurrent blob_data create race; treating as success");
                return Ok(());
            }
            return Err(StorageError::Backend(format!(
                "Failed to create blob_data table: {e}"
            )));
        }
        Ok(())
    }

    async fn read_all_limited(
        mut reader: impl AsyncRead + Unpin,
        max_size: u64,
    ) -> Result<(Vec<u8>, u64, ContentHash), StorageError> {
        let mut hasher = Sha256::new();
        let mut total_bytes: u64 = 0;
        let mut data = Vec::new();
        let mut chunk = [0u8; 64 * 1024];

        loop {
            let n = reader.read(&mut chunk).await?;
            if n == 0 {
                break;
            }

            total_bytes += n as u64;
            if total_bytes > max_size {
                return Err(StorageError::SizeLimitExceeded {
                    actual: total_bytes,
                    limit: max_size,
                });
            }

            hasher.update(&chunk[..n]);
            data.extend_from_slice(&chunk[..n]);
        }

        let hash = ContentHash::from_bytes(hasher.finalize().into());
        Ok((data, total_bytes, hash))
    }
}

#[async_trait]
impl BlobStore for DatabaseBlobStore {
    async fn put_stream(&self, mut reader: BoxReader) -> Result<ContentHash, StorageError> {
        let (buf, total_bytes, hash) = Self::read_all_limited(&mut reader, self.max_size).await?;
        let hash_hex = hash.to_hex();

        let size = i64::try_from(total_bytes)
            .map_err(|_| StorageError::Backend(format!("blob size overflow: {total_bytes}")))?;

        // `created_at` is the database clock, the platform's wall time.
        self.db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO blob_data (content_hash, data, size, created_at) \
                 VALUES ($1, $2, $3, now()) ON CONFLICT (content_hash) DO NOTHING",
                [hash_hex.into(), buf.into(), size.into()],
            ))
            .await
            .map_err(|e| StorageError::Backend(format!("put_stream failed: {e}")))?;

        Ok(hash)
    }

    async fn get_stream(&self, hash: &ContentHash) -> Result<BoxReader, StorageError> {
        let hash_hex = hash.to_hex();

        let model = blob_data::Entity::find_by_id(hash_hex.clone())
            .one(&self.db)
            .await
            .map_err(|e| StorageError::Backend(format!("get_stream query failed: {e}")))?;

        let model = model.ok_or(StorageError::NotFound(hash_hex))?;

        Ok(Box::new(Cursor::new(model.data)))
    }

    async fn get_range(
        &self,
        hash: &ContentHash,
        offset: u64,
        len: usize,
    ) -> Result<(Vec<u8>, bool), StorageError> {
        let hash_hex = hash.to_hex();
        let start = i32::try_from(offset.saturating_add(1))
            .map_err(|_| StorageError::Backend(format!("range offset overflow: {offset}")))?;
        let len_i32 = i32::try_from(len)
            .map_err(|_| StorageError::Backend(format!("range length overflow: {len}")))?;

        let stmt = Statement::from_sql_and_values(
            self.db.get_database_backend(),
            r#"SELECT substr(data, $1, $2) AS chunk, size FROM blob_data WHERE content_hash = $3"#,
            [start.into(), len_i32.into(), hash_hex.clone().into()],
        );
        let row = self
            .db
            .query_one_raw(stmt)
            .await
            .map_err(|e| StorageError::Backend(format!("get_range query failed: {e}")))?;
        let row = row.ok_or(StorageError::NotFound(hash_hex))?;

        let bytes: Vec<u8> = row
            .try_get("", "chunk")
            .map_err(|e| StorageError::Backend(format!("get_range chunk decode failed: {e}")))?;
        let size: i64 = row
            .try_get("", "size")
            .map_err(|e| StorageError::Backend(format!("get_range size decode failed: {e}")))?;
        if size < 0 {
            return Err(StorageError::Backend(format!(
                "size decode failed: negative size {size}"
            )));
        }

        Ok((bytes, offset + len as u64 >= size as u64))
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        let hash_hex = hash.to_hex();

        let row = blob_data::Entity::find_by_id(hash_hex)
            .select_only()
            .column(blob_data::Column::ContentHash)
            .into_tuple::<(String,)>()
            .one(&self.db)
            .await
            .map_err(|e| StorageError::Backend(format!("exists query failed: {e}")))?;

        Ok(row.is_some())
    }

    async fn delete(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        let hash_hex = hash.to_hex();

        let result = blob_data::Entity::delete_by_id(hash_hex)
            .exec(&self.db)
            .await
            .map_err(|e| StorageError::Backend(format!("delete failed: {e}")))?;

        Ok(result.rows_affected > 0)
    }

    async fn size(&self, hash: &ContentHash) -> Result<u64, StorageError> {
        let hash_hex = hash.to_hex();

        let row = blob_data::Entity::find_by_id(hash_hex.clone())
            .select_only()
            .column(blob_data::Column::Size)
            .into_tuple::<(i64,)>()
            .one(&self.db)
            .await
            .map_err(|e| StorageError::Backend(format!("size query failed: {e}")))?;

        let (size,) = row.ok_or(StorageError::NotFound(hash_hex))?;

        if size < 0 {
            return Err(StorageError::Backend(format!(
                "size decode failed: negative size {size}"
            )));
        }

        Ok(size as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::is_concurrent_create_race;
    use sea_orm::DbErr;

    #[test]
    fn classifies_pg_type_unique_violation_as_race() {
        let err = DbErr::Custom(
            "Execution Error: error returned from database: \
             duplicate key value violates unique constraint \"pg_type_typname_nsp_index\""
                .into(),
        );
        assert!(is_concurrent_create_race(&err));
    }

    #[test]
    fn classifies_relation_already_exists_as_race() {
        let err = DbErr::Custom(
            "error returned from database: relation \"blob_data\" already exists".into(),
        );
        assert!(is_concurrent_create_race(&err));
    }

    #[test]
    fn classifies_type_already_exists_as_race() {
        let err =
            DbErr::Custom("error returned from database: type \"blob_data\" already exists".into());
        assert!(is_concurrent_create_race(&err));
    }

    #[test]
    fn does_not_classify_unrelated_error_as_race() {
        let err = DbErr::Custom("connection refused".into());
        assert!(!is_concurrent_create_race(&err));
    }
}
