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

`packages/plugin-core/src/admission.rs` 为每台服务器持有 `C` 个 CPU 令牌，除非 `plugin.max_concurrent_calls` 另有设置，否则为每个可用核心 `TOKENS_PER_CORE`（2）个。低于 `MIN_TOKENS`（2，每个类别一个）的设置会被提升到该值并记录警告。令牌表示“正在运行客体代码”，而不是“调用进行中”。

| 类别 | 调用 | 份额 |
|---|---|---|
| `Interactive` | 可见性查询、请求钩子、插件 HTTP 路由 | `ceil(C/2)` |
| `Background` | 评测、操作、分离回调、定时器、后台钩子 | `floor(C/2)` |

- 一个类别可以借用另一个类别空闲的令牌。两个类别都在等待时释放的令牌交给低于其份额的类别，并在该类别内优先交给恢复的调用，因此在洪峰下任何一方都不会饿死另一方。
- 排队的等待者是一个不占用线程的异步 future。丢弃它（例如客户端已离开）即离开队列。
请求路径上的调用（`Request` 和 `Route`，即交互种类）按插件函数准入，每个函数是一条通道（lane）：同一函数的调用开销相近，而同一插件的两个函数开销可能相差上千倍。因此慢路由永远不会决定一次廉价可见性查询的命运。

- 一条通道至多有 `Capacity::lane_cap` 个已准入调用，即 `max(1, C/2)`。无论一个慢函数收到多少调用，另一半令牌都留给其他所有函数，包括同一插件的其他路由。
- 一个插件的全部 `Route` 调用（跨其所有路由）合计至多有 `Capacity::plugin_cap` 个已准入调用，即 `max(1, C - 2)`。令牌数不少于三个时，即使一个插件有许多慢路由，也总会为宿主的 `Request` 调用留下一个令牌，并为其他插件的路由留下一个。只有两个令牌时，它的路由至多占用一个，另一个共享，宿主调用优先。插件的 `Request` 工作（其可见性查询器、请求钩子）不计入此上限：这些调用是宿主为核心 API 发起的，由其自身的通道限制。
- 请求路径调用至多排队 `admission::SHED_WAIT`（2 秒），之后以 `PluginError::Unavailable` 失败，即带 `Retry-After` 的 503 `PLUGIN_UNAVAILABLE`。即使在只有廉价调用之后突然来了一批慢调用，也没有调用会在队列中等到请求截止时间。
- 通道已满时，如果预计该通道自身的队列需要超过 `SHED_WAIT` 才能排空，新调用会被立即拒绝：预计时间为该通道中排队的调用数加上它自己，乘以该通道已完成调用的平均耗时，再除以该通道已准入的调用数。尚无调用完成的通道不会立即拒绝任何调用。
- `Request` 调用，即核心 API 所依赖的宿主自身的可见性查询和请求钩子，先于 `Route` 调用（插件 HTTP 路由）获得释放的交互令牌。
- 后台调用从不被拒绝，一直排队到截止时间。

每个已准入的调用在返回之前都占用一个客体线程，无论是否挂起，因此每种 `CallKind` 都有自己的已准入调用上限：

| 种类 | 调用 | 上限 |
|---|---|---|
| `Request` | 可见性查询、请求钩子 | 每个令牌 `REQUESTS_PER_TOKEN`（4）个 |
| `Route` | 插件 HTTP 路由 | 每个令牌 `REQUESTS_PER_TOKEN`（4）个 |
| `Judging` | `on_submission`、`on_code_run` | 本服务器的进行中上限 |
| `Work` | 评测器、分离回调、定时器 | 本服务器的扇出上限 + `C` |
| `Hook` | 后台钩子 | `C` |
| `Lifecycle` | `init`，不需要令牌 | `LIFECYCLE_CAP`（4） |

各种类相互独立，因此一种类达到上限时不会阻挡另一种：为等待结果而挂起的比赛调用占满评测上限，而产生这些结果的评测器调用所在的工作上限不受影响。服务器使评测和工作上限始终等于其当前份额（`dispatcher::fanout::spawn_admission_sizing`）。

种类是必填参数。`CallOptions` 没有默认值，trait 也没有便捷的 `call`；每个调用点都要选择 `interactive`、`route`、`judging`、`background`、`background_hook`、`lifecycle` 或 `nested`。钩子的种类来自分发点（`hooks::dispatch_hooks`），因为同一个 `PluginHook` 既服务 `before_submission` 请求路径，也服务后台通知。

池的查找发生在准入之后，因此跨越一次重载而排队的调用会在新版本上运行。重载先完整构建新条目再替换旧条目，因此调用永远不会遇到处于两个版本之间的插件；重载失败时旧版本继续服务。

## 客体线程

客体从不在 tokio 的阻塞线程池上运行。它们运行在 `plugin_core::threads::GuestThreads` 上：它把任务交给空闲线程或新启动一个线程，从不排队。它也不需要排队：使用中的线程至多为每个已准入调用的每一层嵌套一个（`Caps::threads`，即各上限之和的 `MAX_DEPTH` 倍），因此调用从不在等待线程时持有令牌，线程数是各显式上限之和。空闲线程在 30 秒后退出。

## 挂起

每个宿主函数都在构建处（`host_funcs::host_function`）声明它是否等待客体之外的东西（`host_funcs::Waits`）。声明为 `Blocks` 的函数（SQL、存储、定时器、提交、配置、结果等待、批次启动、会访问 Redis 的取消、嵌套调用）在 `plugin_core::executor::parked` 中运行整个函数体：调用交还其令牌，并在返回时先于其类别的排队调用取回一个令牌（`Admission::resume`）。声明为 `Returns` 的函数立即返回（`log_info`、`register_*` 注册表更新、启动分离会话、取消操作批次），保留其令牌，因此记录日志的客体不会为了刚刚持有的令牌而等待。因此即使比赛提交的数量远多于令牌，整批提交也都能推进：每个提交只在其客体真正计算时持有令牌。

携带令牌、类别、截止时间和深度的线程局部 `CallScope` 由守卫安装和恢复，因此 panic 的客体或宿主函数不会留下过期的作用域。

插件 SQL 同样会挂起，因此等待数据库的调用可以多于令牌数。插件数据库连接池按令牌数确定大小，并且等待连接的时间以调用超时为限；规则见 `packages/server/src/database.rs` 中的 `PoolSizes`。每个插件的 SQL 在共享这一预算的自有连接池上运行，见[插件数据库访问](./plugin-database.md)。

## 注册的键

多个已加载的插件可以注册同一个键（比赛类型、题目类型、检查器格式、语言）：每个注册都会保留（`registry::Providers`），该键解析为插件 ID 排序最靠前的提供者。这一规则只取决于加载了哪些插件，与加载顺序无关，因此加载相同插件的服务器结果一致；卸载当前解析到的提供者时，该键交给下一个提供者，而不是被丢弃。

## 嵌套调用

`checker:resolve`、`checker:interpret` 和 `language:resolve`（`host_funcs/checker.rs`、`host_funcs/language.rs`）通过 `CallOptions::nested()` 回调插件。

- 它们运行在各自的客体线程上。内联运行会在 `block_on` 中嵌套 `block_on` 并 panic。
- 它们跳过准入。父调用挂起在该宿主函数中，因此嵌套调用像恢复的调用那样取得令牌，继承父调用的类别，并取父调用截止时间与自身调用超时中较早者。
- 深度上限为 `MAX_DEPTH`（3），即宿主发起的最深调用链：评测器解析检查器，testlib 检查器再解析其源代码的语言。该上限切断任何循环，并限定每个已准入调用占用的线程数（每层一个），线程预算为其预留这些线程。
- 同一实例永不重入：每次调用租用自己的实例，因此对同一插件的嵌套调用会得到第二个 store。

## 截止时间

每个调用都有截止时间。交互调用使用请求的截止时间（`crate::request_deadline`，由包在请求超时外的中间件为每个请求设置为该超时之前 `PLUGIN_DEADLINE_MARGIN`（1 秒），使无法完成的调用在超时层返回 408 之前先返回 503），钩子取它与 30 秒中较早者，定时器使用其认领租约，评测和操作使用 `plugin.call_timeout_secs`。

一个进程级线程（`packages/plugin-core/src/deadline.rs`）为每个运行中的调用维护 `(Instant, CancelHandle)` 堆，并在截止时间到达时取消客体。客体在下一次函数入口或循环迭代时陷入 trap；挂起在宿主函数中的客体在返回客体代码并到达这样的位置时陷入 trap。

工作区为 extism 打了补丁（`third_party/extism`），使调用的取消和清单超时约束该调用运行的所有客体代码。extism 在实例化客体之前设置 store 的 epoch 截止时间并向其计时线程注册调用，因此客体的 start 函数和 `_initialize` 与导出函数受同一取消和超时约束；构建实例时不实例化主模块；实例化失败（包括被取消）之后，下一次调用从新的 store 开始，而不是使用初始化了一半的实例。任何客体未正常返回的调用之后也是如此：trap（取消、超时、燃料耗尽、客体 panic）、宿主函数失败或 WASI 退出都可能使客体内存处于更新了一半的状态，因此 extism 在该实例的下一次调用之前重建 store，实例仍留在池中。客体通过输出返回的错误保留 store。被取消的调用与其他调用一样失败，宿主将其映射为 `PluginError::Unavailable`。`packages/plugin-core/tests/extism_cancel.rs` 检查这些情形。

在 extism 注册调用之前被处理的取消找不到要停止的调用。`deadline::watch` 在 `Plugin::call` 之前运行，extism 在持有实例并替换上一次调用留下的不可用 store 之后才注册调用；这段窗口内不运行客体代码。因此截止时间线程会持续重复调用的取消，间隔从 `CANCEL_RETRY` 起倍增至 `CANCEL_RETRY_MAX`，直到调用返回。清单超时（调用超时加 `TIMEOUT_BACKSTOP_MARGIN`）会结束截止时间线程从未取消的客体。

宿主的长时间等待同样在调用的截止时间结束：`get_next_evaluate_result` 和 `get_next_operation_result` 把客体请求的等待时间截断到 `executor::current_deadline`；插件 SQL 等待连接许可也不超过该时刻，每条语句都在该时刻被截断（见[插件数据库](plugin-database.md#截止时间)）。

在截止时间仍在排队或在截止时间被取消的调用以 `PluginError::Unavailable` 失败。

以不可用失败的调用自身不记录日志：由调用方作答，标记它的宿主函数为其原因记录一行。其他失败的调用记录一条 `Plugin call failed` 警告及其原因（`failure_reason`），即错误的每一层原因，不含 wasm 回溯。extism 只在 `debug` 级别记录失败的调用。

## 插件日志行

每一行携带插件文本的日志都经过同一个汇聚点 `plugin_core::guest_log`：客体通过 extism 的 `log_*` 导入（无论权限如何，都链接进每个实例）和 `logger` 能力的 `log_info` 记录的行，以及宿主关于某个插件的每一行（`guest_log::about`）：`Plugin call failed` 警告、路由的服务器错误响应体、钩子的失败、可见性查询的失败，以及插件语句自身的错误（`host_db::refused`）。每行以 Broccoli 插件 id 标明插件（`plugin_id`，取自调用作用域，而不是 extism 的实例 id）。插件的文本按 `guest_log::plugin_text` 的方式写出：每个控制字符都被转义（`\n`、`\r`、`\t`、`\u{1b}`），因此插件无法在 text 和 pretty 格式中画出新行、颜色或伪造的宿主日志行；然后在字符边界处截断到 `MAX_LINE_BYTES`（2 KiB），注明省略了多少字节。失败调用的原因会出现在关于该失败的每个答复和日志行中，也按同样方式处理。每个插件有 `BURST` 行（100）的额度，以每秒 `PER_SECOND` 行（20）补充，客体的行一份额度，宿主关于它的行另一份额度，因此刷屏的插件永远无法淹没宿主关于其失败的警告：超出额度的行被丢弃，第一次丢弃时记录一条警告，说明该插件的日志超出速率，下一条被接纳的行之前会有一条说明丢弃了多少行。所有这些行都进入 `plugin` 目标，级别至多为 `WARN`：客体的 `error` 和 `warn` 都以 `WARN` 级别记录，并以 `guest_level` 标明它所请求的级别；宿主关于插件的行是 `WARN`，因此没有插件能写出看起来像宿主自身错误的行。插件未能完成请求所需的工作时，答复为 `AppError::PluginFailed`：客户端看到宿主的内部错误，插件的文本只进入宿主关于该插件的日志行。`tests/integration/source_invariants.rs` 检查在宿主调用插件的地方，没有 ERROR 级别的行写明插件，也没有内部错误携带插件的答复。


## 已卸载的实例

每个已加载插件的实例池都带有一个 `pool::Liveness`，在插件被卸载之前一直有效（`unload_plugin` 使其失效）。重新加载会替换实例池但不让旧池失效，因此跨越重新加载的调用会在它开始时的版本上完成；新版本接管旧的实例池（`PluginEntry::succeed`），因此卸载会让该插件的调用仍在运行的每个实例池失效，无论调用开始于哪个版本。新版本只接管仍有调用在其上运行的被替换实例池；调用都已结束的实例池在下一次重新加载时被丢弃。调用在注册表锁下查到实例池的那一刻起（早于其客体线程启动）就持有该实例池的有效性，因此任何重新加载都不会丢弃一个即将有调用在其上运行的实例池。未生效的重新加载（加载失败，或集群拒绝了它）会放回旧版本，并只让被拒绝版本的实例池失效（`PluginEntry::discard`）。每一次卸载，无论是禁用还是集群遗忘该插件，都经过 `plugin_lifecycle::deactivate`，它同时切断该插件的评测会话并丢弃其 SQL 连接池。

插件被卸载时已在运行的调用会保留其实例直到客体返回，但它再也无法访问宿主：调用作用域携带实例的有效性，构建每个宿主函数的 `host_funcs::host_function` 拒绝已失效实例的每个宿主函数调用（`executor::instance_retired`），并使该调用以不可用失败。已查到实例池但尚未租用实例的调用以 `PluginError::NotLoaded` 失败。它的插件 SQL 也以同样方式被拒绝，而且是在开通所持有的锁之下，因此过期的调用永远不会重新开通被卸载流程停用的角色。
## Unavailable 永远不是一个决定

`PluginError::is_unavailable` 对表示插件此刻无法回答、而与请求本身无关的错误为真：`Unavailable`（截止时间）、`ShuttingDown`（服务器正在排空）、`NotLoaded`（插件未在本机加载）和 `InstanceBuild`（无法构建实例）。

- `visibility::query_plugins` 返回 `Result`。不可用的查询者变为 `AppError::PluginUnavailable`：带 `Retry-After: 2` 的 `503 PLUGIN_UNAVAILABLE`。它绝不会并入 `Deny`，否则列表会变成空页面、游标变成 400、单条读取变成 404。由未加载的可见性插件管辖的资源同样返回该 503（参见[可见性](visibility.md)）。查询者的其他失败属于错误的回答，仍然拒绝整批资源。
- 不可用的阻塞钩子同样返回 503，而不是失败关闭的拒绝；不可用的通知钩子被跳过。
- 服务器端从不重试交互调用。Web 客户端把 503 视为临时状态（`packages/web-sdk/src/api/errors.ts` 中的 `isTemporaryApiError`），由 TanStack Query 退避重试。
- 后台调用方保留各自的语义：在租约结束时被截断的定时器投递在租约过期后重试，到达截止时间的评测调用是一个系统错误，由 SystemError 回收器重新评测。

## 评测扇出

服务器在评测机处未完成的测试点数量由 `dispatcher::fanout::Fanout` 限制：即整个集群每个存活评测机槽位 `TEST_CASES_PER_WORKER_SLOT`（2）个中本服务器所占的份额。`WorkerSlots` 是存活容量的唯一记录：评测机心跳中 `max_concurrency` 之和，以及存活服务器的数量（`common::liveness::live_workers_and_server_count`，对两种角色执行一次 `SCAN` 和一次流水线读取），由一个任务保持最新：每当评测机宣告自己加入、离开、开始或停止接收任务时（`common::liveness::changes_channel`），在最早可能过期的存活评测机键过期之后立即，以及至少每 15 秒，它都会重新读取一次。用于确定规模的存活信息不再有其他读取方。由它派生的每个数值都只有一个归属：

| 数值 | 归属 | 取值 |
|---|---|---|
| 扇出上限 | `dispatcher::fanout::fan_out_limit` | 每个存活槽位 2 个中本服务器所占的份额 |
| 进行中上限 | `dispatcher::claim::in_flight_cap` | 每个存活槽位 8 个中本服务器所占的份额，至少 256 |
| 队列上限 | `dispatcher::queue_depth::derived_queue_cap` | 整个集群每个存活槽位 200 个，至少 5000 |

在看到任何容量之前，扇出假定每个本地核心一个槽位，其余数值取下限。单个操作批次内的发布以固定的 32 个为窗口进行。

## 内存

池（`packages/plugin-core/src/pool.rs`）按需构建实例，并按最近使用优先保留归还的实例。内存由 `MemoryBudget`（`packages/plugin-core/src/memory.rs`）全局限制：

- 信号是进程的常驻集，来自 `/proc/self/statm`，在实例归还时至多每 50 毫秒读取一次。
- 一旦超出预算，按最久未使用的顺序在所有插件之间丢弃空闲实例，每个插件保留 `WARM_PER_PLUGIN` 个，直到降到低水位（预算的十分之九）以下。这一间隔使在预算附近徘徊的常驻集不会在每次归还时都触发驱逐。
- 在至少 `RECYCLE_MIN_CALLS` 次调用中搬运的调用输入输出超过 `MemoryBudget::recycle_after` 字节（预算的 1/64，32 MiB 到 512 MiB）的实例会在归还时被丢弃，保温实例也不例外：其分配器的高水位会随处理的数据增长，而 extism 并不暴露实例的内存大小。
- 重建实例只是实例化一个已缓存的模块。wasmtime 把编译好的模块保存在磁盘上的 `plugin.cache_dir` 中（`config::compile_cache_config`），因此重建只需几毫秒的反序列化，而无需编译（ICPC 插件约 300 毫秒）。同一插件的各实例不共享一个 wasmtime `Engine`：取消会推进其引擎的 epoch，这会使该引擎上的所有实例陷入 trap。
- 预算通过弱引用持有池，因此被重载替换的池在其最后一个租约归还之前仍会被裁剪。
- 预算为进程所在 cgroup 内存上限的 3/4（cgroup v2：`/proc/self/cgroup` 中的 `0::` 路径，然后是该 cgroup 或其最近的有上限父级的 `memory.max`；cgroup v1：`/proc/self/cgroup` 所指内存 cgroup 的 `memory.stat` 中的 `hierarchical_memory_limit`，容器只能看到自身 cgroup 时取层级根的该值），没有上限时为 `MemTotal` 的 1/4，因为单机 compose 与其他服务共享机器。`plugin.memory_budget` 可以覆盖它。
- 每个实例的线性内存保留失控上限（`memory_max`），为预算的一半且至多 1.5 GiB。

`broccoli_plugin_instances_live` 和 `broccoli_plugin_calls_in_flight` 是按插件的仪表；`broccoli_plugin_admission_queued_total` 和 `broccoli_plugin_admission_wait_duration_seconds` 反映争用情况。

## 登录

Argon2 运行在自己的线程上（`utils/hash.rs`），每个核心一个，从不使用 tokio 的阻塞线程池，因此插件洪峰无法拖慢登录。

## 设置

`plugin.max_concurrent_calls`、`plugin.memory_budget` 和 `plugin.call_timeout_secs` 是仅有的调节项；前两项自动确定。定时器租约为调用超时加 `LEASE_MARGIN`（30 秒）。任何二进制程序都不读取的设置（例如曾用于调节池或扇出的键）会被忽略，并在启动时由一条警告列出（`common::config::warn_unknown_keys`）。
