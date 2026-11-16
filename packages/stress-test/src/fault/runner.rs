//! Dispatcher for the `fault` subcommand.
//!
//! Mirrors the original fault-harness binary: build a `ScenarioContext` from
//! flags, optionally spin up an ephemeral Redis testcontainer (cancel-storm and
//! rolling-worker-restart, when no `--redis-url` is given - kill-server-recovery
//! presumes operator-managed infra), run the chosen scenario, write the
//! transcript JSON, and exit non-zero on failure.

use testcontainers::runners::AsyncRunner;

use crate::cli::{
    BurstArgs, CancelStormArgs, FaultArgs, FaultScenario, KillServerRecoveryArgs,
    RollingWorkerRestartArgs,
};
use crate::client::AuthCreds;
use crate::fault::scenarios::{
    Scenario, ScenarioContext, burst::Burst, cancel_storm::CancelStorm,
    kill_server_recovery::KillServerRecovery, rolling_worker_restart::RollingWorkerRestart,
};

/// Run the fault subcommand. Returns the exit code (0 = pass, 1 = scenario
/// failed, 2 = setup error).
pub async fn run(args: FaultArgs) -> u8 {
    let result = match &args.scenario {
        FaultScenario::CancelStorm(s) => run_cancel_storm(s).await,
        FaultScenario::KillServerRecovery(s) => run_kill_server_recovery(s).await,
        FaultScenario::RollingWorkerRestart(s) => run_rolling_worker_restart(s).await,
        FaultScenario::Burst(s) => run_burst(s).await,
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fault: error: {e}");
            2
        }
    }
}

async fn run_rolling_worker_restart(args: &RollingWorkerRestartArgs) -> anyhow::Result<u8> {
    let (redis_url, _container) = match args.redis_url.clone() {
        Some(url) => (url, None),
        None => {
            tracing::info!("no --redis-url provided; starting ephemeral Redis testcontainer");
            let container = common::test_util::redis_image().start().await?;
            let port = container.get_host_port_ipv4(6379).await?;
            (format!("redis://127.0.0.1:{port}"), Some(container))
        }
    };

    let ctx = ScenarioContext {
        redis_url,
        db_url: None,
        server_url: None,
        admin_token: None,
        kill_command: args.restart_command.clone(),
    };

    let outcome = RollingWorkerRestart {
        leader_ttl_secs: args.leader_ttl_secs,
        leader_hold_ms: args.leader_hold_ms,
        follower_poll_interval_ms: args.follower_poll_interval_ms,
    }
    .run(&ctx)
    .await?;

    outcome.transcript.write_json(&args.out)?;
    tracing::info!(
        path = %args.out.display(),
        passed = outcome.passed,
        "wrote transcript"
    );

    Ok(if outcome.passed { 0 } else { 1 })
}

async fn run_cancel_storm(args: &CancelStormArgs) -> anyhow::Result<u8> {
    // cancel-storm is Redis-only; spin up an ephemeral container when no
    // --redis-url is given so operators can run it standalone.
    let (redis_url, _container) = match args.redis_url.clone() {
        Some(url) => (url, None),
        None => {
            tracing::info!("no --redis-url provided; starting ephemeral Redis testcontainer");
            let container = common::test_util::redis_image().start().await?;
            let port = container.get_host_port_ipv4(6379).await?;
            (format!("redis://127.0.0.1:{port}"), Some(container))
        }
    };

    let ctx = ScenarioContext {
        redis_url,
        db_url: None,
        server_url: None,
        admin_token: None,
        kill_command: None,
    };

    let outcome = CancelStorm {
        batch_count: args.batch_count,
        readers: args.readers,
        key_ttl_secs: args.key_ttl_secs,
    }
    .run(&ctx)
    .await?;

    outcome.transcript.write_json(&args.out)?;
    tracing::info!(
        path = %args.out.display(),
        passed = outcome.passed,
        "wrote transcript"
    );

    Ok(if outcome.passed { 0 } else { 1 })
}

async fn run_burst(args: &BurstArgs) -> anyhow::Result<u8> {
    let creds = match (
        args.admin_token.clone(),
        args.admin_username.clone(),
        args.admin_password.clone(),
    ) {
        (Some(t), _, _) => AuthCreds::Token(t),
        (None, Some(u), Some(p)) => AuthCreds::UsernamePassword {
            username: u,
            password: p,
        },
        _ => {
            return Err(anyhow::anyhow!(
                "burst requires --admin-token, or both --admin-username and --admin-password"
            ));
        }
    };

    let type_weights = crate::fault::scenarios::burst::parse_type_weights(&args.type_weights)?;

    let burst = Burst {
        submission_count: args.submission_count,
        concurrency: args.concurrency,
        type_weights,
        strict: args.strict,
        terminal_deadline_secs: args.terminal_deadline_secs,
        metrics_url: args.metrics_url.clone(),
        fanout_test_cases_per_problem: args.fanout_test_cases_per_problem,
        baseline_plugin_pool_contention_delta: args.baseline_plugin_pool_contention_delta,
        max_plugin_pool_contention_baseline_ratio: args.max_plugin_pool_contention_baseline_ratio,
        max_plugin_pool_contention_delta: args.max_plugin_pool_contention_delta,
        require_fanout_saturation: args.require_fanout_saturation,
        keep_fixtures: args.keep_fixtures,
        contest_type: args.contest_type.clone(),
        problem_type: args.problem_type.clone(),
        server_url: args.server_url.clone(),
        creds,
        out: args.out.clone(),
    };

    let outcome = burst.run().await?;
    outcome.transcript.write_json(&args.out)?;
    tracing::info!(
        path = %args.out.display(),
        passed = outcome.passed,
        "wrote transcript"
    );

    Ok(if outcome.passed { 0 } else { 1 })
}

async fn run_kill_server_recovery(args: &KillServerRecoveryArgs) -> anyhow::Result<u8> {
    // kill-server-recovery presumes operator-managed infrastructure; refuse
    // to fabricate a Redis since the scenario also needs Postgres + a live
    // server replica it can kill.
    let ctx = ScenarioContext {
        redis_url: args.redis_url.clone(),
        db_url: Some(args.db_url.clone()),
        server_url: Some(args.server_url.clone()),
        admin_token: Some(args.admin_token.clone()),
        kill_command: Some(args.kill_command.clone()),
    };

    // TODO: stress-test's `client::Client` already implements admin login and
    // bearer auth (see packages/stress-test/src/client.rs); the
    // kill-server-recovery scenario still hand-rolls reqwest. Consolidate so
    // operators can pass --admin-username/--admin-password instead of having
    // to pre-mint a token. Not done here to keep this commit a pure move.
    let outcome = KillServerRecovery {
        submission_count: args.submission_count,
        observe_timeout_secs: args.observe_timeout_secs,
        problem_id: args.problem_id,
    }
    .run(&ctx)
    .await?;

    outcome.transcript.write_json(&args.out)?;
    tracing::info!(
        path = %args.out.display(),
        passed = outcome.passed,
        "wrote transcript"
    );

    Ok(if outcome.passed { 0 } else { 1 })
}
