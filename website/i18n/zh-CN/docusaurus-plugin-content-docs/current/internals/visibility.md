---
title: 可见性
sidebar_label: 可见性
sidebar_position: 7
---

# 可见性

调用者能否读取某个比赛范围内的资源，只在一个地方决定：`packages/server/src/visibility` 中的可见性内核。核心处理函数和插件路由都经过它，因此无论从哪里读取，比赛、题目或提交对同一调用者的呈现都相同。

## 内核

`VisibilityKernel::new(state, subject)` 每个请求为一个 `Subject` 构建一次。`decide_batch(action, resources)` 为每个资源给出一个 `Decision`（`Allow`、`Redact(mask)` 或 `Deny`）：

1. `host_rules::host_decide` 以固定数量的查询为整批资源应用宿主规则：激活时间窗、公开与私有比赛、报名、题目归属、提交归属。
2. 宿主未拒绝的每个资源，按比赛类型路由给已加载且声明了 `[[server.queries]] topic = "visibility"` 的插件，请求中带有资源的比赛 ID 和比赛阶段（`QueryResource`）。
3. 各方答复通过 `Decision::meet` 合并，结果只会收窄。

决策按内核实例缓存。`know_submission` 和 `know_contest` 把请求已经读取的行交给内核，避免重复读取。

## 比赛阶段

`utils::contest::contest_phase` 是唯一把比赛时间线转换为 `ContestPhase` 的函数。该枚举位于 `broccoli-types` 中，插件读取的是同样的取值：内核在可见性查询中为每个已解析的比赛附上阶段，代理在插件路由的 `RouteContest` 中提供阶段。插件从不在自己的 SQL 中推导阶段。

## 插件路由

代理（`handlers/proxy.rs`）先认证调用者并匹配路由，然后在调用插件之前，以路由经过检查的 `RouteAccess` 调用 `services::plugin_route_access::authorize`：

- 持有者令牌经过 `extractors::auth::fresh_user`，与 `FreshAuthUser` 使用同一条新鲜度规则，因此失效的令牌在这里同样返回 401。
- `permission` 依据令牌中的权限检查。
- 声明的比赛只读取一次（`find_contest`），对照路由的 `contest_type` 检查，通过 `know_contest` 交给内核，并与声明的题目在同一次 `decide_batch` 中决策。报名情况来自同一批次（`participates`），`participant` 路由应用 `participant_gate`，即 `require_contest_participant` 使用的规则。

`RouteAccess` 由 `ServerRouteConfig::access` 根据清单构建，它拒绝代理无法执行的任何键组合，因此一条路由要么带着代理能执行的规则加载，要么不加载。

插件返回的 `5xx` 响应会变为 `AppError::Internal`：服务器记录响应体，并返回不含该内容的 `500 INTERNAL_ERROR`。客户机 SDK 通过让调用失败（`run_api_handler`）把所有 `SdkError` 送入这条路径，因此内部文本永远不会到达调用者。

## 可见性插件缺失时

`plugin_query::visibility_queriers` 还会报告本副本无法判断的范围，即由未在本地加载的可见性插件管辖的资源：

- 已启用但未在本地加载的插件（加载失败、文件缺失、副本尚未收敛）：其上次加载时在 `Governs` 中记录的比赛类型；若未记录任何类型，则为全部资源。
- 已禁用或正在重新加载、且上次加载时作为某些比赛类型可见性插件的插件：这些比赛类型。

涉及无法判断资源的批次返回带 `Retry-After` 的 `503 PLUGIN_UNAVAILABLE`，绝不返回未遮蔽的数据，也绝不返回会被理解为“不存在”的 `Deny`。其他类型的比赛以及不属于受管辖比赛的独立题目仍可读取。
