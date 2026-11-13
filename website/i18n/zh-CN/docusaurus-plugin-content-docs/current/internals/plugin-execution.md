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

- 一个类别可以借用另一个类别空闲的令牌。两个类别都在等待时释放的令牌交给低于其份额的类别，并在该类别内优先交给恢复的调用，因此在洪峰下任何一方都不会饿死另一方。
- 排队的等待者是一个不占用线程的异步 future。丢弃它（例如客户端已离开）即离开队列。

每个已准入的调用在返回之前都占用一个客体线程，无论是否挂起，因此每种 `CallKind` 都有自己的已准入调用上限：

| 种类 | 调用 | 上限 |
|---|---|---|
| `Request` | 交互调用 | 每个令牌 `REQUESTS_PER_TOKEN`（8）个 |
| `Judging` | `on_submission`、`on_code_run` | 本服务器的进行中上限 |
| `Work` | 评测器、分离回调、定时器 | 本服务器的扇出上限 + `C` |
| `Hook` | 后台钩子 | `C` |
| `Lifecycle` | `init`，不需要令牌 | `LIFECYCLE_CAP`（4） |

各种类相互独立，因此一种类达到上限时不会阻挡另一种：为等待结果而挂起的比赛调用占满评测上限，而产生这些结果的评测器调用所在的工作上限不受影响。服务器使评测和工作上限始终等于其当前份额（`dispatcher::fanout::spawn_admission_sizing`）。

种类是必填参数。`CallOptions` 没有默认值，trait 也没有便捷的 `call`；每个调用点都要选择 `interactive`、`judging`、`background`、`background_hook`、`lifecycle` 或 `nested`。钩子的种类来自分发点（`hooks::dispatch_hooks`），因为同一个 `PluginHook` 既服务 `before_submission` 请求路径，也服务后台通知。

池的查找发生在准入之后，因此跨越一次重载而排队的调用会在新版本上运行。重载先完整构建新条目再替换旧条目，因此调用永远不会遇到处于两个版本之间的插件；重载失败时旧版本继续服务。

## 客体线程

客体从不在 tokio 的阻塞线程池上运行。它们运行在 `plugin_core::threads::GuestThreads` 上：它把任务交给空闲线程或新启动一个线程，从不排队。它也不需要排队：使用中的线程至多为每个已准入调用一个，再加上该调用的嵌套调用一个（`Caps::threads`，即各上限之和的两倍），因此调用从不在等待线程时持有令牌，线程数是各显式上限之和。空闲线程在 30 秒后退出。

## 挂起

每个宿主函数都在构建处（`host_funcs::host_function`）声明它是否等待客体之外的东西（`host_funcs::Waits`）。声明为 `Blocks` 的函数（SQL、存储、定时器、提交、配置、结果等待、批次启动、会访问 Redis 的取消、嵌套调用）在 `plugin_core::executor::parked` 中运行整个函数体：调用交还其令牌，并在返回时先于其类别的排队调用取回一个令牌（`Admission::resume`）。声明为 `Returns` 的函数立即返回（`log_info`、`register_*` 注册表更新、启动分离会话、取消操作批次），保留其令牌，因此记录日志的客体不会为了刚刚持有的令牌而等待。因此即使比赛提交的数量远多于令牌，整批提交也都能推进：每个提交只在其客体真正计算时持有令牌。

携带令牌、类别、截止时间和深度的线程局部 `CallScope` 由守卫安装和恢复，因此 panic 的客体或宿主函数不会留下过期的作用域。

## 嵌套调用

`checker:resolve`、`checker:interpret` 和 `language:resolve`（`host_funcs/checker.rs`、`host_funcs/language.rs`）通过 `CallOptions::nested()` 回调插件。

- 它们运行在第二个客体线程上。内联运行会在 `block_on` 中嵌套 `block_on` 并 panic。
- 它们跳过准入。父调用挂起在该宿主函数中，因此嵌套调用像恢复的调用那样取得令牌，继承父调用的类别，并取父调用截止时间与自身调用超时中较早者。
- 深度上限为 `MAX_DEPTH`（2）。这阻止了 A 调 B 再调 A 的循环，并使每个已准入调用至多有一个嵌套调用，即线程预算为它保留的第二个线程。
- 同一实例永不重入：每次调用租用自己的实例，因此对同一插件的嵌套调用会得到第二个 store。

## 截止时间

每个调用都有截止时间。交互调用使用请求的截止时间（`crate::request_deadline`，由包在请求超时外的中间件为每个请求设置为该超时之前 `PLUGIN_DEADLINE_MARGIN`（1 秒），使无法完成的调用在超时层返回 408 之前先返回 503），钩子取它与 30 秒中较早者，定时器使用其认领租约，评测和操作使用 `plugin.call_timeout_secs`。

一个进程级线程（`packages/plugin-core/src/deadline.rs`）为每个运行中的调用维护 `(Instant, CancelHandle)` 堆，并在截止时间到达时取消客体。挂起在宿主函数中的客体在返回客体代码时陷入 trap。

extism 先向其计时线程发送 `TimerAction::Start`，然后才设置 store 的 epoch 截止时间（`set_epoch_deadline(1)`）。在两者之间被处理的取消会过早地推进 epoch，并同时删除该调用的计时条目，于是客体继续运行，之后重复的取消也都被忽略。因此截止时间线程在调用注册后至少 `START_GRACE` 才发出第一次取消，并持续重复，间隔从 `CANCEL_RETRY` 起倍增至 `CANCEL_RETRY_MAX`，直到调用返回；在第一次取消后 5 秒仍在运行的客体会被记录为取消丢失。清单超时（调用超时加 `TIMEOUT_BACKSTOP_MARGIN`）会结束截止时间线程从未取消的客体。丢失的取消没有后备：它同时删除了清单超时的计时条目。只有 extism 在注册调用之前设置 epoch 截止时间的修复才能消除这一问题。

宿主的长时间等待同样在调用的截止时间结束：`get_next_evaluate_result` 和 `get_next_operation_result` 把客体请求的等待时间截断到 `executor::current_deadline`。

在截止时间仍在排队或在截止时间被取消的调用以 `PluginError::Unavailable` 失败。

## Unavailable 永远不是一个决定

`PluginError::is_unavailable` 对表示插件此刻无法回答、而与请求本身无关的错误为真：`Unavailable`（截止时间）、`ShuttingDown`（服务器正在排空）、`NotLoaded`（插件未在本机加载）和 `InstanceBuild`（无法构建实例）。

- `visibility::query_plugins` 返回 `Result`。不可用的查询者变为 `AppError::PluginUnavailable`：带 `Retry-After: 2` 的 `503 PLUGIN_UNAVAILABLE`。它绝不会并入 `Deny`，否则列表会变成空页面、游标变成 400、单条读取变成 404。查询者的其他失败属于错误的回答，仍然拒绝整批资源。
- 不可用的阻塞钩子同样返回 503，而不是失败关闭的拒绝；不可用的通知钩子被跳过。
- 服务器端从不重试交互调用。Web 客户端把 503 视为临时状态（`packages/web-sdk/src/api/errors.ts` 中的 `isTemporaryApiError`），由 TanStack Query 退避重试。
- 后台调用方保留各自的语义：在租约结束时被截断的定时器投递在租约过期后重试，到达截止时间的评测调用是一个系统错误，由 SystemError 回收器重新评测。

## 评测扇出

服务器在评测机处未完成的测试点数量由 `dispatcher::fanout::Fanout` 限制：即整个集群每个存活评测机槽位 `TEST_CASES_PER_WORKER_SLOT`（2）个中本服务器所占的份额。`WorkerSlots` 是存活容量的唯一记录：评测机心跳中 `max_concurrency` 之和，以及存活服务器的数量（`common::liveness::live_workers_and_server_count`，对两种角色执行一次 `SCAN` 和一次流水线读取），由一个任务每 15 秒刷新一次。认领协程的进行中上限也是同样的份额：取配置的每服务器下限与本服务器在每个存活评测机槽位 8 个中所占份额的较大者。在看到任何容量之前，假定每个本地核心一个槽位。单个操作批次内的发布以固定的 32 个为窗口进行。

## 内存

池（`packages/plugin-core/src/pool.rs`）按需构建实例，并按最近使用优先保留归还的实例。内存由 `MemoryBudget`（`packages/plugin-core/src/memory.rs`）全局限制：

- 信号是进程的常驻集，来自 `/proc/self/statm`，在实例归还时至多每 50 毫秒读取一次。
- 一旦超出预算，按最久未使用的顺序在所有插件之间丢弃空闲实例，每个插件保留 `WARM_PER_PLUGIN` 个，直到降到低水位（预算的十分之九）以下。这一间隔使在预算附近徘徊的常驻集不会在每次归还时都触发驱逐。
- 在至少 `RECYCLE_MIN_CALLS` 次调用中搬运的调用输入输出超过 `MemoryBudget::recycle_after` 字节（预算的 1/64，32 MiB 到 512 MiB）的实例会在归还时被丢弃，保温实例也不例外：其分配器的高水位会随处理的数据增长，而 extism 并不暴露实例的内存大小。
- 重建实例只是实例化一个已缓存的模块。wasmtime 把编译好的模块保存在磁盘上的 `plugin.cache_dir` 中（`config::compile_cache_config`），因此重建只需几毫秒的反序列化，而无需编译（ICPC 插件约 300 毫秒）。同一插件的各实例不共享一个 wasmtime `Engine`：取消会推进其引擎的 epoch，这会使该引擎上的所有实例陷入 trap。
- 预算通过弱引用持有池，因此被重载替换的池在其最后一个租约归还之前仍会被裁剪。
- 预算为进程所在 cgroup 内存上限的 3/4（cgroup v2：`/proc/self/cgroup` 中的 `0::` 路径，然后是该 cgroup 或其最近的有上限父级的 `memory.max`；cgroup v1：`hierarchical_memory_limit`），没有上限时为 `MemTotal` 的 1/4，因为单机 compose 与其他服务共享机器。`plugin.memory_budget` 可以覆盖它。
- 每个实例的线性内存保留失控上限（`memory_max`），为预算的一半且至多 1.5 GiB。

`broccoli_plugin_instances_live` 和 `broccoli_plugin_calls_in_flight` 是按插件的仪表；`broccoli_plugin_admission_queued_total` 和 `broccoli_plugin_admission_wait_duration_seconds` 反映争用情况。

## 登录

Argon2 运行在自己的线程上（`utils/hash.rs`），每个核心一个，从不使用 tokio 的阻塞线程池，因此插件洪峰无法拖慢登录。

## 设置

`plugin.max_concurrent_calls`、`plugin.memory_budget` 和 `plugin.call_timeout_secs` 是仅有的调节项；前两项自动确定。定时器租约为调用超时加 `LEASE_MARGIN`（30 秒）。曾用于调节池和扇出的键列在 `config::REMOVED_KEYS` 中；启动时会对仍被设置的键发出警告。
