---
title: 评测性能
sidebar_label: 评测性能
sidebar_position: 3
---

# 评测性能

有两个工具用于测量评测吞吐量，以及每个提交的时间花在了哪里：

- `broccoli-stress-test perf` 通过 HTTP 驱动一个或多个正在运行的服务器，报告一个
  测量点：吞吐量、延迟、分阶段耗时，以及每个提交、每个测试点的 Postgres、Redis
  和插件调用次数。
- `scripts/perf/judging-matrix.py` 用 release 二进制启动服务器与 worker 的拓扑，
  在测试点数量与并发度组成的网格上重复运行 `perf`，采样 CPU，并写出 `summary.md`。

只测平台自身开销时使用 mock 沙箱（`worker.sandbox_backend = "mock"`）。它把提交
程序和检查器作为普通的宿主进程运行，因此 `isolate` 的 init、cleanup、cgroup 与挂载
都不计入结果。要测完整开销，请在能运行 `isolate` 的主机上使用 `isolate`。

## 运行一个测量点

先构建二进制，再让 `perf` 指向一个已加载评测插件并有管理员账号的服务器：

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

`--concurrency` 个客户端各自保持一个在途提交：提交后每隔 `--poll-interval-ms`
轮询一次 `GET /api/v1/submissions/{id}`，直到进入终态。因此负载是闭环的。吞吐量
随并发度上升，直到某个阶段饱和，此后上升的是延迟。扫描并发度即可找到饱和点。闭环负载还会把吞吐量限制在每个客户端每个轮询间隔一个提交
（`client_throughput.poll_cap_subs_per_s`）；接近这一上限时，请改看服务端的
`db_throughput`，或提高并发度。
传入多个 `--server-url` 时，提交按轮询方式分摊到各服务器。

`--database-url`、`--redis-url` 与 `--metrics-url` 都是可选的，每个都会给报告增加
一部分：

- `--database-url` 读取本次运行所产生行的阶段时间戳，并在运行前重置
  `pg_stat_statements`、运行后读取。该扩展必须预加载
  （`shared_preload_libraries = 'pg_stat_statements'`）；补充迁移会创建它。
- `--redis-url` 对 `INFO commandstats` 做差。
- `--metrics-url` 对每个 URL 上的所有 Prometheus 计数器与直方图做差。同一主机上的
  每个 worker 需要各自的 `worker.metrics_addr`。

当每个提交都得到场景预期的判定时进程退出码为 0；有提交没有得到预期判定时为 1
（SystemError 会出现在 `statuses` 中）；准备阶段出错时为 2。

## 运行矩阵

脚本要求 Postgres 与 Redis 已在运行，并且评测插件（`icpc`、`ioi`、
`batch-evaluator`、`standard-checkers`、`standard-languages`）已构建到
`--plugins-dir` 中：

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

拓扑 `SxW` 表示本机上 S 个服务器（端口从 3000 起）和 W 个 worker（指标端口从
9100 起）。`--fresh-db` 为每个拓扑重建数据库。`--max-test-cases-per-point` 限制
单个测量点的提交数量，使 100 个测试点的测量不至于过长。

并发度为 1 时测量点关注延迟，客户端每 `--poll-interval-ms`（50）轮询一次；并发度
更高时每 `--load-poll-interval-ms`（1000）轮询一次。64 个客户端以 50 ms 轮询时每秒
有 1280 个请求，每个约 9 条语句，这会主导服务器与数据库的 CPU，掩盖评测路径本身。

要证明某个瓶颈，只改动一个参数并重跑同一测量点。设置通过环境变量传入：

```bash
python3 scripts/perf/judging-matrix.py --topologies 1x1 --test-cases 1 \
    --concurrency 64 --repeats 3 \
    --server-env BROCCOLI__SERVER__CLAIM_POLL_INTERVAL_MS=100 \
    --worker-env BROCCOLI__WORKER__MAX_CONCURRENCY=8 \
    --out-dir perf-out/claim-100ms-slots-8
```

使用 mock 沙箱时，检查器二进制必须位于宿主机的 `/tools`，因为 mock 后端按原样
执行 argv，不会重映射 `/tools/...`。脚本会把 `broccoli-compare` 复制到那里
（`--tools-dir`）。

## 各项数字的含义

客户端（本进程的时钟）：

| 字段                | 含义                                                         |
| ------------------- | ------------------------------------------------------------ |
| `client_throughput` | 测量的提交数（及测试点数）除以测量的墙钟时间。               |
| `submit_latency_ms` | `POST .../submissions` 往返：鉴权、限流、钩子、插入。        |
| `e2e_latency_ms`    | 从 POST 到第一次轮询看到终态。                               |

服务端，来自本次运行自己的行。每个时间戳都由服务器或 Postgres 写入，不会与客户端
时钟比较。多主机运行时，`created_at` 与测试结果的 `created_at` 来自写入它的服务器
的时钟，而 `leased_at` 与 `judged_at` 来自 `NOW()`，因此在依赖跨阶段数字之前，
请先让各服务器的时钟保持同步。

| 阶段              | 从 → 到                                  | 包含的内容                                         |
| ----------------- | ---------------------------------------- | -------------------------------------------------- |
| `queued`          | `submission.created_at` → `leased_at`    | 等待认领 fiber 的下一次 tick 以及在途名额。        |
| `to_first_result` | `leased_at` → 第一条 `test_case_result`  | 插件分派、操作发布、worker、结果消费。             |
| `between_results` | 第一条 → 最后一条 `test_case_result`     | 其余测试点。                                       |
| `finalize`        | 最后一条 `test_case_result` → `judged_at` | 汇总与判定写入。                                   |
| `total`           | `created_at` → `judged_at`               | 除客户端轮询滞后之外的全部时间。                   |
| `db_throughput`   | 已评测行数除以数据库时钟跨度             | `steady_subs_per_s` 取第 10 到第 90 百分位的完成。 |

开销统计：

- `db.statements_per_submission` 与 `per_test_case` 统计运行期间数据库中的所有
  语句，包括后台 fiber。使用 `--idle-secs` 时，会按语句减去同样时长下的空载速率。
- `client_polling` 在运行结束后发出 `--calibrate-polls` 次 GET，测出一次客户端
  轮询的开销（语句、Redis 命令、插件调用），并报告扣除客户端自身轮询后的总量。
  只看评测路径时请使用其中的 `net_*` 字段。
- `redis.by_command_per_submission` 对 Redis 命令做同样的统计。低负载时空闲轮询
  循环（worker 消费、服务器结果轮询）主导这一计数，因此阅读它时务必传入
  `--idle-secs`。
- `metrics.histograms` 给出每个直方图序列每个提交的调用次数，以及均值、p50 与
  p95。分位数在桶内插值，应视为桶分辨率的估计。与阶段对应的有：
  `broccoli_task_queue_wait_duration_seconds`（操作等待 worker 槽位）、
  `broccoli_task_process_duration_seconds`（worker 上的一次操作）、按 `step_kind`
  区分的 `broccoli_step_duration_seconds`（编译、测试点、检查器）、
  `broccoli_operation_result_e2e_duration_seconds`（从发布到结果送达）、按插件和函数
  区分的 `broccoli_plugin_call_duration_seconds`，以及
  `broccoli_submission_state_transition_duration_seconds`。

矩阵还会为每个角色给出每个提交的 CPU 时间（`server`、`worker` 取自 `/proc`，
`postgres` 与 `redis` 取自各自容器的 cgroup），以及每个测量点期间各角色占用的核数。

## 可复现性

输出目录中的 `environment.json` 记录 `nproc`、内存、CPU 型号、git 提交以及全部
参数。每个测量点至少重复三次；`summary.md` 输出中位数以及最小到最大的范围。只使用
release 构建，矩阵运行期间不要在主机上施加其他负载。
