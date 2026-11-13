---
title: Judging performance
sidebar_label: Judging performance
sidebar_position: 3
---

# Judging performance

Two tools measure judging throughput and where each submission spends its time:

- `broccoli-stress-test perf` drives one or more running servers over HTTP and
  reports one point: throughput, latency, a per-stage breakdown, and Postgres,
  Redis and plugin-call counts per submission and per test case.
- `scripts/perf/judging-matrix.py` starts server and worker topologies from
  release binaries, runs `perf` over a grid of test-case counts and concurrency
  levels with repeats, samples CPU, and writes `summary.md`.

Use the mock sandbox (`worker.sandbox_backend = "mock"`) to measure platform
overhead alone. It runs the submission and the checker as plain host processes,
so `isolate` init, cleanup, cgroups and mounts are not in the numbers. Use
`isolate` to measure the full cost on a host that can run it.

## Run one point

Build the binaries, then point `perf` at a server that has the judging plugins
loaded and an admin account:

```bash
cargo build --release -p server -p worker -p stress-test -p broccoli-compare

target/release/broccoli-stress-test perf \
    --server-url http://127.0.0.1:3000 \
    --admin-username admin --admin-password '<password>' \
    --contest-type icpc --test-cases 10 \
    --submissions 200 --concurrency 32 \
    --database-url postgres://postgres:broccoli_pg_secret@127.0.0.1:5433/broccoli \
    --redis-url redis://127.0.0.1:6379 \
    --metrics-url http://127.0.0.1:3000/metrics,http://127.0.0.1:9091/metrics \
    --idle-secs 10 --out perf.json
```

`--concurrency` clients each keep one submission in flight: submit, then poll
`GET /api/v1/submissions/{id}` every `--poll-interval-ms` until it is terminal.
That makes the load closed loop. Throughput rises with concurrency until a
stage saturates, then latency rises instead. Sweep concurrency to find the
saturation point. A closed loop also caps throughput at one submission per
client per poll interval (`client_throughput.poll_cap_subs_per_s`); near that
cap, read the server-side `db_throughput` instead, or raise concurrency. Several `--server-url` values spread the submissions
round-robin.

`--database-url`, `--redis-url` and `--metrics-url` are optional. Each adds a
section to the report:

- `--database-url` reads stage timestamps of the run's own rows and resets
  `pg_stat_statements` before the run, then reads it after. The extension must
  be preloaded (`shared_preload_libraries = 'pg_stat_statements'`); the
  supplementary migration creates it.
- `--redis-url` diffs `INFO commandstats`.
- `--metrics-url` diffs every Prometheus counter and histogram on each URL.
  Give each worker on one host its own `worker.metrics_addr`.

The process exits 0 when every submission reaches the scenario's expected
verdict, 1 when one does not (a SystemError shows in `statuses`), and 2 on a
setup error.

## Run the matrix

The script expects Postgres and Redis to be running already, and the judging
plugins (`icpc`, `ioi`, `batch-evaluator`, `standard-checkers`,
`standard-languages`) built into `--plugins-dir`:

```bash
docker run -d --name bpg -p 127.0.0.1:5433:5432 \
    -e POSTGRES_PASSWORD=broccoli_pg_secret -e POSTGRES_DB=broccoli \
    postgres:17-alpine -c shared_preload_libraries=pg_stat_statements \
    -c max_connections=300
docker run -d --name bredis -p 127.0.0.1:6379:6379 redis:7-alpine

python3 scripts/perf/judging-matrix.py \
    --topologies 1x1,1x4,2x4 --test-cases 1,10,100 \
    --concurrency 1,64 --submissions 100 --repeats 3 \
    --fresh-db --flush-redis --idle-secs 10 \
    --out-dir perf-out/master
```

A topology `SxW` is S servers on ports 3000 and up and W workers with metrics
on ports 9100 and up, all on this host. `--fresh-db` recreates the database for
each topology. `--max-test-cases-per-point` caps the submissions in one point
so that 100-case points stay short.

Clients poll every `--poll-interval-ms` (50) at concurrency 1, where the point
measures latency, and every `--load-poll-interval-ms` (1000) above it. At 64
clients a 50 ms poll is 1280 requests per second, about 9 statements each,
which then dominates server and database CPU and hides the judging path.

To prove a bottleneck, move one knob and rerun the same point. Settings pass
through as environment variables:

```bash
python3 scripts/perf/judging-matrix.py --topologies 1x1 --test-cases 1 \
    --concurrency 64 --repeats 3 \
    --server-env BROCCOLI__SERVER__CLAIM_POLL_INTERVAL_MS=100 \
    --worker-env BROCCOLI__WORKER__MAX_CONCURRENCY=8 \
    --out-dir perf-out/claim-100ms-slots-8
```

With the mock sandbox the checker binary must exist at `/tools` on the host,
because the mock backend runs argv as is and does not remap `/tools/...`. The
script copies `broccoli-compare` there (`--tools-dir`).

## What the numbers mean

Client side (this process's clock):

| Field                   | Meaning                                                               |
| ----------------------- | --------------------------------------------------------------------- |
| `client_throughput`     | Measured submissions (and test cases) over the measured wall time.    |
| `submit_latency_ms`     | `POST .../submissions` round trip: auth, rate limit, hooks, insert.   |
| `e2e_latency_ms`        | POST to the first poll that sees a terminal status.                   |

Server side, from the run's own rows. Every timestamp is written by a server or
by Postgres, and none is compared with the client clock. On a multi-host run,
`created_at` and the test-result `created_at` come from the writing server's
clock while `leased_at` and `judged_at` come from `NOW()`, so keep server
clocks synchronised before trusting cross-stage numbers there.

| Stage             | From → to                                  | What it holds                                               |
| ----------------- | ------------------------------------------ | ----------------------------------------------------------- |
| `queued`          | `submission.created_at` → `leased_at`      | Waiting for the claim fiber's next tick and in-flight room. |
| `to_first_result` | `leased_at` → first `test_case_result`     | Plugin dispatch, operation publish, worker, result consume. |
| `between_results` | first → last `test_case_result`            | The remaining test cases.                                   |
| `finalize`        | last `test_case_result` → `judged_at`      | Aggregation and the verdict write.                          |
| `total`           | `created_at` → `judged_at`                 | Everything except the client's poll lag.                    |
| `db_throughput`   | judged rows over the DB-clock span         | `steady_subs_per_s` uses the 10th to 90th completions.      |

Overhead accounting:

- `db.statements_per_submission` and `per_test_case` count every statement in
  the database over the run, background fibers included. With `--idle-secs`,
  the no-load rate for the same window length is subtracted per statement.
- `client_polling` measures what one client poll costs (statements, Redis
  commands, plugin calls) by issuing `--calibrate-polls` GETs after the run,
  and reports the totals net of the client's own polling. Use the `net_*`
  fields for the judging path alone.
- `redis.by_command_per_submission` is the same for Redis commands. Idle
  polling loops (worker consume, server result poll) dominate this count at low
  load, so always pass `--idle-secs` when you read it.
- `metrics.histograms` gives, for each histogram series, calls per submission
  and mean, p50 and p95. Quantiles are interpolated inside buckets, so read
  them as bucket-resolution estimates. The ones that map to stages:
  `broccoli_task_queue_wait_duration_seconds` (operation waiting for a worker
  slot), `broccoli_task_process_duration_seconds` (one operation on a worker),
  `broccoli_step_duration_seconds` by `step_kind` (compile, test case,
  checker), `broccoli_operation_result_e2e_duration_seconds` (publish to result
  delivery), `broccoli_plugin_call_duration_seconds` by plugin and function, and
  `broccoli_submission_state_transition_duration_seconds`.

The matrix adds CPU per submission for each role (`server`, `worker` from
`/proc`, `postgres` and `redis` from their containers' cgroups) and the cores
each role used during the point.

## Reproducibility

`environment.json` in the output directory records `nproc`, memory, CPU model,
the git commit and every argument. Run at least three repeats per point;
`summary.md` prints the median and the min to max spread. Use release builds
only, and keep other load off the host while a matrix runs.
