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
3. **Claim rate (all bursts).** The dispatcher claims 32 submissions per 1 s
   poll (`server.claim_batch_size`, `server.claim_poll_interval_ms`). A
   1000-submission burst waits ~15 s (p50) just to be claimed, in both builds.
   Perf in C2 (27 submissions/s) is already close to that 32/s ceiling, so
   raising the batch size is the next lever.

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
- One API note: `JudgeProgress::request` no longer carries test cases or source
  files. No in-repo plugin reads them there; a third-party plugin that did would
  see them empty.

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
