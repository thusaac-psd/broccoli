# Judging pipeline benchmark, 2026-09-26

Two builds, identical scenarios, identical statistics:

- **baseline**: PR #23 head (`143db747`) plus metrics-only instrumentation.
- **perf**: the same plus the three `perf/judge-copying` commits (result-queue
  polling, no test data in the detached-eval session state, configurable poll
  interval, default 50 ms).

Both builds carry the same instrumentation commit (instance build time and
count), so every row below exists for both.

## Setup

- Real stack: 1 server, 4 workers with real `isolate` sandboxes (cgroups),
  Postgres 17, Redis 7, SeaweedFS. One 96-core host, otherwise idle.
- Standalone submissions judged by the **ICPC** contest plugin, a C++ echo
  program, and the fused `exact` checker. All answers are correct.
- Before each build's run, compiled-binary and worker blob caches are wiped and
  the server and workers are recreated. Test data is uploaded once and shared by
  both builds.
- Timings come from the server's own database timestamps: created, claimed
  (`leased_at`) and judged. Stage breakdowns come from Prometheus deltas on the
  server and all 4 workers. Memory and CPU are sampled once a second from
  `/proc` and cgroups.
- Workers use the default `max_concurrency = 1`, except in C2.

| Scenario | Load                                                             |
| -------- | ---------------------------------------------------------------- |
| S0       | 1 submission, 50 × 64 B cases, right after a server start (cold) |
| S1       | 3 submissions in sequence, 50 × 64 B                             |
| S2       | 3 submissions in sequence, 50 × 150 KB (inline test data)        |
| L1       | 1 submission, 20 × 50 MB (1 GB)                                  |
| L2       | 1 submission, 50 × 50 MB (2.5 GB)                                |
| C1       | 1000 submissions, 100 concurrent, 10 × 64 B each                 |
| C2       | C1 with 8 operation slots per worker (32 in total)               |

## Summary

|                                                            | baseline | perf    |                     |
| ---------------------------------------------------------- | -------- | ------- | ------------------- |
| S0 cold, per test case                                     | 505 ms   | 150 ms  | 3.4× faster         |
| S1 small, per test case                                    | 499 ms   | 143 ms  | 3.5× faster         |
| S2 150 KB, per test case                                   | 537 ms   | 168 ms  | 3.2× faster         |
| S2 ICPC plugin time (3 submissions)                        | 26.8 s   | 1.5 s   | 18× less            |
| S2 server CPU                                              | 42.7 s   | 5.3 s   | 8× less             |
| S2 server memory peak                                      | 957 MiB  | 675 MiB | −30%                |
| L1 1 GB, per test case                                     | 970 ms   | 592 ms  | 1.6× faster         |
| L2 2.5 GB, per test case                                   | 911 ms   | 590 ms  | 1.5× faster         |
| C1 1000 @ 100, 4 slots: wall                               | 155 s    | 155 s   | worker-bound, equal |
| C2 1000 @ 100, 32 slots: wall                              | 84 s     | 37 s    | 2.3× throughput     |
| C2 submission latency p50                                  | 72.7 s   | 20.9 s  | 3.5× faster         |
| SystemErrors, re-dispatches, pool failures (all scenarios) | 0        | 0       |                     |

**Nothing broke under load.** Both 1000-submission runs, on both builds, judged
1000/1000 Accepted with 0 SystemErrors, 0 hidden re-dispatches, 0 re-judges, 0
plugin-pool acquire failures and 0 rejected submit requests.

## Answers

**Where the per-case overhead went.** Worker execution is ~75–95 ms per small
case in both builds. On the baseline each small case took ~505 ms. The gap was
the server's result consumer napping 500 ms whenever the queue was empty: the
operation round trip p50 was 378 ms. Contest plugins judge one case at a time,
so this was paid per case. Perf brings the round trip p50 to 151 ms.

**Copying.** On the baseline, the detached-eval session state carried every test
body and the source, and it was copied into the host and back on every result.
With 150 KB cases, the ICPC plugin spent 26.8 s on 156 calls and the server
burned 42.7 s of CPU for 3 submissions. The state was also large enough to trip
the 128 MiB instance-recycle threshold on nearly every result: 30 ICPC instances
were rebuilt in 3 submissions. Perf: 1.5 s, 5.3 s, 0 rebuilds.

**Is a WASM instance started per submission? Is the pool big enough?**

- No instance is started per submission. Each plugin has one pool (at most 32
  instances by default, or `evaluator_parallelism` if higher), grown lazily and
  reused.
- A busy pool makes callers wait (up to 300 s, with retries on the hot path),
  never fail fast. Pool acquire failures were 0 in every run.
- The acquire wait was a few milliseconds per call even at 100 concurrent
  submissions: p99 of 20–23 ms, about 0.3–0.5 ms per call on average.
- Pools grew to 98–161 live instances across all plugins under the concurrency
  runs, well below the caps.

**Cost of starting a plugin instance.** Each build compiles and instantiates the
module. It took **10–13 ms** per instance here because wasmtime's on-disk
compile cache hits. With 50 MB test cases the batch evaluator was **never**
recycled: large test data moves by blob hash on the worker and never streams
through the evaluator plugin. The only recycle storm measured was the baseline's
bloated ICPC session state, which perf removes.

**Memory per WASM instance.** Measured as idle server RSS growth divided by
extra live instances: **~7 MiB per live instance** (7.2 MiB baseline, 7.0 MiB
perf). This is an upper bound, since RSS growth also includes other caches
warmed during the run. The configured hard cap per instance is 1.5 GiB of linear
memory. Idle server memory went from ~550 MiB right after start to 1.2–1.3 GiB
once the pools had grown under load.

**What actually limits throughput.**

1. **Worker slots (C1).** The default `worker.max_concurrency = 1` is
   deliberate, so timings stay fair. It makes 4 workers run 4 operations at
   once. At ~75 ms each that caps the stack at ~64 test cases/s, which is
   exactly what both builds reached. Operations queued behind the 4 slots
   (worker queue wait p50 ~0.7 s), and that shows up on the server as a 10.6 s
   average wait for a batch-evaluator fan-out permit. Throughput scales with the
   number of workers.
2. **Server overhead (C2).** With 32 slots the server became the limit, and
   perf's lower per-case overhead gave 2.3× throughput.
3. **Claim rate is not a limit.** The dispatcher claims 32 submissions per 1 s
   poll, so a 1000-submission burst waits ~15 s (p50) to be claimed. Raising the
   batch to 128 gave no speedup (C3: 37.3 s against 36.0 s), because the work
   only moves from the database queue into the server. The default stays 32. See
   the in-flight cap below.

**Trade-offs of the perf changes, measured.**

- Idle Redis load grows with the shorter result poll: ~490 extra ops/s per
  server at 50 ms. It is configurable as `mq.result_poll_interval_ms`; see
  `config.example.toml` for the measured curve.
- Redis commands per run moved both ways:
  - S2 −22%, C2 −6%, because fewer idle polls fit in a shorter run;
  - C1 +21%, L1 +31% and L2 +45%, because perf polls every 50 ms instead of 500
    ms.
- Removing test data from the plugin state changed no plugin behaviour. All
  plugin suites (556 tests) and the full workspace suite (1921 tests, including
  e2e) pass. The state shape is unchanged, so in-flight sessions survive a
  plugin upgrade.
- API change: judging policies now get `JudgeProgress::submission` (a
  `JudgedSubmission`) and `ScoringCase`s. Test data and source are not part of
  those types, so a policy cannot read them after judging starts: it is a
  compile error, not a silently empty list. All in-repo contest plugins compile
  unchanged apart from the field rename.

## Follow-up: in-flight cap (`server.max_in_flight_submissions`)

Before, a server claimed a batch every tick however much it was already judging,
and judging is detached after dispatch, so nothing bounded it. In C1 one server
held **979** submissions in judging at once. That puts the backlog in one
process's memory instead of the database, where any server can take it, and a
crash strands all of it until the lease expires.

With a cap of 256 (the new default), each tick claims at most
`min(claim_batch_size, cap - in_flight)`. Same stack, cold caches, same stats
(full tables in the **perf vs perf + cap** section below):

|                                                    | perf (no cap) | perf + cap 256 |
| -------------------------------------------------- | ------------- | -------------- |
| C1 (4 slots): wall                                 | 155.1 s       | 156.3 s        |
| C1: most submissions judging at once on the server | 979           | 256            |
| C1: server memory peak                             | 1570 MiB      | 1360 MiB       |
| C2 (32 slots): wall                                | 37.0 s        | 36.0 s         |
| C2: server memory peak                             | 1598 MiB      | 1525 MiB       |
| C3 (32 slots, batch 128): wall                     | —             | 37.3 s         |
| SystemErrors / re-dispatches                       | 0 / 0         | 0 / 0          |

## Full results

Every scenario, same rows for both builds. The change column is perf/baseline; ≈
means within 5%.

## S0-cold-small

1 submission(s), concurrency 1, 50 test cases × 64 bytes each.

|                                                          | baseline          | perf              | change |
| -------------------------------------------------------- | ----------------- | ----------------- | ------ |
| **Outcome**                                              |                   |                   |        |
| Submissions judged                                       | 1                 | 1                 | ≈      |
| Verdicts                                                 | Judged/Accepted 1 | Judged/Accepted 1 |        |
| SystemError (final)                                      | 0                 | 0                 |        |
| Re-dispatches (hidden retries)                           | 0                 | 0                 |        |
| Re-judged (epoch > 0)                                    | 0                 | 0                 |        |
| Submit request errors                                    | 0                 | 0                 |        |
| **Latency**                                              |                   |                   |        |
| Wall time (whole scenario)                               | 26.63 s           | 7.72 s            | 0.29×  |
| Submission latency p50 (created→judged)                  | 26.23 s           | 7.59 s            | 0.29×  |
| Submission latency p95                                   | 26.23 s           | 7.59 s            | 0.29×  |
| Submission latency max                                   | 26.23 s           | 7.59 s            | 0.29×  |
| Waiting to be claimed p50 (created→leased)               | 996 ms            | 104 ms            | 0.10×  |
| Waiting to be claimed p95                                | 996 ms            | 104 ms            | 0.10×  |
| Judging p50 (leased→judged)                              | 25.24 s           | 7.49 s            | 0.30×  |
| Judging per test case p50                                | 505 ms            | 150 ms            | 0.30×  |
| Throughput (submissions / s)                             | 0.04              | 0.13              | 3.45×  |
| **Plugin host (server)**                                 |                   |                   |        |
| Plugin calls                                             | 304               | 304               | ≈      |
| Plugin call time, total                                  | 922 ms            | 539 ms            | 0.58×  |
| Plugin call p95                                          | 29 ms             | 8 ms              | 0.27×  |
| icpc: calls / total time                                 | 52 / 669 ms       | 52 / 320 ms       |        |
| batch-evaluator: calls / total time                      | 100 / 172 ms      | 100 / 151 ms      |        |
| standard-checkers: calls / total time                    | 100 / 22 ms       | 100 / 21 ms       |        |
| standard-languages: calls / total time                   | 50 / 56 ms        | 50 / 43 ms        |        |
| Pool acquire wait, total                                 | 0 ms              | 0 ms              | ≈      |
| Pool acquire wait p99                                    | 0 ms              | 0 ms              | ≈      |
| Pool acquire failures                                    | 0                 | 0                 |        |
| Instances built (compile + instantiate)                  | 0                 | 0                 |        |
| Instance build time, total                               | 0 ms              | 0 ms              |        |
| Instance build time, mean                                | —                 | —                 |        |
| Instances recycled                                       | 0                 | 0                 |        |
| Evaluator semaphore wait, total                          | 0 ms              | 0 ms              | 0.81×  |
| Host function time, total                                | 922 ms            | 539 ms            | 0.58×  |
| **Queues**                                               |                   |                   |        |
| Operation round trip p50 (enqueue→result delivered)      | 378 ms            | 151 ms            | 0.40×  |
| Operation round trip p95                                 | 492 ms            | 240 ms            | 0.49×  |
| Worker queue wait p50                                    | 25 ms             | 25 ms             | ≈      |
| Worker queue wait p95                                    | 48 ms             | 48 ms             | ≈      |
| Message age at consume p95                               | 48 ms             | 48 ms             | ≈      |
| Redis commands                                           | 16,174            | 11,216            | 0.69×  |
| **Worker**                                               |                   |                   |        |
| Operations run                                           | 50                | 50                | ≈      |
| Operation processing p50                                 | 87 ms             | 95 ms             | 1.09×  |
| Operation processing p95                                 | 235 ms            | 233 ms            | ≈      |
| Step 'compile' mean                                      | 4 ms              | 4 ms              | 0.86×  |
| Step 'testcase' mean                                     | 25 ms             | 30 ms             | 1.18×  |
| Sandbox init mean                                        | 6 ms              | 4 ms              | 0.68×  |
| Sandbox cleanup mean                                     | 2 ms              | 2 ms              | ≈      |
| File materialization mean                                | 0 ms              | 0 ms              | 0.89×  |
| Blob cache hits / misses                                 | 46 / 3            | 46 / 3            |        |
| **Plugin host (server)**                                 |                   |                   |        |
| Live pooled instances (all plugins, since boot)          | 11                | 11                | ≈      |
| icpc: live / built / recycled (since boot)               | 1 / 1 / 0         | 1 / 1 / 0         |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | 1 / 1 / 0         |        |
| **Resources**                                            |                   |                   |        |
| Server RSS at start                                      | 563 MiB           | 561 MiB           | ≈      |
| Server RSS peak                                          | 564 MiB           | 561 MiB           | ≈      |
| Server RSS mean                                          | 550 MiB           | 554 MiB           | ≈      |
| Server RSS idle after the run                            | 549 MiB           | 554 MiB           | ≈      |
| Server container memory peak (incl. page cache)          | 579 MiB           | 570 MiB           | ≈      |
| Server CPU time                                          | 2.10 s            | 1.03 s            | 0.49×  |
| Workers CPU time (all 4)                                 | 2.40 s            | 1.54 s            | 0.64×  |

## S1-small

3 submission(s), concurrency 1, 50 test cases × 64 bytes each.

|                                                          | baseline          | perf              | change |
| -------------------------------------------------------- | ----------------- | ----------------- | ------ |
| **Outcome**                                              |                   |                   |        |
| Submissions judged                                       | 3                 | 3                 | ≈      |
| Verdicts                                                 | Judged/Accepted 3 | Judged/Accepted 3 |        |
| SystemError (final)                                      | 0                 | 0                 |        |
| Re-dispatches (hidden retries)                           | 0                 | 0                 |        |
| Re-judged (epoch > 0)                                    | 0                 | 0                 |        |
| Submit request errors                                    | 0                 | 0                 |        |
| **Latency**                                              |                   |                   |        |
| Wall time (whole scenario)                               | 77.20 s           | 24.50 s           | 0.32×  |
| Submission latency p50 (created→judged)                  | 25.73 s           | 7.95 s            | 0.31×  |
| Submission latency p95                                   | 25.93 s           | 8.19 s            | 0.32×  |
| Submission latency max                                   | 25.95 s           | 8.21 s            | 0.32×  |
| Waiting to be claimed p50 (created→leased)               | 855 ms            | 832 ms            | ≈      |
| Waiting to be claimed p95                                | 975 ms            | 908 ms            | 0.93×  |
| Judging p50 (leased→judged)                              | 24.96 s           | 7.16 s            | 0.29×  |
| Judging per test case p50                                | 499 ms            | 143 ms            | 0.29×  |
| Throughput (submissions / s)                             | 0.04              | 0.12              | 3.15×  |
| **Plugin host (server)**                                 |                   |                   |        |
| Plugin calls                                             | 912               | 912               | ≈      |
| Plugin call time, total                                  | 2.30 s            | 1.44 s            | 0.63×  |
| Plugin call p95                                          | 20 ms             | 5 ms              | 0.27×  |
| icpc: calls / total time                                 | 156 / 1.62 s      | 156 / 857 ms      |        |
| batch-evaluator: calls / total time                      | 300 / 466 ms      | 300 / 410 ms      |        |
| standard-checkers: calls / total time                    | 300 / 64 ms       | 300 / 55 ms       |        |
| standard-languages: calls / total time                   | 150 / 145 ms      | 150 / 119 ms      |        |
| Pool acquire wait, total                                 | 1 ms              | 1 ms              | 0.92×  |
| Pool acquire wait p99                                    | 0 ms              | 0 ms              | ≈      |
| Pool acquire failures                                    | 0                 | 0                 |        |
| Instances built (compile + instantiate)                  | 0                 | 0                 |        |
| Instance build time, total                               | 0 ms              | 0 ms              |        |
| Instance build time, mean                                | —                 | —                 |        |
| Instances recycled                                       | 0                 | 0                 |        |
| Evaluator semaphore wait, total                          | 0 ms              | 0 ms              | 0.67×  |
| Host function time, total                                | 2.30 s            | 1.44 s            | 0.63×  |
| **Queues**                                               |                   |                   |        |
| Operation round trip p50 (enqueue→result delivered)      | 375 ms            | 141 ms            | 0.38×  |
| Operation round trip p95                                 | 488 ms            | 240 ms            | 0.49×  |
| Worker queue wait p50                                    | 25 ms             | 25 ms             | ≈      |
| Worker queue wait p95                                    | 48 ms             | 48 ms             | ≈      |
| Message age at consume p95                               | 48 ms             | 48 ms             | ≈      |
| Redis commands                                           | 46,622            | 34,282            | 0.74×  |
| **Worker**                                               |                   |                   |        |
| Operations run                                           | 150               | 150               | ≈      |
| Operation processing p50                                 | 88 ms             | 90 ms             | ≈      |
| Operation processing p95                                 | 228 ms            | 233 ms            | ≈      |
| Step 'compile' mean                                      | 1 ms              | 1 ms              | 0.93×  |
| Step 'testcase' mean                                     | 26 ms             | 27 ms             | ≈      |
| Sandbox init mean                                        | 5 ms              | 5 ms              | ≈      |
| Sandbox cleanup mean                                     | 2 ms              | 2 ms              | 0.93×  |
| File materialization mean                                | 0 ms              | 0 ms              | 0.86×  |
| Blob cache hits / misses                                 | 150 / 0           | 150 / 0           |        |
| **Plugin host (server)**                                 |                   |                   |        |
| Live pooled instances (all plugins, since boot)          | 11                | 11                | ≈      |
| icpc: live / built / recycled (since boot)               | 1 / 1 / 0         | 1 / 1 / 0         |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | 1 / 1 / 0         |        |
| **Resources**                                            |                   |                   |        |
| Server RSS at start                                      | 549 MiB           | 554 MiB           | ≈      |
| Server RSS peak                                          | 554 MiB           | 555 MiB           | ≈      |
| Server RSS mean                                          | 551 MiB           | 547 MiB           | ≈      |
| Server RSS idle after the run                            | 552 MiB           | 547 MiB           | ≈      |
| Server container memory peak (incl. page cache)          | 574 MiB           | 560 MiB           | ≈      |
| Server CPU time                                          | 6.05 s            | 2.87 s            | 0.47×  |
| Workers CPU time (all 4)                                 | 6.75 s            | 4.51 s            | 0.67×  |

## S2-inline-150k

3 submission(s), concurrency 1, 50 test cases × 153,600 bytes each.

|                                                          | baseline          | perf              | change |
| -------------------------------------------------------- | ----------------- | ----------------- | ------ |
| **Outcome**                                              |                   |                   |        |
| Submissions judged                                       | 3                 | 3                 | ≈      |
| Verdicts                                                 | Judged/Accepted 3 | Judged/Accepted 3 |        |
| SystemError (final)                                      | 0                 | 0                 |        |
| Re-dispatches (hidden retries)                           | 0                 | 0                 |        |
| Re-judged (epoch > 0)                                    | 0                 | 0                 |        |
| Submit request errors                                    | 0                 | 0                 |        |
| **Latency**                                              |                   |                   |        |
| Wall time (whole scenario)                               | 81.55 s           | 27.34 s           | 0.34×  |
| Submission latency p50 (created→judged)                  | 27.41 s           | 8.88 s            | 0.32×  |
| Submission latency p95                                   | 27.52 s           | 9.06 s            | 0.33×  |
| Submission latency max                                   | 27.54 s           | 9.07 s            | 0.33×  |
| Waiting to be claimed p50 (created→leased)               | 447 ms            | 661 ms            | 1.48×  |
| Waiting to be claimed p95                                | 684 ms            | 744 ms            | 1.09×  |
| Judging p50 (leased→judged)                              | 26.83 s           | 8.41 s            | 0.31×  |
| Judging per test case p50                                | 537 ms            | 168 ms            | 0.31×  |
| Throughput (submissions / s)                             | 0.04              | 0.11              | 2.98×  |
| **Plugin host (server)**                                 |                   |                   |        |
| Plugin calls                                             | 912               | 912               | ≈      |
| Plugin call time, total                                  | 28.15 s           | 3.11 s            | 0.11×  |
| Plugin call p95                                          | 381 ms            | 29 ms             | 0.08×  |
| icpc: calls / total time                                 | 156 / 26.80 s     | 156 / 1.47 s      |        |
| batch-evaluator: calls / total time                      | 300 / 1.07 s      | 300 / 1.28 s      |        |
| standard-checkers: calls / total time                    | 300 / 155 ms      | 300 / 220 ms      |        |
| standard-languages: calls / total time                   | 150 / 131 ms      | 150 / 142 ms      |        |
| Pool acquire wait, total                                 | 1 ms              | 1 ms              | 0.86×  |
| Pool acquire wait p99                                    | 0 ms              | 0 ms              | ≈      |
| Pool acquire failures                                    | 0                 | 0                 |        |
| Instances built (compile + instantiate)                  | 30                | 0                 | 0.00×  |
| Instance build time, total                               | 309 ms            | 0 ms              | 0.00×  |
| Instance build time, mean                                | 10 ms             | —                 |        |
| Instances recycled                                       | 30                | 0                 | 0.00×  |
| Evaluator semaphore wait, total                          | 0 ms              | 0 ms              | 0.83×  |
| Host function time, total                                | 28.15 s           | 3.11 s            | 0.11×  |
| **Queues**                                               |                   |                   |        |
| Operation round trip p50 (enqueue→result delivered)      | 287 ms            | 157 ms            | 0.55×  |
| Operation round trip p95                                 | 495 ms            | 241 ms            | 0.49×  |
| Worker queue wait p50                                    | 25 ms             | 25 ms             | ≈      |
| Worker queue wait p95                                    | 48 ms             | 48 ms             | ≈      |
| Message age at consume p95                               | 48 ms             | 48 ms             | ≈      |
| Redis commands                                           | 48,653            | 38,165            | 0.78×  |
| **Worker**                                               |                   |                   |        |
| Operations run                                           | 150               | 150               | ≈      |
| Operation processing p50                                 | 94 ms             | 94 ms             | ≈      |
| Operation processing p95                                 | 233 ms            | 232 ms            | ≈      |
| Step 'compile' mean                                      | 1 ms              | 1 ms              | 0.95×  |
| Step 'testcase' mean                                     | 26 ms             | 25 ms             | ≈      |
| Sandbox init mean                                        | 5 ms              | 5 ms              | ≈      |
| Sandbox cleanup mean                                     | 2 ms              | 2 ms              | ≈      |
| File materialization mean                                | 1 ms              | 1 ms              | 0.93×  |
| Blob cache hits / misses                                 | 235 / 53          | 235 / 53          |        |
| **Plugin host (server)**                                 |                   |                   |        |
| Live pooled instances (all plugins, since boot)          | 11                | 11                | ≈      |
| icpc: live / built / recycled (since boot)               | 1 / 31 / 30       | 1 / 1 / 0         |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | 1 / 1 / 0         |        |
| **Resources**                                            |                   |                   |        |
| Server RSS at start                                      | 553 MiB           | 547 MiB           | ≈      |
| Server RSS peak                                          | 957 MiB           | 675 MiB           | 0.70×  |
| Server RSS mean                                          | 866 MiB           | 646 MiB           | 0.75×  |
| Server RSS idle after the run                            | 814 MiB           | 625 MiB           | 0.77×  |
| Server container memory peak (incl. page cache)          | 999 MiB           | 682 MiB           | 0.68×  |
| Server CPU time                                          | 42.67 s           | 5.28 s            | 0.12×  |
| Workers CPU time (all 4)                                 | 7.58 s            | 5.41 s            | 0.71×  |

## L1-50MBx20

1 submission(s), concurrency 1, 20 test cases × 52,428,800 bytes each.

|                                                          | baseline          | perf              | change |
| -------------------------------------------------------- | ----------------- | ----------------- | ------ |
| **Outcome**                                              |                   |                   |        |
| Submissions judged                                       | 1                 | 1                 | ≈      |
| Verdicts                                                 | Judged/Accepted 1 | Judged/Accepted 1 |        |
| SystemError (final)                                      | 0                 | 0                 |        |
| Re-dispatches (hidden retries)                           | 0                 | 0                 |        |
| Re-judged (epoch > 0)                                    | 0                 | 0                 |        |
| Submit request errors                                    | 0                 | 0                 |        |
| **Latency**                                              |                   |                   |        |
| Wall time (whole scenario)                               | 20.33 s           | 12.83 s           | 0.63×  |
| Submission latency p50 (created→judged)                  | 20.01 s           | 12.47 s           | 0.62×  |
| Submission latency p95                                   | 20.01 s           | 12.47 s           | 0.62×  |
| Submission latency max                                   | 20.01 s           | 12.47 s           | 0.62×  |
| Waiting to be claimed p50 (created→leased)               | 610 ms            | 638 ms            | ≈      |
| Waiting to be claimed p95                                | 610 ms            | 638 ms            | ≈      |
| Judging p50 (leased→judged)                              | 19.40 s           | 11.84 s           | 0.61×  |
| Judging per test case p50                                | 970 ms            | 592 ms            | 0.61×  |
| Throughput (submissions / s)                             | 0.05              | 0.08              | 1.58×  |
| **Plugin host (server)**                                 |                   |                   |        |
| Plugin calls                                             | 124               | 124               | ≈      |
| Plugin call time, total                                  | 267 ms            | 210 ms            | 0.79×  |
| Plugin call p95                                          | 9 ms              | 7 ms              | 0.75×  |
| icpc: calls / total time                                 | 22 / 163 ms       | 22 / 111 ms       |        |
| batch-evaluator: calls / total time                      | 40 / 76 ms        | 40 / 72 ms        |        |
| standard-checkers: calls / total time                    | 40 / 8 ms         | 40 / 9 ms         |        |
| standard-languages: calls / total time                   | 20 / 19 ms        | 20 / 17 ms        |        |
| Pool acquire wait, total                                 | 0 ms              | 0 ms              | 1.12×  |
| Pool acquire wait p99                                    | 0 ms              | 0 ms              | ≈      |
| Pool acquire failures                                    | 0                 | 0                 |        |
| Instances built (compile + instantiate)                  | 0                 | 0                 |        |
| Instance build time, total                               | 0 ms              | 0 ms              |        |
| Instance build time, mean                                | —                 | —                 |        |
| Instances recycled                                       | 0                 | 0                 |        |
| Evaluator semaphore wait, total                          | 0 ms              | 0 ms              | 0.88×  |
| Host function time, total                                | 267 ms            | 210 ms            | 0.79×  |
| **Queues**                                               |                   |                   |        |
| Operation round trip p50 (enqueue→result delivered)      | 737 ms            | 688 ms            | 0.93×  |
| Operation round trip p95                                 | 974 ms            | 969 ms            | ≈      |
| Worker queue wait p50                                    | 25 ms             | 25 ms             | ≈      |
| Worker queue wait p95                                    | 48 ms             | 48 ms             | ≈      |
| Message age at consume p95                               | 48 ms             | 48 ms             | ≈      |
| Redis commands                                           | 10,999            | 14,413            | 1.31×  |
| **Worker**                                               |                   |                   |        |
| Operations run                                           | 20                | 20                | ≈      |
| Operation processing p50                                 | 706 ms            | 615 ms            | 0.87×  |
| Operation processing p95                                 | 971 ms            | 962 ms            | ≈      |
| Step 'compile' mean                                      | 1 ms              | 1 ms              | ≈      |
| Step 'testcase' mean                                     | 75 ms             | 75 ms             | ≈      |
| Sandbox init mean                                        | 8 ms              | 8 ms              | 1.09×  |
| Sandbox cleanup mean                                     | 6 ms              | 5 ms              | 0.72×  |
| File materialization mean                                | 142 ms            | 128 ms            | 0.90×  |
| Blob cache hits / misses                                 | 40 / 20           | 40 / 20           |        |
| **Plugin host (server)**                                 |                   |                   |        |
| Live pooled instances (all plugins, since boot)          | 11                | 11                | ≈      |
| icpc: live / built / recycled (since boot)               | 1 / 31 / 30       | 1 / 1 / 0         |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | 1 / 1 / 0         |        |
| **Resources**                                            |                   |                   |        |
| Server RSS at start                                      | 798 MiB           | 624 MiB           | 0.78×  |
| Server RSS peak                                          | 798 MiB           | 624 MiB           | 0.78×  |
| Server RSS mean                                          | 749 MiB           | 617 MiB           | 0.82×  |
| Server RSS idle after the run                            | 708 MiB           | 614 MiB           | 0.87×  |
| Server container memory peak (incl. page cache)          | 838 MiB           | 638 MiB           | 0.76×  |
| Server CPU time                                          | 1.22 s            | 925 ms            | 0.76×  |
| Workers CPU time (all 4)                                 | 11.86 s           | 10.86 s           | 0.92×  |

## L2-50MBx50

1 submission(s), concurrency 1, 50 test cases × 52,428,800 bytes each.

|                                                          | baseline          | perf              | change |
| -------------------------------------------------------- | ----------------- | ----------------- | ------ |
| **Outcome**                                              |                   |                   |        |
| Submissions judged                                       | 1                 | 1                 | ≈      |
| Verdicts                                                 | Judged/Accepted 1 | Judged/Accepted 1 |        |
| SystemError (final)                                      | 0                 | 0                 |        |
| Re-dispatches (hidden retries)                           | 0                 | 0                 |        |
| Re-judged (epoch > 0)                                    | 0                 | 0                 |        |
| Submit request errors                                    | 0                 | 0                 |        |
| **Latency**                                              |                   |                   |        |
| Wall time (whole scenario)                               | 46.21 s           | 30.39 s           | 0.66×  |
| Submission latency p50 (created→judged)                  | 46.04 s           | 30.01 s           | 0.65×  |
| Submission latency p95                                   | 46.04 s           | 30.01 s           | 0.65×  |
| Submission latency max                                   | 46.04 s           | 30.01 s           | 0.65×  |
| Waiting to be claimed p50 (created→leased)               | 493 ms            | 526 ms            | 1.07×  |
| Waiting to be claimed p95                                | 493 ms            | 526 ms            | 1.07×  |
| Judging p50 (leased→judged)                              | 45.54 s           | 29.48 s           | 0.65×  |
| Judging per test case p50                                | 911 ms            | 590 ms            | 0.65×  |
| Throughput (submissions / s)                             | 0.02              | 0.03              | 1.52×  |
| **Plugin host (server)**                                 |                   |                   |        |
| Plugin calls                                             | 304               | 304               | ≈      |
| Plugin call time, total                                  | 936 ms            | 539 ms            | 0.58×  |
| Plugin call p95                                          | 28 ms             | 8 ms              | 0.28×  |
| icpc: calls / total time                                 | 52 / 665 ms       | 52 / 325 ms       |        |
| batch-evaluator: calls / total time                      | 100 / 201 ms      | 100 / 159 ms      |        |
| standard-checkers: calls / total time                    | 100 / 21 ms       | 100 / 17 ms       |        |
| standard-languages: calls / total time                   | 50 / 49 ms        | 50 / 37 ms        |        |
| Pool acquire wait, total                                 | 0 ms              | 0 ms              | 0.93×  |
| Pool acquire wait p99                                    | 0 ms              | 0 ms              | ≈      |
| Pool acquire failures                                    | 0                 | 0                 |        |
| Instances built (compile + instantiate)                  | 0                 | 0                 |        |
| Instance build time, total                               | 0 ms              | 0 ms              |        |
| Instance build time, mean                                | —                 | —                 |        |
| Instances recycled                                       | 0                 | 0                 |        |
| Evaluator semaphore wait, total                          | 0 ms              | 0 ms              | 0.71×  |
| Host function time, total                                | 936 ms            | 539 ms            | 0.58×  |
| **Queues**                                               |                   |                   |        |
| Operation round trip p50 (enqueue→result delivered)      | 695 ms            | 709 ms            | ≈      |
| Operation round trip p95                                 | 970 ms            | 971 ms            | ≈      |
| Worker queue wait p50                                    | 25 ms             | 25 ms             | ≈      |
| Worker queue wait p95                                    | 48 ms             | 48 ms             | ≈      |
| Message age at consume p95                               | 48 ms             | 48 ms             | ≈      |
| Redis commands                                           | 24,314            | 35,332            | 1.45×  |
| **Worker**                                               |                   |                   |        |
| Operations run                                           | 50                | 50                | ≈      |
| Operation processing p50                                 | 621 ms            | 609 ms            | ≈      |
| Operation processing p95                                 | 962 ms            | 961 ms            | ≈      |
| Step 'compile' mean                                      | 1 ms              | 1 ms              | ≈      |
| Step 'testcase' mean                                     | 67 ms             | 69 ms             | ≈      |
| Sandbox init mean                                        | 5 ms              | 8 ms              | 1.83×  |
| Sandbox cleanup mean                                     | 3 ms              | 3 ms              | ≈      |
| File materialization mean                                | 136 ms            | 132 ms            | ≈      |
| Blob cache hits / misses                                 | 100 / 50          | 100 / 50          |        |
| **Plugin host (server)**                                 |                   |                   |        |
| Live pooled instances (all plugins, since boot)          | 11                | 11                | ≈      |
| icpc: live / built / recycled (since boot)               | 1 / 31 / 30       | 1 / 1 / 0         |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | 1 / 1 / 0         |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | 1 / 1 / 0         |        |
| **Resources**                                            |                   |                   |        |
| Server RSS at start                                      | 708 MiB           | 614 MiB           | 0.87×  |
| Server RSS peak                                          | 708 MiB           | 615 MiB           | 0.87×  |
| Server RSS mean                                          | 696 MiB           | 614 MiB           | 0.88×  |
| Server RSS idle after the run                            | 689 MiB           | 613 MiB           | 0.89×  |
| Server container memory peak (incl. page cache)          | 748 MiB           | 629 MiB           | 0.84×  |
| Server CPU time                                          | 3.04 s            | 2.28 s            | 0.75×  |
| Workers CPU time (all 4)                                 | 28.53 s           | 27.29 s           | ≈      |

## C1-1000@100

1000 submission(s), concurrency 100, 10 test cases × 64 bytes each.

|                                                          | baseline             | perf                 | change |
| -------------------------------------------------------- | -------------------- | -------------------- | ------ |
| **Outcome**                                              |                      |                      |        |
| Submissions judged                                       | 1,000                | 1,000                | ≈      |
| Verdicts                                                 | Judged/Accepted 1000 | Judged/Accepted 1000 |        |
| SystemError (final)                                      | 0                    | 0                    |        |
| Re-dispatches (hidden retries)                           | 0                    | 0                    |        |
| Re-judged (epoch > 0)                                    | 0                    | 0                    |        |
| Submit request errors                                    | 0                    | 0                    |        |
| **Latency**                                              |                      |                      |        |
| Wall time (whole scenario)                               | 155.04 s             | 155.09 s             | ≈      |
| Submission latency p50 (created→judged)                  | 142.53 s             | 141.94 s             | ≈      |
| Submission latency p95                                   | 152.83 s             | 152.51 s             | ≈      |
| Submission latency max                                   | 153.79 s             | 153.19 s             | ≈      |
| Waiting to be claimed p50 (created→leased)               | 15.17 s              | 14.84 s              | ≈      |
| Waiting to be claimed p95                                | 28.68 s              | 28.36 s              | ≈      |
| Judging p50 (leased→judged)                              | 124.73 s             | 124.78 s             | ≈      |
| Judging per test case p50                                | 12.47 s              | 12.48 s              | ≈      |
| Throughput (submissions / s)                             | 6.45                 | 6.45                 | ≈      |
| **Plugin host (server)**                                 |                      |                      |        |
| Plugin calls                                             | 64,000               | 64,000               | ≈      |
| Plugin call time, total                                  | 195.16 s             | 123.52 s             | 0.63×  |
| Plugin call p95                                          | 30 ms                | 5 ms                 | 0.16×  |
| icpc: calls / total time                                 | 12,000 / 57.15 s     | 12,000 / 47.61 s     |        |
| batch-evaluator: calls / total time                      | 20,000 / 114.23 s    | 20,000 / 36.50 s     |        |
| standard-checkers: calls / total time                    | 20,000 / 2.73 s      | 20,000 / 2.90 s      |        |
| standard-languages: calls / total time                   | 10,000 / 7.30 s      | 10,000 / 7.87 s      |        |
| Pool acquire wait, total                                 | 31.14 s              | 35.12 s              | 1.13×  |
| Pool acquire wait p99                                    | 23 ms                | 20 ms                | 0.89×  |
| Pool acquire failures                                    | 0                    | 0                    |        |
| Instances built (compile + instantiate)                  | 110                  | 87                   | 0.79×  |
| Instance build time, total                               | 1.47 s               | 1.14 s               | 0.77×  |
| Instance build time, mean                                | 13 ms                | 13 ms                | ≈      |
| Instances recycled                                       | 0                    | 0                    |        |
| Evaluator semaphore wait, total                          | 5 ms                 | 5 ms                 | ≈      |
| Host function time, total                                | 195.16 s             | 123.52 s             | 0.63×  |
| **Queues**                                               |                      |                      |        |
| Operation round trip p50 (enqueue→result delivered)      | 1.64 s               | 821 ms               | 0.50×  |
| Operation round trip p95                                 | 2.41 s               | 2.17 s               | 0.90×  |
| Worker queue wait p50                                    | 695 ms               | 753 ms               | 1.08×  |
| Worker queue wait p95                                    | 971 ms               | 984 ms               | ≈      |
| Message age at consume p95                               | 971 ms               | 984 ms               | ≈      |
| Redis commands                                           | 457,291              | 554,497              | 1.21×  |
| **Worker**                                               |                      |                      |        |
| Operations run                                           | 10,000               | 10,000               | ≈      |
| Operation processing p50                                 | 75 ms                | 75 ms                | ≈      |
| Operation processing p95                                 | 98 ms                | 98 ms                | ≈      |
| Step 'compile' mean                                      | 1 ms                 | 1 ms                 | ≈      |
| Step 'testcase' mean                                     | 6 ms                 | 6 ms                 | ≈      |
| Sandbox init mean                                        | 2 ms                 | 2 ms                 | ≈      |
| Sandbox cleanup mean                                     | 1 ms                 | 1 ms                 | ≈      |
| File materialization mean                                | 0 ms                 | 0 ms                 | ≈      |
| Blob cache hits / misses                                 | 10,000 / 0           | 10,000 / 0           |        |
| **Plugin host (server)**                                 |                      |                      |        |
| Live pooled instances (all plugins, since boot)          | 121                  | 98                   | 0.81×  |
| icpc: live / built / recycled (since boot)               | 34 / 64 / 30         | 34 / 34 / 0          |        |
| batch-evaluator: live / built / recycled (since boot)    | 35 / 35 / 0          | 13 / 13 / 0          |        |
| standard-checkers: live / built / recycled (since boot)  | 8 / 8 / 0            | 4 / 4 / 0            |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0            | 1 / 1 / 0            |        |
| **Resources**                                            |                      |                      |        |
| Server RSS at start                                      | 692 MiB              | 617 MiB              | 0.89×  |
| Server RSS peak                                          | 1765 MiB             | 1570 MiB             | 0.89×  |
| Server RSS mean                                          | 1459 MiB             | 1298 MiB             | 0.89×  |
| Server RSS idle after the run                            | 1339 MiB             | 1166 MiB             | 0.87×  |
| Server container memory peak (incl. page cache)          | 1821 MiB             | 1605 MiB             | 0.88×  |
| Server CPU time                                          | 100.74 s             | 90.27 s              | 0.90×  |
| Workers CPU time (all 4)                                 | 198.04 s             | 197.30 s             | ≈      |

## C2-1000@100-32slots

1000 submission(s), concurrency 100, 10 test cases × 64 bytes each.

|                                                          | baseline             | perf                 | change |
| -------------------------------------------------------- | -------------------- | -------------------- | ------ |
| **Outcome**                                              |                      |                      |        |
| Submissions judged                                       | 1,000                | 1,000                | ≈      |
| Verdicts                                                 | Judged/Accepted 1000 | Judged/Accepted 1000 |        |
| SystemError (final)                                      | 0                    | 0                    |        |
| Re-dispatches (hidden retries)                           | 0                    | 0                    |        |
| Re-judged (epoch > 0)                                    | 0                    | 0                    |        |
| Submit request errors                                    | 0                    | 0                    |        |
| **Latency**                                              |                      |                      |        |
| Wall time (whole scenario)                               | 84.13 s              | 37.05 s              | 0.44×  |
| Submission latency p50 (created→judged)                  | 72.68 s              | 20.87 s              | 0.29×  |
| Submission latency p95                                   | 81.81 s              | 35.05 s              | 0.43×  |
| Submission latency max                                   | 82.33 s              | 35.32 s              | 0.43×  |
| Waiting to be claimed p50 (created→leased)               | 14.71 s              | 14.95 s              | ≈      |
| Waiting to be claimed p95                                | 28.22 s              | 28.47 s              | ≈      |
| Judging p50 (leased→judged)                              | 55.07 s              | 5.75 s               | 0.10×  |
| Judging per test case p50                                | 5.51 s               | 575 ms               | 0.10×  |
| Throughput (submissions / s)                             | 11.89                | 26.99                | 2.27×  |
| **Plugin host (server)**                                 |                      |                      |        |
| Plugin calls                                             | 64,000               | 64,000               | ≈      |
| Plugin call time, total                                  | 341.64 s             | 261.04 s             | 0.76×  |
| Plugin call p95                                          | 38 ms                | 30 ms                | 0.77×  |
| icpc: calls / total time                                 | 12,000 / 74.89 s     | 12,000 / 94.05 s     |        |
| batch-evaluator: calls / total time                      | 20,000 / 241.01 s    | 20,000 / 142.13 s    |        |
| standard-checkers: calls / total time                    | 20,000 / 4.69 s      | 20,000 / 2.88 s      |        |
| standard-languages: calls / total time                   | 10,000 / 8.70 s      | 10,000 / 9.00 s      |        |
| Pool acquire wait, total                                 | 47.22 s              | 39.56 s              | 0.84×  |
| Pool acquire wait p99                                    | 36 ms                | 30 ms                | 0.85×  |
| Pool acquire failures                                    | 0                    | 0                    |        |
| Instances built (compile + instantiate)                  | 146                  | 150                  | ≈      |
| Instance build time, total                               | 2.04 s               | 1.93 s               | 0.94×  |
| Instance build time, mean                                | 14 ms                | 13 ms                | 0.92×  |
| Instances recycled                                       | 0                    | 0                    |        |
| Evaluator semaphore wait, total                          | 7 ms                 | 5 ms                 | 0.76×  |
| Host function time, total                                | 341.64 s             | 261.04 s             | 0.76×  |
| **Queues**                                               |                      |                      |        |
| Operation round trip p50 (enqueue→result delivered)      | 655 ms               | 196 ms               | 0.30×  |
| Operation round trip p95                                 | 966 ms               | 444 ms               | 0.46×  |
| Worker queue wait p50                                    | 48 ms                | 75 ms                | 1.57×  |
| Worker queue wait p95                                    | 220 ms               | 200 ms               | 0.91×  |
| Message age at consume p95                               | 220 ms               | 200 ms               | 0.91×  |
| Redis commands                                           | 506,211              | 474,799              | 0.94×  |
| **Worker**                                               |                      |                      |        |
| Operations run                                           | 10,000               | 10,000               | ≈      |
| Operation processing p50                                 | 93 ms                | 120 ms               | 1.28×  |
| Operation processing p95                                 | 233 ms               | 238 ms               | ≈      |
| Step 'compile' mean                                      | 1 ms                 | 1 ms                 | ≈      |
| Step 'testcase' mean                                     | 13 ms                | 17 ms                | 1.26×  |
| Sandbox init mean                                        | 10 ms                | 9 ms                 | 0.90×  |
| Sandbox cleanup mean                                     | 5 ms                 | 9 ms                 | 1.90×  |
| File materialization mean                                | 0 ms                 | 0 ms                 | ≈      |
| Blob cache hits / misses                                 | 9,996 / 3            | 9,996 / 3            |        |
| **Plugin host (server)**                                 |                      |                      |        |
| Live pooled instances (all plugins, since boot)          | 157                  | 161                  | ≈      |
| icpc: live / built / recycled (since boot)               | 43 / 43 / 0          | 50 / 50 / 0          |        |
| batch-evaluator: live / built / recycled (since boot)    | 61 / 61 / 0          | 60 / 60 / 0          |        |
| standard-checkers: live / built / recycled (since boot)  | 13 / 13 / 0          | 8 / 8 / 0            |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0            | 1 / 1 / 0            |        |
| **Resources**                                            |                      |                      |        |
| Server RSS at start                                      | 544 MiB              | 571 MiB              | ≈      |
| Server RSS peak                                          | 1719 MiB             | 1598 MiB             | 0.93×  |
| Server RSS mean                                          | 1398 MiB             | 1378 MiB             | ≈      |
| Server RSS idle after the run                            | 1359 MiB             | 1488 MiB             | 1.10×  |
| Server container memory peak (incl. page cache)          | 1733 MiB             | 1602 MiB             | 0.92×  |
| Server CPU time                                          | 103.34 s             | 63.49 s              | 0.61×  |
| Workers CPU time (all 4)                                 | 263.27 s             | 277.34 s             | 1.05×  |

# perf vs perf + cap

## S0-cold-small

1 submission(s), concurrency 1, 50 test cases × 64 bytes each.

|                                                          | perf              | perfcap | change |
| -------------------------------------------------------- | ----------------- | ------- | ------ |
| **Outcome**                                              |                   |         |        |
| Submissions judged                                       | 1                 | —       |        |
| Verdicts                                                 | Judged/Accepted 1 | —       |        |
| SystemError (final)                                      | 0                 | —       |        |
| Re-dispatches (hidden retries)                           | 0                 | —       |        |
| Re-judged (epoch > 0)                                    | 0                 | —       |        |
| Submit request errors                                    | 0                 | —       |        |
| **Latency**                                              |                   |         |        |
| Wall time (whole scenario)                               | 7.72 s            | —       |        |
| Submission latency p50 (created→judged)                  | 7.59 s            | —       |        |
| Submission latency p95                                   | 7.59 s            | —       |        |
| Submission latency max                                   | 7.59 s            | —       |        |
| Waiting to be claimed p50 (created→leased)               | 104 ms            | —       |        |
| Waiting to be claimed p95                                | 104 ms            | —       |        |
| Judging p50 (leased→judged)                              | 7.49 s            | —       |        |
| Judging per test case p50                                | 150 ms            | —       |        |
| Throughput (submissions / s)                             | 0.13              | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Plugin calls                                             | 304               | —       |        |
| Plugin call time, total                                  | 539 ms            | —       |        |
| Plugin call p95                                          | 8 ms              | —       |        |
| icpc: calls / total time                                 | 52 / 320 ms       | —       |        |
| batch-evaluator: calls / total time                      | 100 / 151 ms      | —       |        |
| standard-checkers: calls / total time                    | 100 / 21 ms       | —       |        |
| standard-languages: calls / total time                   | 50 / 43 ms        | —       |        |
| Pool acquire wait, total                                 | 0 ms              | —       |        |
| Pool acquire wait p99                                    | 0 ms              | —       |        |
| Pool acquire failures                                    | 0                 | —       |        |
| Instances built (compile + instantiate)                  | 0                 | —       |        |
| Instance build time, total                               | 0 ms              | —       |        |
| Instance build time, mean                                | —                 | —       |        |
| Instances recycled                                       | 0                 | —       |        |
| Evaluator semaphore wait, total                          | 0 ms              | —       |        |
| Host function time, total                                | 539 ms            | —       |        |
| **Queues**                                               |                   |         |        |
| Operation round trip p50 (enqueue→result delivered)      | 151 ms            | —       |        |
| Operation round trip p95                                 | 240 ms            | —       |        |
| Worker queue wait p50                                    | 25 ms             | —       |        |
| Worker queue wait p95                                    | 48 ms             | —       |        |
| Message age at consume p95                               | 48 ms             | —       |        |
| Redis commands                                           | 11,216            | —       |        |
| **Worker**                                               |                   |         |        |
| Operations run                                           | 50                | —       |        |
| Operation processing p50                                 | 95 ms             | —       |        |
| Operation processing p95                                 | 233 ms            | —       |        |
| Step 'compile' mean                                      | 4 ms              | —       |        |
| Step 'testcase' mean                                     | 30 ms             | —       |        |
| Sandbox init mean                                        | 4 ms              | —       |        |
| Sandbox cleanup mean                                     | 2 ms              | —       |        |
| File materialization mean                                | 0 ms              | —       |        |
| Blob cache hits / misses                                 | 46 / 3            | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Live pooled instances (all plugins, since boot)          | 11                | —       |        |
| icpc: live / built / recycled (since boot)               | 1 / 1 / 0         | —       |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | —       |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | —       |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | —       |        |
| **Resources**                                            |                   |         |        |
| Server RSS at start                                      | 561 MiB           | —       |        |
| Server RSS peak                                          | 561 MiB           | —       |        |
| Server RSS mean                                          | 554 MiB           | —       |        |
| Server RSS idle after the run                            | 554 MiB           | —       |        |
| Server container memory peak (incl. page cache)          | 570 MiB           | —       |        |
| Server CPU time                                          | 1.03 s            | —       |        |
| Workers CPU time (all 4)                                 | 1.54 s            | —       |        |

## S1-small

3 submission(s), concurrency 1, 50 test cases × 64 bytes each.

|                                                          | perf              | perfcap | change |
| -------------------------------------------------------- | ----------------- | ------- | ------ |
| **Outcome**                                              |                   |         |        |
| Submissions judged                                       | 3                 | —       |        |
| Verdicts                                                 | Judged/Accepted 3 | —       |        |
| SystemError (final)                                      | 0                 | —       |        |
| Re-dispatches (hidden retries)                           | 0                 | —       |        |
| Re-judged (epoch > 0)                                    | 0                 | —       |        |
| Submit request errors                                    | 0                 | —       |        |
| **Latency**                                              |                   |         |        |
| Wall time (whole scenario)                               | 24.50 s           | —       |        |
| Submission latency p50 (created→judged)                  | 7.95 s            | —       |        |
| Submission latency p95                                   | 8.19 s            | —       |        |
| Submission latency max                                   | 8.21 s            | —       |        |
| Waiting to be claimed p50 (created→leased)               | 832 ms            | —       |        |
| Waiting to be claimed p95                                | 908 ms            | —       |        |
| Judging p50 (leased→judged)                              | 7.16 s            | —       |        |
| Judging per test case p50                                | 143 ms            | —       |        |
| Throughput (submissions / s)                             | 0.12              | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Plugin calls                                             | 912               | —       |        |
| Plugin call time, total                                  | 1.44 s            | —       |        |
| Plugin call p95                                          | 5 ms              | —       |        |
| icpc: calls / total time                                 | 156 / 857 ms      | —       |        |
| batch-evaluator: calls / total time                      | 300 / 410 ms      | —       |        |
| standard-checkers: calls / total time                    | 300 / 55 ms       | —       |        |
| standard-languages: calls / total time                   | 150 / 119 ms      | —       |        |
| Pool acquire wait, total                                 | 1 ms              | —       |        |
| Pool acquire wait p99                                    | 0 ms              | —       |        |
| Pool acquire failures                                    | 0                 | —       |        |
| Instances built (compile + instantiate)                  | 0                 | —       |        |
| Instance build time, total                               | 0 ms              | —       |        |
| Instance build time, mean                                | —                 | —       |        |
| Instances recycled                                       | 0                 | —       |        |
| Evaluator semaphore wait, total                          | 0 ms              | —       |        |
| Host function time, total                                | 1.44 s            | —       |        |
| **Queues**                                               |                   |         |        |
| Operation round trip p50 (enqueue→result delivered)      | 141 ms            | —       |        |
| Operation round trip p95                                 | 240 ms            | —       |        |
| Worker queue wait p50                                    | 25 ms             | —       |        |
| Worker queue wait p95                                    | 48 ms             | —       |        |
| Message age at consume p95                               | 48 ms             | —       |        |
| Redis commands                                           | 34,282            | —       |        |
| **Worker**                                               |                   |         |        |
| Operations run                                           | 150               | —       |        |
| Operation processing p50                                 | 90 ms             | —       |        |
| Operation processing p95                                 | 233 ms            | —       |        |
| Step 'compile' mean                                      | 1 ms              | —       |        |
| Step 'testcase' mean                                     | 27 ms             | —       |        |
| Sandbox init mean                                        | 5 ms              | —       |        |
| Sandbox cleanup mean                                     | 2 ms              | —       |        |
| File materialization mean                                | 0 ms              | —       |        |
| Blob cache hits / misses                                 | 150 / 0           | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Live pooled instances (all plugins, since boot)          | 11                | —       |        |
| icpc: live / built / recycled (since boot)               | 1 / 1 / 0         | —       |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | —       |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | —       |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | —       |        |
| **Resources**                                            |                   |         |        |
| Server RSS at start                                      | 554 MiB           | —       |        |
| Server RSS peak                                          | 555 MiB           | —       |        |
| Server RSS mean                                          | 547 MiB           | —       |        |
| Server RSS idle after the run                            | 547 MiB           | —       |        |
| Server container memory peak (incl. page cache)          | 560 MiB           | —       |        |
| Server CPU time                                          | 2.87 s            | —       |        |
| Workers CPU time (all 4)                                 | 4.51 s            | —       |        |

## S2-inline-150k

3 submission(s), concurrency 1, 50 test cases × 153,600 bytes each.

|                                                          | perf              | perfcap | change |
| -------------------------------------------------------- | ----------------- | ------- | ------ |
| **Outcome**                                              |                   |         |        |
| Submissions judged                                       | 3                 | —       |        |
| Verdicts                                                 | Judged/Accepted 3 | —       |        |
| SystemError (final)                                      | 0                 | —       |        |
| Re-dispatches (hidden retries)                           | 0                 | —       |        |
| Re-judged (epoch > 0)                                    | 0                 | —       |        |
| Submit request errors                                    | 0                 | —       |        |
| **Latency**                                              |                   |         |        |
| Wall time (whole scenario)                               | 27.34 s           | —       |        |
| Submission latency p50 (created→judged)                  | 8.88 s            | —       |        |
| Submission latency p95                                   | 9.06 s            | —       |        |
| Submission latency max                                   | 9.07 s            | —       |        |
| Waiting to be claimed p50 (created→leased)               | 661 ms            | —       |        |
| Waiting to be claimed p95                                | 744 ms            | —       |        |
| Judging p50 (leased→judged)                              | 8.41 s            | —       |        |
| Judging per test case p50                                | 168 ms            | —       |        |
| Throughput (submissions / s)                             | 0.11              | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Plugin calls                                             | 912               | —       |        |
| Plugin call time, total                                  | 3.11 s            | —       |        |
| Plugin call p95                                          | 29 ms             | —       |        |
| icpc: calls / total time                                 | 156 / 1.47 s      | —       |        |
| batch-evaluator: calls / total time                      | 300 / 1.28 s      | —       |        |
| standard-checkers: calls / total time                    | 300 / 220 ms      | —       |        |
| standard-languages: calls / total time                   | 150 / 142 ms      | —       |        |
| Pool acquire wait, total                                 | 1 ms              | —       |        |
| Pool acquire wait p99                                    | 0 ms              | —       |        |
| Pool acquire failures                                    | 0                 | —       |        |
| Instances built (compile + instantiate)                  | 0                 | —       |        |
| Instance build time, total                               | 0 ms              | —       |        |
| Instance build time, mean                                | —                 | —       |        |
| Instances recycled                                       | 0                 | —       |        |
| Evaluator semaphore wait, total                          | 0 ms              | —       |        |
| Host function time, total                                | 3.11 s            | —       |        |
| **Queues**                                               |                   |         |        |
| Operation round trip p50 (enqueue→result delivered)      | 157 ms            | —       |        |
| Operation round trip p95                                 | 241 ms            | —       |        |
| Worker queue wait p50                                    | 25 ms             | —       |        |
| Worker queue wait p95                                    | 48 ms             | —       |        |
| Message age at consume p95                               | 48 ms             | —       |        |
| Redis commands                                           | 38,165            | —       |        |
| **Worker**                                               |                   |         |        |
| Operations run                                           | 150               | —       |        |
| Operation processing p50                                 | 94 ms             | —       |        |
| Operation processing p95                                 | 232 ms            | —       |        |
| Step 'compile' mean                                      | 1 ms              | —       |        |
| Step 'testcase' mean                                     | 25 ms             | —       |        |
| Sandbox init mean                                        | 5 ms              | —       |        |
| Sandbox cleanup mean                                     | 2 ms              | —       |        |
| File materialization mean                                | 1 ms              | —       |        |
| Blob cache hits / misses                                 | 235 / 53          | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Live pooled instances (all plugins, since boot)          | 11                | —       |        |
| icpc: live / built / recycled (since boot)               | 1 / 1 / 0         | —       |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | —       |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | —       |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | —       |        |
| **Resources**                                            |                   |         |        |
| Server RSS at start                                      | 547 MiB           | —       |        |
| Server RSS peak                                          | 675 MiB           | —       |        |
| Server RSS mean                                          | 646 MiB           | —       |        |
| Server RSS idle after the run                            | 625 MiB           | —       |        |
| Server container memory peak (incl. page cache)          | 682 MiB           | —       |        |
| Server CPU time                                          | 5.28 s            | —       |        |
| Workers CPU time (all 4)                                 | 5.41 s            | —       |        |

## L1-50MBx20

1 submission(s), concurrency 1, 20 test cases × 52,428,800 bytes each.

|                                                          | perf              | perfcap | change |
| -------------------------------------------------------- | ----------------- | ------- | ------ |
| **Outcome**                                              |                   |         |        |
| Submissions judged                                       | 1                 | —       |        |
| Verdicts                                                 | Judged/Accepted 1 | —       |        |
| SystemError (final)                                      | 0                 | —       |        |
| Re-dispatches (hidden retries)                           | 0                 | —       |        |
| Re-judged (epoch > 0)                                    | 0                 | —       |        |
| Submit request errors                                    | 0                 | —       |        |
| **Latency**                                              |                   |         |        |
| Wall time (whole scenario)                               | 12.83 s           | —       |        |
| Submission latency p50 (created→judged)                  | 12.47 s           | —       |        |
| Submission latency p95                                   | 12.47 s           | —       |        |
| Submission latency max                                   | 12.47 s           | —       |        |
| Waiting to be claimed p50 (created→leased)               | 638 ms            | —       |        |
| Waiting to be claimed p95                                | 638 ms            | —       |        |
| Judging p50 (leased→judged)                              | 11.84 s           | —       |        |
| Judging per test case p50                                | 592 ms            | —       |        |
| Throughput (submissions / s)                             | 0.08              | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Plugin calls                                             | 124               | —       |        |
| Plugin call time, total                                  | 210 ms            | —       |        |
| Plugin call p95                                          | 7 ms              | —       |        |
| icpc: calls / total time                                 | 22 / 111 ms       | —       |        |
| batch-evaluator: calls / total time                      | 40 / 72 ms        | —       |        |
| standard-checkers: calls / total time                    | 40 / 9 ms         | —       |        |
| standard-languages: calls / total time                   | 20 / 17 ms        | —       |        |
| Pool acquire wait, total                                 | 0 ms              | —       |        |
| Pool acquire wait p99                                    | 0 ms              | —       |        |
| Pool acquire failures                                    | 0                 | —       |        |
| Instances built (compile + instantiate)                  | 0                 | —       |        |
| Instance build time, total                               | 0 ms              | —       |        |
| Instance build time, mean                                | —                 | —       |        |
| Instances recycled                                       | 0                 | —       |        |
| Evaluator semaphore wait, total                          | 0 ms              | —       |        |
| Host function time, total                                | 210 ms            | —       |        |
| **Queues**                                               |                   |         |        |
| Operation round trip p50 (enqueue→result delivered)      | 688 ms            | —       |        |
| Operation round trip p95                                 | 969 ms            | —       |        |
| Worker queue wait p50                                    | 25 ms             | —       |        |
| Worker queue wait p95                                    | 48 ms             | —       |        |
| Message age at consume p95                               | 48 ms             | —       |        |
| Redis commands                                           | 14,413            | —       |        |
| **Worker**                                               |                   |         |        |
| Operations run                                           | 20                | —       |        |
| Operation processing p50                                 | 615 ms            | —       |        |
| Operation processing p95                                 | 962 ms            | —       |        |
| Step 'compile' mean                                      | 1 ms              | —       |        |
| Step 'testcase' mean                                     | 75 ms             | —       |        |
| Sandbox init mean                                        | 8 ms              | —       |        |
| Sandbox cleanup mean                                     | 5 ms              | —       |        |
| File materialization mean                                | 128 ms            | —       |        |
| Blob cache hits / misses                                 | 40 / 20           | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Live pooled instances (all plugins, since boot)          | 11                | —       |        |
| icpc: live / built / recycled (since boot)               | 1 / 1 / 0         | —       |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | —       |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | —       |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | —       |        |
| **Resources**                                            |                   |         |        |
| Server RSS at start                                      | 624 MiB           | —       |        |
| Server RSS peak                                          | 624 MiB           | —       |        |
| Server RSS mean                                          | 617 MiB           | —       |        |
| Server RSS idle after the run                            | 614 MiB           | —       |        |
| Server container memory peak (incl. page cache)          | 638 MiB           | —       |        |
| Server CPU time                                          | 925 ms            | —       |        |
| Workers CPU time (all 4)                                 | 10.86 s           | —       |        |

## L2-50MBx50

1 submission(s), concurrency 1, 50 test cases × 52,428,800 bytes each.

|                                                          | perf              | perfcap | change |
| -------------------------------------------------------- | ----------------- | ------- | ------ |
| **Outcome**                                              |                   |         |        |
| Submissions judged                                       | 1                 | —       |        |
| Verdicts                                                 | Judged/Accepted 1 | —       |        |
| SystemError (final)                                      | 0                 | —       |        |
| Re-dispatches (hidden retries)                           | 0                 | —       |        |
| Re-judged (epoch > 0)                                    | 0                 | —       |        |
| Submit request errors                                    | 0                 | —       |        |
| **Latency**                                              |                   |         |        |
| Wall time (whole scenario)                               | 30.39 s           | —       |        |
| Submission latency p50 (created→judged)                  | 30.01 s           | —       |        |
| Submission latency p95                                   | 30.01 s           | —       |        |
| Submission latency max                                   | 30.01 s           | —       |        |
| Waiting to be claimed p50 (created→leased)               | 526 ms            | —       |        |
| Waiting to be claimed p95                                | 526 ms            | —       |        |
| Judging p50 (leased→judged)                              | 29.48 s           | —       |        |
| Judging per test case p50                                | 590 ms            | —       |        |
| Throughput (submissions / s)                             | 0.03              | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Plugin calls                                             | 304               | —       |        |
| Plugin call time, total                                  | 539 ms            | —       |        |
| Plugin call p95                                          | 8 ms              | —       |        |
| icpc: calls / total time                                 | 52 / 325 ms       | —       |        |
| batch-evaluator: calls / total time                      | 100 / 159 ms      | —       |        |
| standard-checkers: calls / total time                    | 100 / 17 ms       | —       |        |
| standard-languages: calls / total time                   | 50 / 37 ms        | —       |        |
| Pool acquire wait, total                                 | 0 ms              | —       |        |
| Pool acquire wait p99                                    | 0 ms              | —       |        |
| Pool acquire failures                                    | 0                 | —       |        |
| Instances built (compile + instantiate)                  | 0                 | —       |        |
| Instance build time, total                               | 0 ms              | —       |        |
| Instance build time, mean                                | —                 | —       |        |
| Instances recycled                                       | 0                 | —       |        |
| Evaluator semaphore wait, total                          | 0 ms              | —       |        |
| Host function time, total                                | 539 ms            | —       |        |
| **Queues**                                               |                   |         |        |
| Operation round trip p50 (enqueue→result delivered)      | 709 ms            | —       |        |
| Operation round trip p95                                 | 971 ms            | —       |        |
| Worker queue wait p50                                    | 25 ms             | —       |        |
| Worker queue wait p95                                    | 48 ms             | —       |        |
| Message age at consume p95                               | 48 ms             | —       |        |
| Redis commands                                           | 35,332            | —       |        |
| **Worker**                                               |                   |         |        |
| Operations run                                           | 50                | —       |        |
| Operation processing p50                                 | 609 ms            | —       |        |
| Operation processing p95                                 | 961 ms            | —       |        |
| Step 'compile' mean                                      | 1 ms              | —       |        |
| Step 'testcase' mean                                     | 69 ms             | —       |        |
| Sandbox init mean                                        | 8 ms              | —       |        |
| Sandbox cleanup mean                                     | 3 ms              | —       |        |
| File materialization mean                                | 132 ms            | —       |        |
| Blob cache hits / misses                                 | 100 / 50          | —       |        |
| **Plugin host (server)**                                 |                   |         |        |
| Live pooled instances (all plugins, since boot)          | 11                | —       |        |
| icpc: live / built / recycled (since boot)               | 1 / 1 / 0         | —       |        |
| batch-evaluator: live / built / recycled (since boot)    | 1 / 1 / 0         | —       |        |
| standard-checkers: live / built / recycled (since boot)  | 1 / 1 / 0         | —       |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0         | —       |        |
| **Resources**                                            |                   |         |        |
| Server RSS at start                                      | 614 MiB           | —       |        |
| Server RSS peak                                          | 615 MiB           | —       |        |
| Server RSS mean                                          | 614 MiB           | —       |        |
| Server RSS idle after the run                            | 613 MiB           | —       |        |
| Server container memory peak (incl. page cache)          | 629 MiB           | —       |        |
| Server CPU time                                          | 2.28 s            | —       |        |
| Workers CPU time (all 4)                                 | 27.29 s           | —       |        |

## C1-1000@100

1000 submission(s), concurrency 100, 10 test cases × 64 bytes each.

|                                                          | perf                 | perfcap              | change |
| -------------------------------------------------------- | -------------------- | -------------------- | ------ |
| **Outcome**                                              |                      |                      |        |
| Submissions judged                                       | 1,000                | 1,000                | ≈      |
| Verdicts                                                 | Judged/Accepted 1000 | Judged/Accepted 1000 |        |
| SystemError (final)                                      | 0                    | 0                    |        |
| Re-dispatches (hidden retries)                           | 0                    | 0                    |        |
| Re-judged (epoch > 0)                                    | 0                    | 0                    |        |
| Submit request errors                                    | 0                    | 0                    |        |
| **Latency**                                              |                      |                      |        |
| Wall time (whole scenario)                               | 155.09 s             | 156.28 s             | ≈      |
| Submission latency p50 (created→judged)                  | 141.94 s             | 86.27 s              | 0.61×  |
| Submission latency p95                                   | 152.51 s             | 153.35 s             | ≈      |
| Submission latency max                                   | 153.19 s             | 154.04 s             | ≈      |
| Waiting to be claimed p50 (created→leased)               | 14.84 s              | 47.24 s              | 3.18×  |
| Waiting to be claimed p95                                | 28.36 s              | 122.74 s             | 4.33×  |
| Judging p50 (leased→judged)                              | 124.78 s             | 38.49 s              | 0.31×  |
| Judging per test case p50                                | 12.48 s              | 3.85 s               | 0.31×  |
| Throughput (submissions / s)                             | 6.45                 | 6.40                 | ≈      |
| **Plugin host (server)**                                 |                      |                      |        |
| Plugin calls                                             | 64,000               | 64,000               | ≈      |
| Plugin call time, total                                  | 123.52 s             | 124.64 s             | ≈      |
| Plugin call p95                                          | 5 ms                 | 5 ms                 | ≈      |
| icpc: calls / total time                                 | 12,000 / 47.61 s     | 12,000 / 43.59 s     |        |
| batch-evaluator: calls / total time                      | 20,000 / 36.50 s     | 20,000 / 43.71 s     |        |
| standard-checkers: calls / total time                    | 20,000 / 2.90 s      | 20,000 / 2.84 s      |        |
| standard-languages: calls / total time                   | 10,000 / 7.87 s      | 10,000 / 7.87 s      |        |
| Pool acquire wait, total                                 | 35.12 s              | 33.93 s              | ≈      |
| Pool acquire wait p99                                    | 20 ms                | 20 ms                | ≈      |
| Pool acquire failures                                    | 0                    | 0                    |        |
| Instances built (compile + instantiate)                  | 87                   | 111                  | 1.28×  |
| Instance build time, total                               | 1.14 s               | 1.44 s               | 1.27×  |
| Instance build time, mean                                | 13 ms                | 13 ms                | ≈      |
| Instances recycled                                       | 0                    | 0                    |        |
| Evaluator semaphore wait, total                          | 5 ms                 | 6 ms                 | 1.07×  |
| Host function time, total                                | 123.52 s             | 124.64 s             | ≈      |
| **Queues**                                               |                      |                      |        |
| Operation round trip p50 (enqueue→result delivered)      | 821 ms               | 828 ms               | ≈      |
| Operation round trip p95                                 | 2.17 s               | 2.19 s               | ≈      |
| Worker queue wait p50                                    | 753 ms               | 752 ms               | ≈      |
| Worker queue wait p95                                    | 984 ms               | 982 ms               | ≈      |
| Message age at consume p95                               | 984 ms               | 982 ms               | ≈      |
| Redis commands                                           | 554,497              | 522,641              | 0.94×  |
| **Worker**                                               |                      |                      |        |
| Operations run                                           | 10,000               | 10,000               | ≈      |
| Operation processing p50                                 | 75 ms                | 75 ms                | ≈      |
| Operation processing p95                                 | 98 ms                | 98 ms                | ≈      |
| Step 'compile' mean                                      | 1 ms                 | 1 ms                 | 1.18×  |
| Step 'testcase' mean                                     | 6 ms                 | 6 ms                 | ≈      |
| Sandbox init mean                                        | 2 ms                 | 2 ms                 | ≈      |
| Sandbox cleanup mean                                     | 1 ms                 | 1 ms                 | ≈      |
| File materialization mean                                | 0 ms                 | 0 ms                 | ≈      |
| Blob cache hits / misses                                 | 10,000 / 0           | 9,996 / 3            |        |
| **Plugin host (server)**                                 |                      |                      |        |
| Live pooled instances (all plugins, since boot)          | 98                   | 122                  | 1.24×  |
| icpc: live / built / recycled (since boot)               | 34 / 34 / 0          | 37 / 37 / 0          |        |
| batch-evaluator: live / built / recycled (since boot)    | 13 / 13 / 0          | 35 / 35 / 0          |        |
| standard-checkers: live / built / recycled (since boot)  | 4 / 4 / 0            | 5 / 5 / 0            |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0            | 1 / 1 / 0            |        |
| **Resources**                                            |                      |                      |        |
| Server RSS at start                                      | 617 MiB              | 529 MiB              | 0.86×  |
| Server RSS peak                                          | 1570 MiB             | 1360 MiB             | 0.87×  |
| Server RSS mean                                          | 1298 MiB             | 1188 MiB             | 0.92×  |
| Server RSS idle after the run                            | 1166 MiB             | 1108 MiB             | ≈      |
| Server container memory peak (incl. page cache)          | 1605 MiB             | 1356 MiB             | 0.85×  |
| Server CPU time                                          | 90.27 s              | 73.15 s              | 0.81×  |
| Workers CPU time (all 4)                                 | 197.30 s             | 198.59 s             | ≈      |

## C2-1000@100-32slots

1000 submission(s), concurrency 100, 10 test cases × 64 bytes each.

|                                                          | perf                 | perfcap              | change |
| -------------------------------------------------------- | -------------------- | -------------------- | ------ |
| **Outcome**                                              |                      |                      |        |
| Submissions judged                                       | 1,000                | 1,000                | ≈      |
| Verdicts                                                 | Judged/Accepted 1000 | Judged/Accepted 1000 |        |
| SystemError (final)                                      | 0                    | 0                    |        |
| Re-dispatches (hidden retries)                           | 0                    | 0                    |        |
| Re-judged (epoch > 0)                                    | 0                    | 0                    |        |
| Submit request errors                                    | 0                    | 0                    |        |
| **Latency**                                              |                      |                      |        |
| Wall time (whole scenario)                               | 37.05 s              | 36.04 s              | ≈      |
| Submission latency p50 (created→judged)                  | 20.87 s              | 20.24 s              | ≈      |
| Submission latency p95                                   | 35.05 s              | 34.34 s              | ≈      |
| Submission latency max                                   | 35.32 s              | 34.76 s              | ≈      |
| Waiting to be claimed p50 (created→leased)               | 14.95 s              | 14.80 s              | ≈      |
| Waiting to be claimed p95                                | 28.47 s              | 28.26 s              | ≈      |
| Judging p50 (leased→judged)                              | 5.75 s               | 5.33 s               | 0.93×  |
| Judging per test case p50                                | 575 ms               | 533 ms               | 0.93×  |
| Throughput (submissions / s)                             | 26.99                | 27.75                | ≈      |
| **Plugin host (server)**                                 |                      |                      |        |
| Plugin calls                                             | 64,000               | 64,000               | ≈      |
| Plugin call time, total                                  | 261.04 s             | 164.58 s             | 0.63×  |
| Plugin call p95                                          | 30 ms                | 15 ms                | 0.50×  |
| icpc: calls / total time                                 | 12,000 / 94.05 s     | 12,000 / 41.89 s     |        |
| batch-evaluator: calls / total time                      | 20,000 / 142.13 s    | 20,000 / 90.72 s     |        |
| standard-checkers: calls / total time                    | 20,000 / 2.88 s      | 20,000 / 3.12 s      |        |
| standard-languages: calls / total time                   | 10,000 / 9.00 s      | 10,000 / 7.65 s      |        |
| Pool acquire wait, total                                 | 39.56 s              | 31.15 s              | 0.79×  |
| Pool acquire wait p99                                    | 30 ms                | 23 ms                | 0.76×  |
| Pool acquire failures                                    | 0                    | 0                    |        |
| Instances built (compile + instantiate)                  | 150                  | 113                  | 0.75×  |
| Instance build time, total                               | 1.93 s               | 1.48 s               | 0.76×  |
| Instance build time, mean                                | 13 ms                | 13 ms                | ≈      |
| Instances recycled                                       | 0                    | 0                    |        |
| Evaluator semaphore wait, total                          | 5 ms                 | 5 ms                 | ≈      |
| Host function time, total                                | 261.04 s             | 164.58 s             | 0.63×  |
| **Queues**                                               |                      |                      |        |
| Operation round trip p50 (enqueue→result delivered)      | 196 ms               | 198 ms               | ≈      |
| Operation round trip p95                                 | 444 ms               | 448 ms               | ≈      |
| Worker queue wait p50                                    | 75 ms                | 77 ms                | ≈      |
| Worker queue wait p95                                    | 200 ms               | 200 ms               | ≈      |
| Message age at consume p95                               | 200 ms               | 200 ms               | ≈      |
| Redis commands                                           | 474,799              | 458,147              | ≈      |
| **Worker**                                               |                      |                      |        |
| Operations run                                           | 10,000               | 10,000               | ≈      |
| Operation processing p50                                 | 120 ms               | 121 ms               | ≈      |
| Operation processing p95                                 | 238 ms               | 238 ms               | ≈      |
| Step 'compile' mean                                      | 1 ms                 | 1 ms                 | ≈      |
| Step 'testcase' mean                                     | 17 ms                | 17 ms                | ≈      |
| Sandbox init mean                                        | 9 ms                 | 8 ms                 | ≈      |
| Sandbox cleanup mean                                     | 9 ms                 | 9 ms                 | ≈      |
| File materialization mean                                | 0 ms                 | 0 ms                 | ≈      |
| Blob cache hits / misses                                 | 9,996 / 3            | 9,996 / 3            |        |
| **Plugin host (server)**                                 |                      |                      |        |
| Live pooled instances (all plugins, since boot)          | 161                  | 124                  | 0.77×  |
| icpc: live / built / recycled (since boot)               | 50 / 50 / 0          | 35 / 35 / 0          |        |
| batch-evaluator: live / built / recycled (since boot)    | 60 / 60 / 0          | 36 / 36 / 0          |        |
| standard-checkers: live / built / recycled (since boot)  | 8 / 8 / 0            | 9 / 9 / 0            |        |
| standard-languages: live / built / recycled (since boot) | 1 / 1 / 0            | 1 / 1 / 0            |        |
| **Resources**                                            |                      |                      |        |
| Server RSS at start                                      | 571 MiB              | 528 MiB              | 0.92×  |
| Server RSS peak                                          | 1598 MiB             | 1525 MiB             | ≈      |
| Server RSS mean                                          | 1378 MiB             | 1284 MiB             | 0.93×  |
| Server RSS idle after the run                            | 1488 MiB             | 1147 MiB             | 0.77×  |
| Server container memory peak (incl. page cache)          | 1602 MiB             | 1521 MiB             | 0.95×  |
| Server CPU time                                          | 63.49 s              | 62.30 s              | ≈      |
| Workers CPU time (all 4)                                 | 277.34 s             | 279.46 s             | ≈      |

## C3-1000@100-32slots-batch128

1000 submission(s), concurrency 100, 10 test cases × 64 bytes each.

|                                                          | perf | perfcap              | change |
| -------------------------------------------------------- | ---- | -------------------- | ------ |
| **Outcome**                                              |      |                      |        |
| Submissions judged                                       | —    | 1,000                |        |
| Verdicts                                                 | —    | Judged/Accepted 1000 |        |
| SystemError (final)                                      | —    | 0                    |        |
| Re-dispatches (hidden retries)                           | —    | 0                    |        |
| Re-judged (epoch > 0)                                    | —    | 0                    |        |
| Submit request errors                                    | —    | 0                    |        |
| **Latency**                                              |      |                      |        |
| Wall time (whole scenario)                               | —    | 37.28 s              |        |
| Submission latency p50 (created→judged)                  | —    | 19.04 s              |        |
| Submission latency p95                                   | —    | 34.77 s              |        |
| Submission latency max                                   | —    | 34.98 s              |        |
| Waiting to be claimed p50 (created→leased)               | —    | 10.99 s              |        |
| Waiting to be claimed p95                                | —    | 27.44 s              |        |
| Judging p50 (leased→judged)                              | —    | 8.23 s               |        |
| Judging per test case p50                                | —    | 823 ms               |        |
| Throughput (submissions / s)                             | —    | 26.82                |        |
| **Plugin host (server)**                                 |      |                      |        |
| Plugin calls                                             | —    | 64,000               |        |
| Plugin call time, total                                  | —    | 195.22 s             |        |
| Plugin call p95                                          | —    | 25 ms                |        |
| icpc: calls / total time                                 | —    | 12,000 / 74.62 s     |        |
| batch-evaluator: calls / total time                      | —    | 20,000 / 93.59 s     |        |
| standard-checkers: calls / total time                    | —    | 20,000 / 3.14 s      |        |
| standard-languages: calls / total time                   | —    | 10,000 / 7.85 s      |        |
| Pool acquire wait, total                                 | —    | 59.28 s              |        |
| Pool acquire wait p99                                    | —    | 43 ms                |        |
| Pool acquire failures                                    | —    | 0                    |        |
| Instances built (compile + instantiate)                  | —    | 120                  |        |
| Instance build time, total                               | —    | 1.63 s               |        |
| Instance build time, mean                                | —    | 14 ms                |        |
| Instances recycled                                       | —    | 0                    |        |
| Evaluator semaphore wait, total                          | —    | 12 ms                |        |
| Host function time, total                                | —    | 195.22 s             |        |
| **Queues**                                               |      |                      |        |
| Operation round trip p50 (enqueue→result delivered)      | —    | 201 ms               |        |
| Operation round trip p95                                 | —    | 453 ms               |        |
| Worker queue wait p50                                    | —    | 78 ms                |        |
| Worker queue wait p95                                    | —    | 208 ms               |        |
| Message age at consume p95                               | —    | 208 ms               |        |
| Redis commands                                           | —    | 469,074              |        |
| **Worker**                                               |      |                      |        |
| Operations run                                           | —    | 10,000               |        |
| Operation processing p50                                 | —    | 125 ms               |        |
| Operation processing p95                                 | —    | 239 ms               |        |
| Step 'compile' mean                                      | —    | 1 ms                 |        |
| Step 'testcase' mean                                     | —    | 17 ms                |        |
| Sandbox init mean                                        | —    | 9 ms                 |        |
| Sandbox cleanup mean                                     | —    | 9 ms                 |        |
| File materialization mean                                | —    | 0 ms                 |        |
| Blob cache hits / misses                                 | —    | 9,996 / 3            |        |
| **Plugin host (server)**                                 |      |                      |        |
| Live pooled instances (all plugins, since boot)          | —    | 131                  |        |
| icpc: live / built / recycled (since boot)               | —    | 44 / 44 / 0          |        |
| batch-evaluator: live / built / recycled (since boot)    | —    | 32 / 32 / 0          |        |
| standard-checkers: live / built / recycled (since boot)  | —    | 7 / 7 / 0            |        |
| standard-languages: live / built / recycled (since boot) | —    | 1 / 1 / 0            |        |
| **Resources**                                            |      |                      |        |
| Server RSS at start                                      | —    | 503 MiB              |        |
| Server RSS peak                                          | —    | 1691 MiB             |        |
| Server RSS mean                                          | —    | 1526 MiB             |        |
| Server RSS idle after the run                            | —    | 1440 MiB             |        |
| Server container memory peak (incl. page cache)          | —    | 1694 MiB             |        |
| Server CPU time                                          | —    | 61.90 s              |        |
| Workers CPU time (all 4)                                 | —    | 277.45 s             |        |
