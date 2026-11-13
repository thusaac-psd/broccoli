use clap::Parser;
use stress_test::cli::{Cli, LoadProfile};

#[test]
fn requires_url() {
    // Clap allows --url to be omitted (so the `fault` subcommand can be used
    // without supplying a stress-mode URL), but stress mode rejects it at
    // validate() time.
    let cli = Cli::try_parse_from(["broccoli-stress-test", "--admin-token", "abc"]).unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn fault_subcommand_skips_top_level_validation() {
    use stress_test::cli::Command;
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "fault",
        "cancel-storm",
        "--batch-count",
        "100",
    ])
    .unwrap();
    assert!(matches!(cli.command, Some(Command::Fault(_))));
    assert!(cli.validate().is_ok());
}

#[test]
fn rolling_worker_restart_fault_args_parse() {
    use stress_test::cli::{Command, FaultScenario};
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "fault",
        "rolling-worker-restart",
        "--redis-url",
        "redis://127.0.0.1:6379",
        "--restart-command",
        "docker compose restart worker",
        "--leader-ttl-secs",
        "2",
        "--leader-hold-ms",
        "250",
        "--follower-poll-interval-ms",
        "25",
    ])
    .unwrap();

    let Some(Command::Fault(args)) = cli.command else {
        panic!("expected fault command");
    };
    let FaultScenario::RollingWorkerRestart(args) = args.scenario else {
        panic!("expected rolling-worker-restart scenario");
    };
    assert_eq!(args.redis_url.as_deref(), Some("redis://127.0.0.1:6379"));
    assert_eq!(
        args.restart_command.as_deref(),
        Some("docker compose restart worker")
    );
    assert_eq!(args.leader_ttl_secs, 2);
    assert_eq!(args.leader_hold_ms, 250);
    assert_eq!(args.follower_poll_interval_ms, 25);
}

#[test]
fn burst_fault_args_parse_fanout_metrics_guards() {
    use stress_test::cli::{Command, FaultScenario};
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "fault",
        "burst",
        "--server-url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--submission-count",
        "1000",
        "--fanout-test-cases-per-problem",
        "20",
        "--metrics-url",
        "http://localhost:3000/metrics",
        "--baseline-plugin-pool-contention-delta",
        "200",
        "--max-plugin-pool-contention-baseline-ratio",
        "0.1",
        "--max-plugin-pool-contention-delta",
        "15",
        "--require-fanout-saturation",
    ])
    .unwrap();

    let Some(Command::Fault(args)) = cli.command else {
        panic!("expected fault command");
    };
    let FaultScenario::Burst(args) = args.scenario else {
        panic!("expected burst scenario");
    };
    assert_eq!(args.submission_count, 1000);
    assert_eq!(args.fanout_test_cases_per_problem, 20);
    assert_eq!(
        args.metrics_url.as_deref(),
        Some("http://localhost:3000/metrics")
    );
    assert_eq!(args.baseline_plugin_pool_contention_delta, Some(200));
    assert_eq!(args.max_plugin_pool_contention_baseline_ratio, 0.1);
    assert_eq!(args.max_plugin_pool_contention_delta, 15);
    assert!(args.require_fanout_saturation);
}

#[test]
fn kill_server_recovery_logs_in_with_a_username_and_password() {
    use stress_test::cli::{Command, FaultScenario};
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "fault",
        "kill-server-recovery",
        "--db-url",
        "postgres://localhost/broccoli",
        "--server-url",
        "http://localhost:3000",
        "--admin-username",
        "admin",
        "--admin-password",
        "secret",
        "--kill-command",
        "true",
        "--redis-url",
        "redis://localhost",
    ])
    .unwrap();

    let Some(Command::Fault(args)) = cli.command else {
        panic!("expected fault command");
    };
    let FaultScenario::KillServerRecovery(args) = args.scenario else {
        panic!("expected kill-server-recovery scenario");
    };
    assert!(matches!(
        args.admin.auth(),
        Ok(stress_test::client::AuthCreds::UsernamePassword { .. })
    ));
}

#[test]
fn rejects_token_and_username_password_both_missing() {
    let cli =
        Cli::try_parse_from(["broccoli-stress-test", "--url", "http://localhost:3000"]).unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn accepts_username_password_pair() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-username",
        "admin",
        "--admin-password",
        "secret",
    ])
    .unwrap();
    assert!(cli.validate().is_ok());
    assert_eq!(cli.admin.admin_username.as_deref(), Some("admin"));
    assert_eq!(cli.admin.admin_password.as_deref(), Some("secret"));
}

#[test]
fn rejects_username_without_password() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-username",
        "admin",
    ])
    .unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn rejects_password_without_username() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-password",
        "secret",
    ])
    .unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn rejects_zero_total() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--total",
        "0",
    ])
    .unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn rejects_zero_rate() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--rate",
        "0",
    ])
    .unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn rejects_zero_concurrency() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--concurrency",
        "0",
    ])
    .unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn parses_all_numeric_flags_with_non_default_values() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--total",
        "1000",
        "--rate",
        "100",
        "--concurrency",
        "200",
        "--per-job-timeout",
        "120",
        "--p95-budget-ms",
        "30000",
        "--contest-concurrency",
        "40",
        "--seed",
        "42",
    ])
    .unwrap();
    assert_eq!(cli.total, 1000);
    assert_eq!(cli.rate, 100);
    assert_eq!(cli.concurrency, 200);
    assert_eq!(cli.per_job_timeout, 120);
    assert_eq!(cli.p95_budget_ms, 30000);
    assert_eq!(cli.contest_concurrency, 40);
    assert_eq!(cli.seed, 42);
    assert!(cli.validate().is_ok());
}

#[test]
fn parses_contest_id_passthrough_flags() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--contest-id",
        "7",
        "--problem-id",
        "13",
    ])
    .unwrap();
    assert_eq!(cli.contest_id, Some(7));
    assert_eq!(cli.problem_id, Some(13));
}

#[test]
fn parses_json_flag() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--json",
    ])
    .unwrap();
    assert!(cli.json);
}

#[test]
fn parses_keep_fixtures_flag() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--keep-fixtures",
    ])
    .unwrap();
    assert!(cli.keep_fixtures);
}

#[test]
fn parses_valid_run_id() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--run-id",
        "profile.run-1_2",
    ])
    .unwrap();
    assert_eq!(cli.run_id.as_deref(), Some("profile.run-1_2"));
    assert!(cli.validate().is_ok());
}

#[test]
fn rejects_invalid_run_id() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--run-id",
        "profile run 1",
    ])
    .unwrap();
    assert!(cli.validate().is_err());
}

#[test]
fn parses_mixed_profile_and_final_burst_flags() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--profile",
        "mixed",
        "--final-burst-duration",
        "300",
        "--final-burst-multiplier",
        "4",
    ])
    .unwrap();

    assert_eq!(cli.profile, LoadProfile::Mixed);
    assert_eq!(cli.final_burst_duration, 300);
    assert_eq!(cli.final_burst_multiplier, 4);
    assert!(cli.validate().is_ok());
}

#[test]
fn parses_contestants_and_duration_flags() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--profile",
        "mixed",
        "--contestants",
        "25",
        "--duration",
        "600",
    ])
    .unwrap();

    assert_eq!(cli.contestants, 25);
    assert_eq!(cli.duration, Some(600));
    assert!(cli.validate().is_ok());
}

#[test]
fn rejects_zero_final_burst_multiplier_when_burst_enabled() {
    let cli = Cli::try_parse_from([
        "broccoli-stress-test",
        "--url",
        "http://localhost:3000",
        "--admin-token",
        "abc",
        "--profile",
        "mixed",
        "--final-burst-duration",
        "60",
        "--final-burst-multiplier",
        "0",
    ])
    .unwrap();

    assert!(cli.validate().is_err());
}
