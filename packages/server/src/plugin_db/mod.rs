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
//!   (at most `MAX_OPEN_TXNS_PER_PLUGIN` for a
//!   plugin) and one for its current statement;
//! - a plugin with no idle connection of its own, while the pools already
//!   hold `budget` connections, first closes an idle connection of another
//!   plugin, so the pools keep to the budget as load moves between plugins;
//! - a connection returning to its pool drops the session state its
//!   statements left behind (`SESSION_RESET`);
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
use plugin_core::pool::Liveness;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};
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
/// boot), stops its `LISTEN`s, puts the connection back on the plugin's login
/// role, and resets its session settings to what the connection started with.
/// `RESET ALL` leaves the role alone (Postgres exempts it), so `RESET ROLE`
/// comes first. sqlx's prepared statements and the plugin's temporary tables,
/// which are its own, stay.
const SESSION_RESET: &str = "SELECT pg_advisory_unlock_all(); UNLISTEN *; RESET ROLE; RESET ALL";

/// Drop the session state a plugin's statements left on `conn`
/// (`SESSION_RESET`). The pools run it on every connection returning to
/// them.
pub async fn reset_session(
    conn: &mut sea_orm::sqlx::PgConnection,
) -> Result<(), sea_orm::sqlx::Error> {
    sea_orm::sqlx::Executor::execute(conn, SESSION_RESET)
        .await
        .map(drop)
}

/// How long a plugin's SQL stays unavailable after its provisioning failed,
/// before the next call tries again.
const PROVISION_BACKOFF: Duration = Duration::from_secs(30);

/// Why a plugin cannot run SQL right now. The host function answers it by
/// failing the call as unavailable, which callers retry.
#[derive(Debug, thiserror::Error)]
pub enum Unavailable {
    #[error("no plugin SQL connection was free within the call's deadline")]
    Busy,
    #[error(
        "the call waited for a plugin SQL connection while it holds one of its own; \
         its transaction or statement must end first"
    )]
    SelfHeld,
    #[error("the plugin SQL statement was cut at the call's deadline; its backend is cancelled")]
    Deadline,
    #[error("the plugin was unloaded while this call ran")]
    Unloaded,
    #[error("the plugin's database role could not be provisioned: {0}")]
    Unprovisioned(String),
    #[error("the database cannot serve the statement: {1}")]
    Outage(common::db::Outage, String),
}

impl Unavailable {
    /// A connection (or a transaction's) a call asked its lease for, as the
    /// call takes it: no connection free in time is contention
    /// ([`Self::Busy`]), unless the call already holds one of its own
    /// (`reused`), which is the plugin waiting on itself
    /// ([`Self::SelfHeld`]); any other answer is as [`Self::settle`] takes
    /// it.
    pub fn settle_acquire<T>(
        answer: Result<T, DbErr>,
        reused: bool,
    ) -> Result<Result<T, DbErr>, Self> {
        match answer {
            Err(e) if common::db::outage(&e) == Some(common::db::Outage::Busy) => {
                Err(if reused { Self::SelfHeld } else { Self::Busy })
            }
            answer => Self::settle(answer),
        }
    }

    /// A plugin statement's answer, as the call takes it: an outage of the
    /// database ([`common::db::is_outage`]) says nothing about the
    /// statement, so it fails the call as unavailable, which callers answer
    /// with "try again" as they answer a busy pool; any other error is the
    /// statement's own and goes back to the plugin.
    pub fn settle<T>(answer: Result<T, DbErr>) -> Result<Result<T, DbErr>, Self> {
        match answer {
            Err(e) => match common::db::outage(&e) {
                Some(outage) => Err(Self::Outage(outage, e.to_string())),
                None => Ok(Err(e)),
            },
            answer => Ok(answer),
        }
    }
}

/// The plugin call a lease is for.
#[derive(Clone, Default)]
pub struct SqlCall {
    /// The call tree's [`plugin_core::executor::call_locals`], which hold the
    /// permit it already has.
    pub locals: Option<Arc<CallLocals>>,
    /// The liveness of the instance the call runs on: a lease never
    /// provisions or opens a pool for a call of an unloaded plugin.
    pub instance: Option<Liveness>,
    /// The call's deadline: a lease waits for a permit no longer, and the
    /// host functions cut every statement at it.
    pub deadline: Option<Instant>,
}

impl SqlCall {
    /// The plugin call running on this thread.
    pub fn current() -> Self {
        Self {
            locals: plugin_core::executor::call_locals(),
            instance: plugin_core::executor::current_instance(),
            deadline: plugin_core::executor::current_deadline(),
        }
    }

    fn unloaded(&self) -> bool {
        self.instance.as_ref().is_some_and(|i| !i.is_live())
    }
}

/// The right to run on a plugin's pool: the pool and the call's permit from
/// the shared budget. A transaction keeps the permit until it ends.
pub struct Lease {
    pub db: DatabaseConnection,
    pub permit: Arc<OwnedSemaphorePermit>,
    /// The call already held its permit (a transaction or a statement of
    /// its own is open): a wait for a connection now is one the call made
    /// itself ([`Unavailable::SelfHeld`]).
    pub reused: bool,
}

impl Lease {
    /// A connection of the lease's pool for one statement, and the pid of
    /// its backend, so a statement cut at its call's deadline is cancelled
    /// there ([`PluginSqlPools::cancel_backend`]). It is checked out through
    /// the pool's handle, so it fails fast while the database is unreachable
    /// and counts as busy while held, like the pool's transactions.
    pub async fn connection(&self) -> Result<(common::db::PooledConnection, i32), DbErr> {
        let mut conn = self.db.acquire().await?;
        let pid = conn.backend_pid().await?;
        Ok((conn, pid))
    }
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

    /// A lease on `plugin_id`'s pool for `call`, provisioning the plugin's
    /// role and schema and building its pool on first use. `read` is the
    /// content columns the plugin's manifest declares; a change from what the
    /// role was granted provisions it again.
    pub async fn lease(
        &self,
        plugin_id: &str,
        read: &[SqlReadScope],
        call: &SqlCall,
    ) -> Result<Lease, Unavailable> {
        if call.unloaded() {
            return Err(Unavailable::Unloaded);
        }
        let budget = &self.inner.leases;
        let locals = call.locals.as_deref();
        let held = locals.and_then(|c| c.with(|held: &mut HeldPermits| held.live(budget)));
        let reused = held.is_some();
        let permit = match held {
            Some(permit) => permit,
            None => {
                let mut wait = self.inner.acquire_timeout;
                if let Some(deadline) = call.deadline {
                    wait = wait.min(deadline.saturating_duration_since(Instant::now()));
                }
                let permit = tokio::time::timeout(wait, budget.clone().acquire_owned())
                    .await
                    .map_err(|_| Unavailable::Busy)?
                    .map_err(|_| Unavailable::Busy)?;
                let permit = Arc::new(permit);
                if let Some(locals) = locals {
                    locals.with(|held: &mut HeldPermits| held.hold(budget, &permit));
                }
                permit
            }
        };
        let db = self.pool(plugin_id, read, call).await?;
        self.inner.make_room(plugin_id).await;
        Ok(Lease { db, permit, reused })
    }

    /// `plugin_id`'s pool. A failed provisioning is logged once, at error
    /// level, and not retried for [`PROVISION_BACKOFF`]; calls in between
    /// fail at once.
    async fn pool(
        &self,
        plugin_id: &str,
        read: &[SqlReadScope],
        call: &SqlCall,
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
                .regrant(plugin_id, &slot, read, call)
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
            .get_or_try_init(|| self.open(plugin_id, &slot, read, call))
            .await
        {
            Ok(db) => {
                *slot.failed.lock().unwrap_or_else(|p| p.into_inner()) = None;
                self.regrant(plugin_id, &slot, read, call)
                    .await
                    .map(|()| db.clone())
            }
            Err(Opened::Unloaded) => Err(Unavailable::Unloaded),
            Err(Opened::Failed(e)) => {
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
        call: &SqlCall,
    ) -> Result<(), Unavailable> {
        let mut granted = slot.granted.lock().await;
        // Under the lock `forget` waits on: a call of an unloaded plugin
        // never provisions its role again.
        if call.unloaded() {
            return Err(Unavailable::Unloaded);
        }
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
        call: &SqlCall,
    ) -> Result<DatabaseConnection, Opened> {
        let mut granted = slot.granted.lock().await;
        // Under the lock `forget` waits on: a call of an unloaded plugin
        // never provisions its role again.
        if call.unloaded() {
            return Err(Opened::Unloaded);
        }
        let login = schema::provision(&self.inner.host, plugin_id, read).await?;
        *granted = read.to_vec();
        drop(granted);
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
                reset_session(conn).await?;
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

/// Why a pool was not opened.
enum Opened {
    Unloaded,
    Failed(DbErr),
}

impl From<DbErr> for Opened {
    fn from(e: DbErr) -> Self {
        Self::Failed(e)
    }
}

impl PluginSqlPools {
    /// Drop `plugin_id`'s pool once the plugin is unloaded here, so its next
    /// load provisions its role again. Returns once no provisioning of the
    /// old pool is in flight: one that started before the unload has
    /// finished, and every later one sees the call's instance retired and
    /// refuses, so a [`schema::retire`] after this stays in force.
    pub async fn forget(&self, plugin_id: &str) {
        let slot = self
            .inner
            .pools
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(plugin_id);
        let Some(slot) = slot else { return };
        drop(slot.granted.lock().await);
        if let Some(db) = slot.pool.get().cloned() {
            tokio::spawn(async move {
                let _ = db.close().await;
            });
        }
    }

    /// Cancel the statement plugin backend `pid` runs, through the app role,
    /// which holds every plugin role (see [`schema::provision`]).
    pub async fn cancel_backend(&self, pid: i32) {
        let cancel = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_cancel_backend($1)",
            [pid.into()],
        );
        if let Err(e) = self.inner.host.execute_raw(cancel).await {
            tracing::warn!(pid, error = %e, "Could not cancel a plugin statement past its deadline");
        }
    }

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
            .map(|(_, db)| db.connections().open)
            .sum()
    }

    /// Close an idle connection of another plugin when `plugin_id` would
    /// otherwise open one past the budget.
    async fn make_room(&self, plugin_id: &str) {
        let built = self.built();
        let own_idle = built
            .iter()
            .any(|(id, db)| id == plugin_id && db.connections().idle > 0);
        let open: u32 = built.iter().map(|(_, db)| db.connections().open).sum();
        if own_idle || open < self.budget {
            return;
        }
        for (id, db) in &built {
            if id != plugin_id && db.close_idle().await {
                return;
            }
        }
    }
}
