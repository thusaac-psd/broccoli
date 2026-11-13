---
title: 插件执行
sidebar_label: 插件执行
sidebar_position: 5
---

# 插件执行

每一次对插件客体的调用都经过同一条路径，即 `packages/plugin-core/src/traits.rs` 中的 `PluginInvoker::call_raw`，并遵循 `packages/plugin-core/src/executor.rs` 中的同一个模型：调用先被准入，然后带着截止时间在阻塞线程上运行，并在每次等待时交还其 CPU。

## 为什么采用这个模型

Extism 调用在发起它的线程上一直运行到客体返回，宿主函数通过 `block_on` 回到异步运行时。因此进行中的调用始终占用一个操作系统线程，丢弃等待它的 future 什么也停不下来。由此得出两点：

- 任何排队都必须发生在占用线程之前。持有阻塞线程等待容量的调用会在插件变慢时耗尽 tokio 的阻塞线程池，连带拖垮池中的其他一切。
- 在宿主等待期间持有容量会使评测死锁。比赛的 `on_submission` 调用（`services/submission_dispatch.rs`）在整个评测期间停留在 `get_next_evaluate_result` 和 `get_next_operation_result`（`host_funcs/evaluate.rs`、`host_funcs/dispatch.rs`）中，而它的评测器调用是需要容量才能运行的独立调用。

## 准入

`packages/plugin-core/src/admission.rs` 为每台服务器持有 `C` 个 CPU 令牌，除非 `plugin.max_concurrent_calls` 另有设置，否则为每个可用核心 `TOKENS_PER_CORE`（2）个。令牌表示“正在运行客体代码”，而不是“调用进行中”。

| 类别 | 调用 | 份额 |
|---|---|---|
| `Interactive` | 可见性查询、请求钩子、插件 HTTP 路由 | `ceil(C/2)` |
| `Background` | 评测、操作、分离回调、定时器、后台钩子 | `floor(C/2)` |

- 一个类别可以借用另一个类别空闲的令牌。两个类别都在等待时释放的令牌交给低于其份额的类别，因此在洪峰下任何一方都不会饿死另一方。
- 交互调用还受已准入调用数上限的约束（每个令牌 `INTERACTIVE_ADMITTED_PER_TOKEN` 个），因为每个已准入的调用即使在挂起时也占用一个阻塞线程。数据库变慢导致所有交互调用变慢时，多出的调用在队列中等待，不占用任何资源。
- 排队的等待者是一个异步 future。丢弃它（例如客户端已离开）即离开队列。
- 生命周期调用（`init`、`CallOptions::lifecycle`）跳过准入：它们从不被丢弃，也从不排在任何类别之后。

类别是必填参数。`CallOptions` 没有默认值，trait 也没有便捷的 `call`；每个调用点都要选择 `interactive`、`background`、`lifecycle` 或 `nested`。钩子的类别来自分发点（`hooks::dispatch_hooks`），因为同一个 `PluginHook` 既服务 `before_submission` 请求路径，也服务后台通知。

池的查找发生在准入之后，因此跨越一次重载而排队的调用会在新版本上运行。

## 挂起

每个宿主函数都会执行的序言 `HostCall::enter`（`packages/server/src/host_funcs/mod.rs`）在函数体期间持有一个 `plugin_core::executor::Parked`。它交还调用的令牌，并在析构时先于所有排队调用取回一个令牌（`Admission::resume`）。因此即使比赛提交的数量远多于令牌，整批提交也都能推进：每个提交只在其客体真正计算时持有令牌。

携带令牌、类别、截止时间和深度的线程局部 `CallScope` 由守卫安装和恢复，因此 panic 的客体或宿主函数不会留下过期的作用域。

## 嵌套调用

`checker:resolve`、`checker:interpret` 和 `language:resolve`（`host_funcs/checker.rs`、`host_funcs/language.rs`）通过 `CallOptions::nested()` 回调插件。

- 它们运行在第二个阻塞线程上。内联运行会在 `block_on` 中嵌套 `block_on` 并 panic。
- 它们跳过准入。父调用挂起在该宿主函数中，因此嵌套调用像恢复的调用那样取得令牌，继承父调用的类别，并取父调用截止时间与自身调用超时中较早者。
- 深度上限为 `MAX_DEPTH`（2）。这阻止了 A 调 B 再调 A 的循环，并限制阻塞线程链的栈深。
- 同一实例永不重入：每次调用租用自己的实例，因此对同一插件的嵌套调用会得到第二个 store。

## 截止时间

每个调用都有截止时间。交互调用使用请求的截止时间（`crate::request_deadline`，由包在请求超时外的中间件为每个请求设置），钩子取它与 30 秒中较早者，定时器使用其认领租约，评测和操作使用 `plugin.call_timeout_secs`。

一个进程级线程（`packages/plugin-core/src/deadline.rs`）为每个运行中的调用维护 `(Instant, CancelHandle)` 堆，并在截止时间到达时取消客体。挂起在宿主函数中的客体在返回客体代码时陷入 trap。

extism 先把调用注册到它的计时线程，然后才设置 store 的 epoch 截止时间。在两者之间被处理的取消会过早地推进 epoch，并同时注销该调用，于是客体继续运行，之后重复的取消也都被忽略。因此截止时间线程在调用注册后至少 `START_GRACE` 才发出第一次取消，并每隔 `CANCEL_RETRY` 重复一次，直到调用返回。

在截止时间仍在排队或在截止时间被取消的调用以 `PluginError::Unavailable` 失败。

## Unavailable 永远不是一个决定

`Unavailable` 表示服务器繁忙，与请求本身无关。

- `visibility::query_plugins` 返回 `Result`。`Unavailable` 变为 `AppError::PluginUnavailable`：带 `Retry-After: 2` 的 `503 PLUGIN_UNAVAILABLE`。它绝不会并入 `Deny`，否则列表会变成空页面、游标变成 400、单条读取变成 404。查询者的其他失败属于错误的回答，仍然拒绝整批资源。
- 不可用的阻塞钩子同样返回 503，而不是失败关闭的拒绝；不可用的通知钩子被跳过。
- 服务器端从不重试交互调用。Web 客户端把 503 视为临时状态（`packages/web-sdk/src/api/errors.ts` 中的 `isTemporaryApiError`），由 TanStack Query 退避重试。
- 后台调用方保留各自的语义：在租约结束时被截断的定时器投递在租约过期后重试，到达截止时间的评测调用是一个系统错误，由 SystemError 回收器重新评测。

## 评测扇出

服务器在评测机处未完成的测试点数量由 `dispatcher::fanout::Fanout` 限制：每个存活评测机槽位 `TEST_CASES_PER_WORKER_SLOT`（2）个。`WorkerSlots` 是存活评测机容量的唯一记录，即评测机心跳（`common::liveness`）中 `max_concurrency` 之和，由一个任务每 15 秒刷新一次；认领协程用同一个值确定其进行中上限。在看到任何容量之前，假定每个本地核心一个槽位。单个操作批次内的发布以固定的 32 个为窗口进行。

## 内存

池（`packages/plugin-core/src/pool.rs`）按需构建实例，并按最近使用优先保留归还的实例。内存由 `MemoryBudget`（`packages/plugin-core/src/memory.rs`）全局限制：

- 信号是进程的常驻集，来自 `/proc/self/statm`，在实例归还时至多每 50 毫秒读取一次。
- 超出预算时，按最久未使用的顺序在所有插件之间丢弃空闲实例，每个插件保留 `WARM_PER_PLUGIN` 个。
- 预算通过弱引用持有池，因此被重载替换的池在其最后一个租约归还之前仍会被裁剪。
- 预算为进程所在 cgroup 内存上限的 3/4（cgroup v2：`/proc/self/cgroup` 中的 `0::` 路径，然后是该 cgroup 或其最近的有上限父级的 `memory.max`；cgroup v1：`hierarchical_memory_limit`），没有上限时为 `MemTotal` 的 1/4，因为单机 compose 与其他服务共享机器。`plugin.memory_budget` 可以覆盖它。
- 每个实例的线性内存保留失控上限（`memory_max`），为预算的一半且至多 1.5 GiB。

`broccoli_plugin_instances_live` 和 `broccoli_plugin_calls_in_flight` 是按插件的仪表；`broccoli_plugin_admission_queued_total` 和 `broccoli_plugin_admission_wait_duration_seconds` 反映争用情况。

## 登录

Argon2 运行在自己的线程上（`utils/hash.rs`），每个核心一个，从不使用 tokio 的阻塞线程池，因此插件洪峰无法拖慢登录。

## 设置

`plugin.max_concurrent_calls`、`plugin.memory_budget` 和 `plugin.call_timeout_secs` 是仅有的调节项；前两项自动确定。定时器租约为调用超时加 `LEASE_MARGIN`（30 秒），阻塞线程池为每核心 16 个线程并限制在 `[512, 8192]` 内。曾用于调节池和扇出的键列在 `config::REMOVED_KEYS` 中；启动时会对仍被设置的键发出警告。
