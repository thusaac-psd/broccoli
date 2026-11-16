---
title: Sandbox overhead
sidebar_label: Sandbox overhead
sidebar_position: 5
---

# Sandbox overhead

What the isolate sandbox costs per judging step, measured through the worker's
own sandbox and handler code by
`packages/worker/tests/isolate_overhead.rs`.
The numbers below come from a GitHub `ubuntu-latest` runner with isolate v2.0
and cgroup v2. Runners are shared VMs, so compare shapes within one run, not
across runs.

| shape                                 | n   | p50 ms | p95 ms | p99 ms |
| ------------------------------------- | --- | ------ | ------ | ------ |
| `isolate --init`                      | 200 | 0.71   | 1.82   | 2.43   |
| `isolate --cleanup`                   | 200 | 0.69   | 1.73   | 2.32   |
| `/bin/true` in an existing box        | 200 | 2.61   | 3.13   | 3.47   |
| one-box operation                     | 200 | 4.73   | 9.01   | 11.95  |
| two-box operation                     | 200 | 5.16   | 7.57   | 9.44   |
| compiled C++ program, one box         | 200 | 4.59   | 8.60   | 9.32   |
| fused test case (program and checker) | 200 | 8.07   | 12.23  | 13.85  |
| communication, 2 processes            | 20  | 6.11   | 10.63  | 11.52  |
| communication, 8 processes            | 20  | 18.37  | 25.94  | 26.34  |
| communication, 64 processes           | 20  | 117.9  | 122.0  | 128.0  |

An operation is a few milliseconds, and most of it is the `isolate --run`
itself: box setup and removal are under a millisecond each, and the worker
removes boxes after the result is published (see
`packages/worker/src/models/operation/handler/teardown.rs`).
A second box adds about 0.4 ms, which is why checkers, platform tools
included, keep a box of their own rather than run in a lighter jail. A
communication problem pays about 1.8 ms per contestant process, one box each.

## Rerun it

The benchmark needs isolate with cgroup v2 delegation, so it runs on a runner:
push any commit to a branch under `bench/`, and the `Isolate overhead`
workflow (`.github/workflows/isolate-bench.yml`) writes the table to its run
summary and uploads it as the `isolate-overhead` artifact.

```bash
git push origin HEAD:refs/heads/bench/my-change
```

On a host with isolate installed, run it directly:

```bash
cargo test -p worker --locked --release --test isolate_overhead -- \
    --ignored --test-threads=1 --nocapture
```

To judge a change, push the base and the change to two `bench/` branches and
compare the same shapes.
