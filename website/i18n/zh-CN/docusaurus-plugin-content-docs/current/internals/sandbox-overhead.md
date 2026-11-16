---
title: 沙箱开销
sidebar_label: 沙箱开销
sidebar_position: 5
---

# 沙箱开销

本页记录 isolate 沙箱在每个评测步骤上的开销。测量由
`packages/worker/tests/isolate_overhead.rs`
完成，直接走 worker 自身的沙箱与处理代码。下表数据来自 GitHub
`ubuntu-latest` 运行器，isolate 版本为 v2.0，cgroup 为 v2。运行器是共享虚拟机，
请在同一次运行内比较不同形态，不要跨运行比较。

| 形态                                  | n   | p50 ms | p95 ms | p99 ms |
| ------------------------------------- | --- | ------ | ------ | ------ |
| `isolate --init`                      | 200 | 0.71   | 1.82   | 2.43   |
| `isolate --cleanup`                   | 200 | 0.69   | 1.73   | 2.32   |
| 在已有沙箱中运行 `/bin/true`          | 200 | 2.61   | 3.13   | 3.47   |
| 单沙箱操作                            | 200 | 4.73   | 9.01   | 11.95  |
| 双沙箱操作                            | 200 | 5.16   | 7.57   | 9.44   |
| 已编译 C++ 程序，单沙箱               | 200 | 4.59   | 8.60   | 9.32   |
| 融合测试点（程序与检查器）            | 200 | 8.07   | 12.23  | 13.85  |
| 交互题，2 个进程                      | 20  | 6.11   | 10.63  | 11.52  |
| 交互题，8 个进程                      | 20  | 18.37  | 25.94  | 26.34  |
| 交互题，64 个进程                     | 20  | 117.9  | 122.0  | 128.0  |

一次操作耗时数毫秒，其中大部分是 `isolate --run` 本身：沙箱的创建与删除各不到
一毫秒，且 worker 在结果发布之后才删除沙箱（见
`packages/worker/src/models/operation/handler/teardown.rs`）。
多一个沙箱只增加约 0.4 ms，因此检查器（包括平台工具）保留自己的沙箱，而不是改用
更轻量的隔离方式。交互题每个选手进程占一个沙箱，每个进程约增加 1.8 ms。

## 重新运行

该基准需要具备 cgroup v2 委派的 isolate，因此在运行器上执行：把任意提交推送到
`bench/` 下的分支，`Isolate overhead` 工作流
（`.github/workflows/isolate-bench.yml`）会把表格写入运行摘要，并上传为
`isolate-overhead` 制品。

```bash
git push origin HEAD:refs/heads/bench/my-change
```

在已安装 isolate 的主机上可直接运行：

```bash
cargo test -p worker --locked --release --test isolate_overhead -- \
    --ignored --test-threads=1 --nocapture
```

评估一项改动时，把基线与改动分别推送到两个 `bench/` 分支，再比较相同形态的数据。
