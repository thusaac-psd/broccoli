//! The connections raw plugin SQL (`host.db.*`) runs on: one pool per plugin,
//! each logging in as that plugin's own role (see [`schema`]).
//!
//! A pool's connections authenticate as the plugin's role, and the role's
//! `search_path` names the plugin's schema first, so a plugin call pays no
//! `SET ROLE` or `SET search_path`, and a `RESET ROLE` lands on the plugin's
//! own role. A pool is built the first time its plugin runs SQL, after its
//! role and schema are provisioned.
//!
//! All plugin pools share one connection budget, the restricted plugin pool
//! size [`crate::database::PoolSizes::plugin`]:
//!
//! - at most `budget` statements or transactions hold a connection at once,
//!   server-wide (a semaphore permit per [`Lease`]);
//! - a plugin with no idle connection of its own, while the pools already
//!   hold `budget` connections, first closes an idle connection of another
//!   plugin, so the pools keep to the budget as load moves between plugins;
//! - a connection released while the pools hold more than `budget` (first
//!   uses racing past that check, or a connection still on its way back to
//!   its pool, which sqlx does from a task of its own) is closed instead of
//!   kept idle, so once released connections are back the pools hold at most
//!   `budget`.

pub mod schema;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use common::db::{DatabaseConnection, PgPoolSpec, PgSession};
use sea_orm::DbErr;
use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};

/// Every plugin's SQL pool, built on first use. Clones share the pools.
#[derive(Clone)]
pub struct PluginSqlPools {
    inner: Arc<Inner>,
}

struct Inner {
    /// The app role's connection that provisions plugin roles and schemas.
    host: DatabaseConnection,
    /// The database URL; plugin pools replace its credentials.
    url: String,
    session: PgSession,
    /// Connections all plugin pools may hold together.
    budget: u32,
    /// How long a call waits for a connection: its own deadline.
    acquire_timeout: Duration,
    leases: Arc<Semaphore>,
    pools: StdMutex<HashMap<String, Arc<OnceCell<DatabaseConnection>>>>,
}

/// The right to run on a plugin's pool: the pool and one of the shared
/// budget's permits. A transaction keeps its lease until it ends.
pub struct Lease {
    pub db: DatabaseConnection,
    pub permit: OwnedSemaphorePermit,
}

impl PluginSqlPools {
    /// Plugin pools on database `url`, provisioned through `host`, holding at
    /// most `budget` connections together. A call waits at most
    /// `acquire_timeout` for a connection.
    pub fn new(
        host: DatabaseConnection,
        url: &str,
        budget: u32,
        acquire_timeout: Duration,
        session: &PgSession,
    ) -> Self {
        let budget = budget.max(1);
        Self {
            inner: Arc::new(Inner {
                host,
                url: url.to_string(),
                session: crate::database::plugin_session(session),
                budget,
                acquire_timeout,
                leases: Arc::new(Semaphore::new(budget as usize)),
                pools: StdMutex::new(HashMap::new()),
            }),
        }
    }

    /// A lease on `plugin_id`'s pool, provisioning the plugin's role and
    /// schema and building its pool on first use.
    pub async fn lease(&self, plugin_id: &str) -> Result<Lease, DbErr> {
        let permit = tokio::time::timeout(
            self.inner.acquire_timeout,
            self.inner.leases.clone().acquire_owned(),
        )
        .await
        .map_err(|_| DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout))?
        .map_err(|_| DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::ConnectionClosed))?;
        let db = self.pool(plugin_id).await?;
        self.inner.make_room(plugin_id).await;
        Ok(Lease { db, permit })
    }

    async fn pool(&self, plugin_id: &str) -> Result<DatabaseConnection, DbErr> {
        let cell = self
            .inner
            .pools
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(plugin_id.to_string())
            .or_default()
            .clone();
        cell.get_or_try_init(|| self.open(plugin_id)).await.cloned()
    }

    async fn open(&self, plugin_id: &str) -> Result<DatabaseConnection, DbErr> {
        let login = schema::provision(&self.inner.host, plugin_id).await?;
        let connect = common::db::pg_connect_options(&self.inner.url, &self.inner.session)?
            .username(&login.names.role)
            .password(&login.password);
        let spec = PgPoolSpec {
            acquire_timeout: self.inner.acquire_timeout,
            ..PgPoolSpec::new(self.inner.budget, 0)
        };
        let pools = Arc::downgrade(&self.inner);
        let options = spec.pool_options().after_release(move |_, _| {
            let keep = Weak::upgrade(&pools).is_none_or(|p| p.open() <= p.budget);
            Box::pin(async move { Ok(keep) })
        });
        tracing::info!(
            plugin_id,
            role = %login.names.role,
            schema = %login.names.schema,
            "Opened the plugin's SQL pool"
        );
        Ok(common::db::connect_lazy_with(options, connect))
    }
}

impl PluginSqlPools {
    /// Connections the plugin pools hold together, in use or idle.
    pub fn open_connections(&self) -> u32 {
        self.inner.open()
    }
}

impl Inner {
    /// Every plugin pool's connections, open or opening.
    fn built(&self) -> Vec<(String, DatabaseConnection)> {
        self.pools
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter_map(|(id, cell)| cell.get().map(|db| (id.clone(), db.clone())))
            .collect()
    }

    /// Connections the plugin pools hold together.
    fn open(&self) -> u32 {
        self.built()
            .iter()
            .map(|(_, db)| sqlx_pool(db).size())
            .sum()
    }

    /// Close an idle connection of another plugin when `plugin_id` would
    /// otherwise open one past the budget.
    async fn make_room(&self, plugin_id: &str) {
        let built = self.built();
        let own_idle = built
            .iter()
            .any(|(id, db)| id == plugin_id && sqlx_pool(db).num_idle() > 0);
        let open: u32 = built.iter().map(|(_, db)| sqlx_pool(db).size()).sum();
        if own_idle || open < self.budget {
            return;
        }
        let idle = built
            .iter()
            .filter(|(id, _)| id != plugin_id)
            .find_map(|(_, db)| sqlx_pool(db).try_acquire());
        if let Some(conn) = idle
            && let Err(e) = conn.close().await
        {
            tracing::debug!(error = %e, "Closing an idle plugin connection failed");
        }
    }
}

fn sqlx_pool(db: &DatabaseConnection) -> &sea_orm::sqlx::PgPool {
    db.as_sea_orm().get_postgres_connection_pool()
}
