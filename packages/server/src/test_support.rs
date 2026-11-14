//! Shared fixtures for the crate's unit tests.

use chrono::Utc;
use common::db::DatabaseConnection;
use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

/// A throwaway Postgres 17 with the full schema (`database::init_db`), for
/// unit tests that need real SQL semantics `MockDatabase` cannot give. Keep
/// the container alive for the test's duration.
pub(crate) async fn postgres() -> (ContainerAsync<Postgres>, DatabaseConnection) {
    let (container, db, _) = postgres_with_url().await;
    (container, db)
}

/// [`postgres`], plus the database URL, for tests that open their own
/// connections.
pub(crate) async fn postgres_with_url() -> (ContainerAsync<Postgres>, DatabaseConnection, String) {
    let container = Postgres::default()
        .with_tag("17-alpine")
        .start()
        .await
        .expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres host port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = crate::database::init_db(&url)
        .await
        .expect("init schema on test db");
    (container, db, url)
}

/// A user and a problem to hang test submissions on. Returns their ids.
pub(crate) async fn seed_user_and_problem(db: &DatabaseConnection) -> (i32, i32) {
    use crate::entity::{problem, user};
    let now = Utc::now();
    let u = user::ActiveModel {
        username: Set("test-user".into()),
        display_name: Set("test-user".into()),
        password: Set("x".into()),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert user");
    let p = problem::ActiveModel {
        title: Set("test-problem".into()),
        content: Set("c".into()),
        time_limit: Set(1000),
        memory_limit: Set(262_144),
        checker_format: Set(broccoli_api_types::problem::DEFAULT_CHECKER_FORMAT.into()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert problem");
    (u.id, p.id)
}

/// Returns once another session of this database is waiting for a row lock.
/// Polls `pg_stat_activity`, the server's own view of who waits on what, so
/// the caller acts only after the wait has begun.
pub(crate) async fn wait_until_blocked_on_a_lock(db: &DatabaseConnection) {
    loop {
        let row = db
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT COUNT(*)::bigint AS n FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        if row.try_get::<i64>("", "n").unwrap() > 0 {
            return;
        }
        tokio::task::yield_now().await;
    }
}

/// What [`MockPlugins`] answers a call with, from the plugin id, the function
/// name and the call's input.
type Answer =
    dyn Fn(&str, &str, &[u8]) -> Result<Vec<u8>, plugin_core::error::PluginError> + Send + Sync;

/// The crate's one plugin-manager double: it owns a registry and answers
/// every call with a closure, never running a guest.
pub(crate) struct MockPlugins {
    registry: plugin_core::registry::PluginRegistry,
    config: plugin_core::config::PluginConfig,
    host_functions: plugin_core::host::HostFunctionRegistry,
    i18n: plugin_core::i18n::I18nRegistry,
    answer: Box<Answer>,
}

impl MockPlugins {
    /// Answers each call with `answer(plugin_id, func_name, input)`.
    pub(crate) fn new(
        registry: plugin_core::registry::PluginRegistry,
        answer: impl Fn(&str, &str, &[u8]) -> Result<Vec<u8>, plugin_core::error::PluginError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            registry,
            config: plugin_core::config::PluginConfig::default(),
            host_functions: plugin_core::host::HostFunctionRegistry::new(),
            i18n: plugin_core::i18n::I18nRegistry::new(),
            answer: Box::new(answer),
        }
    }

    /// Panics on any call: proves the code under test never reaches for the
    /// plugin host.
    pub(crate) fn never_called(registry: plugin_core::registry::PluginRegistry) -> Self {
        Self::new(registry, |plugin_id, func_name, _| {
            panic!("plugin '{plugin_id}'::{func_name} must not be called here")
        })
    }

    /// A plugin that cannot answer before its deadline: every call is
    /// [`PluginError::Unavailable`](plugin_core::error::PluginError::Unavailable).
    pub(crate) fn unavailable(registry: plugin_core::registry::PluginRegistry) -> Self {
        Self::new(registry, |plugin_id, func_name, _| {
            Err(plugin_core::error::PluginError::Unavailable {
                plugin_id: plugin_id.to_string(),
                func_name: func_name.to_string(),
            })
        })
    }
}

#[async_trait::async_trait]
impl plugin_core::traits::PluginInvoker for MockPlugins {
    fn get_registry(&self) -> &plugin_core::registry::PluginRegistry {
        &self.registry
    }

    fn get_config(&self) -> &plugin_core::config::PluginConfig {
        &self.config
    }

    fn executor(&self) -> &plugin_core::executor::Executor {
        plugin_core::executor::Executor::shared_for_tests()
    }

    async fn call_raw(
        &self,
        plugin_id: &str,
        func_name: &str,
        input: Vec<u8>,
        _options: plugin_core::executor::CallOptions,
    ) -> Result<Vec<u8>, plugin_core::error::PluginError> {
        (self.answer)(plugin_id, func_name, &input)
    }
}

impl plugin_core::traits::PluginManager for MockPlugins {
    fn get_host_functions(&self) -> &plugin_core::host::HostFunctionRegistry {
        &self.host_functions
    }

    fn get_i18n_registry(&self) -> &plugin_core::i18n::I18nRegistry {
        &self.i18n
    }

    fn resolve(
        &self,
        _manifest: &plugin_core::manifest::PluginManifest,
    ) -> Option<(String, Vec<String>)> {
        None
    }
}
