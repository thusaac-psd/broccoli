---
title: 路由
sidebar_label: 路由
sidebar_position: 2
---

# 路由

插件路由是服务器转发给插件某个导出函数的 HTTP 端点。你在 `plugin.toml` 中声明谁可以调用它，以及它的路径指向哪个比赛或题目。服务器在处理函数运行之前按照与自身比赛、题目端点相同的规则检查这两点，并把解析出的比赛交给处理函数。处理函数从不自行判断比赛是否可见或是否正在进行。

路由挂载在 `/api/v1/p/<plugin-id>` 之下，后接路由的 `path`。

## 声明路由

```toml
[[server.routes]]
method = "GET"
path = "/api/plugins/ioi/contests/{contest_id}/problems/{problem_id}/submission-status"
handler = "api_submission_status"
access = "participant"
contest = "{contest_id}"
problem = "{problem_id}"
contest_type = "ioi"
```

| 键 | 含义 |
| --- | --- |
| `method`、`path`、`handler` | HTTP 方法、路径模式以及响应它的导出函数。 |
| `access` | 谁可以调用该路由：`public`、`user`（默认）或 `participant`。 |
| `permission` | 调用者必须持有的权限，例如 `contest:manage`。 |
| `contest` | 指明路由所操作比赛的路径参数，写法与 `path` 中一致。 |
| `problem` | 指明题目的路径参数：路由声明了 `contest` 时为该比赛中的题目，否则为独立题目。 |
| `contest_type` | 路由唯一服务的比赛类型。需要同时声明 `contest`。 |

如果清单中这些键的组合是服务器无法执行的，插件不会加载，`broccoli-dev plugin build` 也会报告错误：`public` 路由指定了比赛、题目或权限，`participant` 未声明 `contest`，`contest_type` 未声明 `contest`，参数不在 `path` 中，或出现未知的键。

## 谁可以调用

| `access` | 调用者 |
| --- | --- |
| `public` | 任何人，无论是否登录。用于自行认证调用者的路由，例如打印插件的打印站端点。 |
| `user` | 已登录的用户。 |
| `participant` | 已在路由所属比赛中报名的已登录用户，或持有 `contest:manage` 的用户。 |

访问令牌无效、过期或已失效时，所有路由（包括 `public` 路由）都返回 `401`，以便网页客户端刷新令牌。用户的角色、密码或账号在令牌签发后发生变化，该令牌即失效。因此处理函数在 `req.auth` 中看到的始终是调用者当前的权限。

## 服务器检查的内容

以下情况在处理函数运行之前由服务器作答，与服务器针对同一资源的自身端点完全一致：

| 状态码 | 错误码 | 情形 |
| --- | --- | --- |
| `400` | `VALIDATION_ERROR` | 声明的 `contest` 或 `problem` 参数不是 ID。 |
| `401` | `TOKEN_MISSING`、`TOKEN_INVALID` | 需要令牌的路由未携带令牌，或令牌无效、已失效。 |
| `403` | `PERMISSION_DENIED` | 调用者缺少 `permission`，或在 `participant` 路由上不是公开比赛的参赛者。 |
| `404` | `NOT_FOUND` | 调用者无法读取该比赛或题目、资源不存在、题目不属于该比赛，或比赛属于其他 `contest_type`。 |
| `503` | `PLUGIN_UNAVAILABLE` | 比赛的可见性依赖于当前未加载的插件。带有 `Retry-After`。 |

未激活或已停用的比赛仅对 `contest:manage` 可见。私有比赛对其参赛者和 `contest:manage` 可见。

## 处理函数收到的内容

对于声明了 `contest` 的路由，`req.contest()` 是解析出的比赛；对于声明了 `problem` 的路由，`req.problem_id()` 是对应的题目：

```rust
fn handle_submission_status(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let user_id = req.require_user_id()?;
    let contest = req.contest()?;
    let problem_id = req.problem_id()?;
    if contest.phase != ContestPhase::Running {
        return Err(PluginHttpResponse::error(409, "The contest is not running").into());
    }
    // ...
}
```

`RouteContest` 包含 `id`、`contest_type`、`is_public`、`participant`（调用者是否已报名）和 `phase`。`phase` 是服务器的 `ContestPhase`：`Unactivated`、`Upcoming`、`Running`、`Ended` 或 `Deactivated`。直接与其比较，或使用 `has_started()`、`has_ended()` 以及供排行榜使用的 `stage()`（`"before"`、`"during"`、`"after"`）。不要在自己的 SQL 中根据 `start_time` 和 `end_time` 计算阶段。

在未声明 `contest` 的路由上调用 `req.contest()` 属于插件缺陷，请求会以内部错误失败。

其他路径参数（例如对局 ID 或提交 ID）由插件自行通过 `req.param::<T>("name")` 读取。缺失或无法解析的值返回 `400 VALIDATION_ERROR`。

## 错误

处理函数返回 `Result<PluginHttpResponse, ApiError>`，而 `ApiError` 只有两种：

- 客户端错误，通过 `PluginHttpResponse::error(status, message)` 构造。其响应体采用服务器的错误格式 `{"code": ..., "message": ...}`，错误码为服务器对该状态码使用的错误码（`404` 为 `NOT_FOUND`，`409` 为 `CONFLICT`，依此类推）。当前端需要区分同一状态码下的不同情形时，使用 `PluginHttpResponse::error_with_code(status, code, message)`。
- 内部错误：`?` 转换的所有 `SdkError`（例如查询失败），以及通过 `ApiError::internal(detail)` 包装的任何内容。详情写入服务器日志，调用者收到带通用消息的 `500 INTERNAL_ERROR`，永远不会看到详情。

插件路由返回的任何 `5xx` 都由服务器自己作答，因此即使处理函数手工构造了 `500` 响应，数据库或宿主的文本也不会出现在响应体中。

对于调用者无权查看的资源，返回与不存在的资源相同的 `404`，使路由不会向调用者透露哪些 ID 已被使用。
