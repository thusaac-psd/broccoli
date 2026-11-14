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
//! - at most `budget` plugin calls run SQL at once, server-wide: a [`Lease`]
//!   holds one of `budget` semaphore permits. A call that already holds one,
//!   through an open transaction or a lease of its own or of the call it is
//!   nested in, reuses it for every further statement, so a call never waits
//!   on a permit it holds. Such a call has one connection per open transaction
//!   (at most [`crate::host_funcs::sql::MAX_OPEN_TXNS_PER_PLUGIN`] for a
//!   plugin) and one for its current statement;
//! - a plugin with no idle connection of its own, while the pools already
//!   hold `budget` connections, first closes an idle connection of another
//!   plugin, so the pools keep to the budget as load moves between plugins;
//! - a connection returning to its pool drops the session state its
//!   statements left behind ([`SESSION_RESET`]);
//! - a connection released while the pools hold more than `budget` (first
//!   uses racing past that check, or a connection still on its way back to
//!   its pool, which sqlx does from a task of its own) is closed instead of
//!   kept idle, so once released connections are back the pools hold at most
//!   `budget`.

pub mod schema;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use common::db::{DatabaseConnection, PgPoolSpec, PgSession};
use plugin_core::executor::CallLocals;
use plugin_core::manifest::SqlReadScope;
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
    pools: StdMutex<HashMap<String, Arc<Slot>>>,
}

/// One plugin's pool, built once its role and schema are provisioned.
#[derive(Default)]
struct Slot {
    pool: OnceCell<DatabaseConnection>,
    /// The content columns the role was last granted, and the lock that
    /// serializes granting new ones.
    granted: tokio::sync::Mutex<Vec<SqlReadScope>>,
    /// When provisioning last failed, while it keeps failing.
    failed: StdMutex<Option<Instant>>,
}

/// Run on a plugin connection as it returns to its pool: releases the session
/// advisory locks it took (a lock on the host's boot key would stall the next
/// boot), stops its `LISTEN`s, and resets its session settings to what the
/// connection started with. sqlx's prepared statements and the plugin's
/// temporary tables, which are its own, stay.
const SESSION_RESET: &str = "SELECT pg_advisory_unlock_all(); UNLISTEN *; RESET ALL";

/// How long a plugin's SQL stays unavailable after its provisioning failed,
/// before the next call tries again.
const PROVISION_BACKOFF: Duration = Duration::from_secs(30);

/// Why a plugin cannot run SQL right now. The host function answers it by
/// failing the call as unavailable, which callers retry.
#[derive(Debug, thiserror::Error)]
pub enum Unavailable {
    #[error("no plugin SQL connection was free within the call's deadline")]
    Busy,
    #[error("the plugin's database role could not be provisioned: {0}")]
    Unprovisioned(String),
}

/// The right to run on a plugin's pool: the pool and the call's permit from
/// the shared budget. A transaction keeps the permit until it ends.
pub struct Lease {
    pub db: DatabaseConnection,
    pub permit: Arc<OwnedSemaphorePermit>,
}

/// The permits a call tree holds, kept in its [`CallLocals`] while any of its
/// leases or transactions holds them, keyed by the budget they belong to.
#[derive(Default)]
struct HeldPermits(Vec<(Weak<Semaphore>, Weak<OwnedSemaphorePermit>)>);

impl HeldPermits {
    fn live(&mut self, budget: &Arc<Semaphore>) -> Option<Arc<OwnedSemaphorePermit>> {
        self.0.retain(|(_, permit)| permit.strong_count() > 0);
        self.0
            .iter()
            .find(|(sem, _)| sem.as_ptr() == Arc::as_ptr(budget))
            .and_then(|(_, permit)| permit.upgrade())
    }

    fn hold(&mut self, budget: &Arc<Semaphore>, permit: &Arc<OwnedSemaphorePermit>) {
        self.0
            .push((Arc::downgrade(budget), Arc::downgrade(permit)));
    }
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

    /// A lease on `plugin_id`'s pool for the call tree `call` (the running
    /// plugin call's [`plugin_core::executor::call_locals`]), provisioning the
    /// plugin's role and schema and building its pool on first use. `read` is
    /// the content columns the plugin's manifest declares; a change from what
    /// the role was granted provisions it again.
    pub async fn lease(
        &self,
        plugin_id: &str,
        read: &[SqlReadScope],
        call: Option<&CallLocals>,
    ) -> Result<Lease, Unavailable> {
        let budget = &self.inner.leases;
        let held = call.and_then(|c| c.with(|held: &mut HeldPermits| held.live(budget)));
        let permit = match held {
            Some(permit) => permit,
            None => {
                let permit = tokio::time::timeout(
                    self.inner.acquire_timeout,
                    budget.clone().acquire_owned(),
                )
                .await
                .map_err(|_| Unavailable::Busy)?
                .map_err(|_| Unavailable::Busy)?;
                let permit = Arc::new(permit);
                if let Some(call) = call {
                    call.with(|held: &mut HeldPermits| held.hold(budget, &permit));
                }
                permit
            }
        };
        let db = self.pool(plugin_id, read).await?;
        self.inner.make_room(plugin_id).await;
        Ok(Lease { db, permit })
    }

    /// `plugin_id`'s pool. A failed provisioning is logged once, at error
    /// level, and not retried for [`PROVISION_BACKOFF`]; calls in between
    /// fail at once.
    async fn pool(
        &self,
        plugin_id: &str,
        read: &[SqlReadScope],
    ) -> Result<DatabaseConnection, Unavailable> {
        let slot = self
            .inner
            .pools
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(plugin_id.to_string())
            .or_default()
            .clone();
        if let Some(db) = slot.pool.get() {
            return self
                .regrant(plugin_id, &slot, read)
                .await
                .map(|()| db.clone());
        }
        let failed = *slot.failed.lock().unwrap_or_else(|p| p.into_inner());
        if failed.is_some_and(|at| at.elapsed() < PROVISION_BACKOFF) {
            return Err(Unavailable::Unprovisioned(
                "provisioning failed recently; retrying shortly".into(),
            ));
        }
        match slot
            .pool
            .get_or_try_init(|| self.open(plugin_id, &slot, read))
            .await
        {
            Ok(db) => {
                *slot.failed.lock().unwrap_or_else(|p| p.into_inner()) = None;
                self.regrant(plugin_id, &slot, read)
                    .await
                    .map(|()| db.clone())
            }
            Err(e) => {
                let first = slot
                    .failed
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .replace(Instant::now())
                    .is_none();
                if first {
                    tracing::error!(
                        plugin_id,
                        error = %e,
                        "Could not provision the plugin's database role; its SQL is \
                         unavailable and is retried every {}s",
                        PROVISION_BACKOFF.as_secs()
                    );
                } else {
                    tracing::debug!(plugin_id, error = %e, "Plugin provisioning failed again");
                }
                Err(Unavailable::Unprovisioned(e.to_string()))
            }
        }
    }

    /// Provision `plugin_id` again when its manifest now declares other
    /// content columns than its role was granted.
    async fn regrant(
        &self,
        plugin_id: &str,
        slot: &Slot,
        read: &[SqlReadScope],
    ) -> Result<(), Unavailable> {
        let mut granted = slot.granted.lock().await;
        if granted.as_slice() == read {
            return Ok(());
        }
        schema::provision(&self.inner.host, plugin_id, read)
            .await
            .map_err(|e| Unavailable::Unprovisioned(e.to_string()))?;
        *granted = read.to_vec();
        tracing::info!(
            plugin_id,
            ?read,
            "Granted the plugin's declared content columns"
        );
        Ok(())
    }

    async fn open(
        &self,
        plugin_id: &str,
        slot: &Slot,
        read: &[SqlReadScope],
    ) -> Result<DatabaseConnection, DbErr> {
        let login = schema::provision(&self.inner.host, plugin_id, read).await?;
        *slot.granted.lock().await = read.to_vec();
        let connect = common::db::pg_connect_options(&self.inner.url, &self.inner.session)?
            .username(&login.names.role)
            .password(&login.password);
        // Room for every permit plus the transactions a call may hold open
        // beside its current statement.
        let max = self.inner.budget + crate::host_funcs::sql::MAX_OPEN_TXNS_PER_PLUGIN as u32;
        let spec = PgPoolSpec {
            acquire_timeout: self.inner.acquire_timeout,
            ..PgPoolSpec::new(max, 0)
        };
        let pools = Arc::downgrade(&self.inner);
        let options = spec.pool_options().after_release(move |conn, _| {
            let keep = Weak::upgrade(&pools).is_none_or(|p| p.open() <= p.budget);
            Box::pin(async move {
                if !keep {
                    return Ok(false);
                }
                // Session state a statement left behind would outlive its
                // call on the pooled connection: drop it before the next.
                sea_orm::sqlx::Executor::execute(&mut *conn, SESSION_RESET).await?;
                Ok(true)
            })
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
            .filter_map(|(id, slot)| slot.pool.get().map(|db| (id.clone(), db.clone())))
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
