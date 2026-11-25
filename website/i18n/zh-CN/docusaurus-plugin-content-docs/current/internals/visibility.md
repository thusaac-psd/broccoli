---
title: 可见性
sidebar_label: 可见性
sidebar_position: 7
---

# 可见性

调用者能否读取某个比赛范围内的资源，只在一个地方决定：`packages/server/src/visibility` 中的可见性内核。核心处理函数和插件路由都经过它，因此无论从哪里读取，比赛、题目或提交对同一调用者的呈现都相同。

## 内核

`VisibilityKernel::new(state, subject)` 每个请求为一个 `Subject` 构建一次。`decide_batch(action, resources)` 为每个资源给出一个 `Decision`（`Allow`、`Redact(mask)`、`Withheld` 或 `Deny`）：

1. `host_rules::host_decide` 以固定数量的查询为整批资源应用宿主规则：激活时间窗、公开与私有比赛、报名、题目归属、提交归属。
2. 宿主未拒绝的每个资源，按比赛类型路由给已加载且声明了 `[[server.queries]] topic = "visibility"` 的插件，请求中带有资源的比赛 ID 和比赛阶段（`QueryResource`）。
3. 各方答复通过 `Decision::meet` 合并，结果只会收窄：`Allow > Redact > Withheld > Deny`。

插件从不收窄管理人员：持有 `contest:manage` 或 `submission:view_all` 的主体（`Subject::narrowed_by_plugins`）直接得到主机的决定，不咨询任何插件，也能看到被暂扣的反馈。可见性查询从不会被问到管理人员。

DTO 只能经由 `Visible::into_masked_json` 进入响应体，该函数应用决定。经过它的每种响应形状都实现 `Governed`（`visibility/shapes.rs`），列出仅由主机决定的顶层字段。

插件的掩码总是生效。如果掩码置空了该形状的 API 类型要求存在的值（如 id、比赛题目的标签），这样的响应无法按要求提供，因为任何客户端都无法读取，所以内核改为对该查看者隐藏整个资源（`visible::admits`）并记录日志：查看者看到的只会比插件要求的更少，绝不更多。客户端收到的每个响应都能按其读取的类型解析。测试 `every_answer_a_mask_or_withholding_leaves_parses_as_its_api_type` 对每种形状逐一掩码每个路径、按每种原因隐藏，并解析实际返回的内容。

决策按内核实例缓存。`know_submission` 和 `know_contest` 把请求已经读取的行交给内核，避免重复读取。

## 比赛阶段

`utils::contest::contest_phase` 是唯一把比赛时间线转换为 `ContestPhase` 的函数。该枚举位于 `broccoli-types` 中，插件读取的是同样的取值：内核在可见性查询中为每个已解析的比赛附上阶段，代理在插件路由的 `RouteContest` 中提供阶段。插件从不在自己的 SQL 中推导阶段。

## 插件路由

代理（`handlers/proxy.rs`）先认证调用者并匹配路由，然后在调用插件之前，以路由经过检查的 `RouteAccess` 调用 `services::plugin_route_access::authorize`：

- 持有者令牌经过 `extractors::auth::fresh_user`，与每个会修改状态的核心请求使用同一项新鲜度检查，因此失效的令牌在这里同样返回 401，读请求也不例外。
- `permission` 依据令牌中的权限检查。
- 声明的比赛只读取一次（`find_contest`），对照路由的 `contest_type` 检查，通过 `know_contest` 交给内核，并与声明的题目在同一次 `decide_batch` 中决策。报名情况来自同一批次（`participates`），`participant` 路由应用 `participant_gate`，即 `require_contest_participant` 使用的规则。
- 声明了比赛中题目的路由在比赛的决策与题目的决策之间应用 `require_contest_started`，这与 `handlers::problem::require_problem_part_reachable` 对每个核心题目部分路由采用的顺序相同，因此在比赛开始前，无论题目是否在比赛中，两者都返回 `400`。

`RouteAccess` 由 `ServerRouteConfig::access` 根据清单构建，它拒绝代理无法执行的任何键组合，因此一条路由要么带着代理能执行的规则加载，要么不加载。

插件的应答经过 `plugin_answer`：宿主只发送其状态码和 JSON 响应体，所有响应头都由宿主自己写入。`2xx` 以 `application/json` 发送响应体。客户端错误成为 `services::plugin_output::PluginRefusal`，即以 `<plugin_id>.<CODE>` 表示的插件错误码，并带有插件可用于拒绝的状态码之一，与阻塞钩子的拒绝成为的类型相同。其他任何状态码都会变为 `AppError::Internal`：服务器记录响应体，并返回不含该内容的 `500 INTERNAL_ERROR`。客户机 SDK 通过让调用失败（`run_api_handler`）把所有 `SdkError` 送入这条路径，因此内部文本永远不会到达调用者。每个 `/api` 响应都带有 `X-Content-Type-Options: nosniff` 和不允许运行任何内容的 `Content-Security-Policy`。

## 可见性插件缺失时

`plugin_query::visibility_queriers` 还会报告本副本无法判断的范围，即由未在本地加载的可见性插件管辖的资源，并为每项附上说明原因的 `PluginOutage`（由 `PluginLifecycle::outage` 区分两种情况，这是唯一做此区分的地方）：

- 已启用但未在本地加载的插件（加载失败、文件缺失、副本尚未收敛）：`unavailable`，范围是其上次加载时在 `Governs` 中记录的比赛类型；若未记录任何类型，则为全部资源。这也包括在比赛之外读取、但其题目属于这些比赛的题目和提交，因为其他副本在那里仍执行该插件。
- 已禁用、且上次加载时作为某些比赛类型可见性插件的插件：`disabled`，范围仅限通过这些比赛读取的内容，并且只针对它使其关闭的比赛类型。若另一个已启用的插件提供该类型，该类型仍在服务，这与提交和认领遵循的规则相同（`PluginLifecycle::disabled_provider`），因此已禁用的插件在那里不扣留任何内容。

已加载但未能及时答复的查询方，在该请求中对其管辖的内容为 `unavailable`。

降级按资源、按字段进行，绝不按批次：

- 比赛和澄清是主机记录（`Resource::degrades_in_outage`），故障不会从中拿走任何内容。
- 无法判断的题目部分或提交为 `Decision::Withheld`。其响应只显示其 `Governed` 形状列出的字段，其余字段为 `null`，`withheld` 为 `{"reason": "plugin_outage", "outage": ...}`。列表保留该行。没有主机决定字段的形状（题面、样例、附件）会以该故障使请求失败，读取以外的所有操作也是如此。
- 故障分四种（由 `PluginLifecycle::outage` 区分，这是唯一区分它们的地方）：插件已禁用（`disabled`）；插件已启用但没有任何存活的副本在提供它（`not_loaded`，依据最近一次协调时各副本的报告判断）；插件已启用，另一个心跳新鲜的副本提供它而本副本没有（`not_loaded_here`，`PluginLifecycle::missing_here`）；插件已启用，本副本正在加载它（`unavailable`）。它们分别对应 `503 PLUGIN_DISABLED`、`503 PLUGIN_NOT_LOADED`、`503 PLUGIN_NOT_LOADED_HERE` 和 `503 PLUGIN_UNAVAILABLE`，均带 `Retry-After`：每个 503 都可重试。已禁用插件或未在任何服务器上加载的插件的答复要求 30 秒后再试（`DISABLED_RETRY_AFTER_SECS`），因为需要它的工作被暂存，并在管理员启用它或某个副本加载它后恢复；本副本缺少的插件要求 10 秒后再试（`NOT_LOADED_HERE_RETRY_AFTER_SECS`），届时随附的网关已将浏览器转到提供它的副本。`missing_here` 读取的正是就绪检查所读的同一条规则（`PluginLifecycle::missing_here_served_by_peer`），基于同一组心跳新鲜的副本，因此心跳过期的副本不会让本副本拒绝任何请求。这三种都在 `details.plugin_id` 中写明插件。插件自己的路由返回同样的答复，在缺少该插件文件的副本上也是如此（只有没有任何副本知道的插件才是 `404`）；向由已禁用插件提供比赛类型的比赛提交，返回 `CONTEST_TYPE_DISABLED`，同样写明插件。
- 请求中指明的键（比赛类型、题目类型、检查器格式或语言）依据集群记录的每个键检查（`AppState::known_keys`），而不只是本副本的注册表：插件出现故障的键按上述原因被拒绝，绝不会被当作未知。

`ContestResponse.plugin_outage` 和 `ContestListItem.plugin_outage` 来自 `plugin_lifecycle::contest_type_outage`：该比赛类型已关闭（`PluginLifecycle::key_closure`，对单个键的唯一关闭规则：所有提供者都已禁用，或其全集群提供者未在任何存活副本上加载），或本地没有加载其处理程序。客户端据此展示提交会被拒绝、结果不可用的比赛，除非只是本副本缺少该插件，否则会写明插件。

`SubmissionResponse.parked_by`、`SubmissionListItem.parked_by` 和 `ProblemResponse.closed_by` 来自停放工作的规则 `PluginLifecycle::closed_by`：使工作无法运行的已禁用插件，以及它为该工作提供的内容（`contest_type`，或题目类型评测程序的 `problem_type`）。被停放的提交会指明它；题目按读取时所用的比赛类型（所在比赛的类型，单独读取时为其 `default_contest_type`）指明它，因此无论是哪个插件，客户端都只显示一条提示，并且不提供服务器会拒绝的提交。

## 被暂扣的结果

对受插件收窄的查看者，当插件暂扣提交的反馈（`submission_feedback`）或某个决定隐藏其 `status` 时，提交的结果即被暂扣。提交只显示其 `Governed` 字段，`status` 为 `null`，`withheld` 为 `{"reason": "hidden"}`，客户端因此停止轮询。在 SQL 中其状态为 `NULL`（`submission_feedback::public_status`）：任何状态筛选都不匹配它。按状态排序时，所有状态被暂扣的行作为单独一组排在其余行之后，只按 id 排序，因此其位置不透露其状态。
