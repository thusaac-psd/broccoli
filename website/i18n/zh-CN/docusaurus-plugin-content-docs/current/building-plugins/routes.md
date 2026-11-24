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

如果清单中这些键的组合是服务器无法执行的，插件不会加载，`broccoli-dev plugin build` 也会报告错误：`public` 路由指定了比赛、题目或权限，`participant` 未声明 `contest`，`contest_type` 未声明 `contest`，参数不在 `path` 中，或出现未知的键。同一 `method` 和 `path` 声明两次的清单同样不会加载，因此路由的访问规则永远不会被后面的声明替换。

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
| `400` | `VALIDATION_ERROR` | 声明的 `contest` 或 `problem` 参数不是 ID，或路由声明了尚未开始的比赛中的题目（无论题目是否在比赛中都如此回答；持有 `contest:manage` 的用户可以提前读取）。 |
| `401` | `TOKEN_MISSING`、`TOKEN_INVALID` | 需要令牌的路由未携带令牌，或令牌无效、已失效。 |
| `403` | `PERMISSION_DENIED` | 调用者缺少 `permission`，或在 `participant` 路由上不是公开比赛的参赛者。 |
| `404` | `NOT_FOUND` | 调用者无法读取该比赛或题目、资源不存在、题目不属于该比赛，或比赛属于其他 `contest_type`。 |
| `503` | `PLUGIN_DISABLED` | 你的插件，或决定谁可以读取该路由题目的插件，已被禁用。带 30 秒的 `Retry-After`：管理员重新启用后即恢复服务。 |
| `503` | `PLUGIN_UNAVAILABLE` | 决定谁可以读取该路由题目的插件当前无法答复。带有 `Retry-After`。 |

比赛类型插件被禁用或未加载时，服务器保持其比赛可读，并以 `plugin_outage` 标记。该插件管辖的题目和提交仍会列出，其插件管辖的字段为 `null`，并由 `withheld` 字段说明原因；整个答复都取决于该插件的读取会返回上表中的某个 `503`。可见性查询从不会被问到管理人员（`contest:manage` 或 `submission:view_all`），他们看到的是主机规则所显示的内容。向管理人员展示比参赛者更多内容的路由也以同样的方式判断管理人员：后端用 `req.is_staff()`，插件前端用 `@broccoli/web-sdk/permissions` 中的 `isStaff`。

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
        return Err(PluginHttpResponse::refuse(
            409,
            Refusal::ContestNotRunning,
            "The contest is not running",
        )
        .into());
    }
    // ...
}
```

`RouteContest` 包含 `id`、`contest_type`、`is_public`、`participant`（调用者是否已报名）和 `phase`。`phase` 是服务器的 `ContestPhase`：`Unactivated`、`Upcoming`、`Running`、`Ended` 或 `Deactivated`。直接与其比较，或使用 `has_started()`、`has_ended()` 以及供排行榜使用的 `stage()`（`"before"`、`"during"`、`"after"`）。不要在自己的 SQL 中根据 `start_time` 和 `end_time` 计算阶段。

在未声明 `contest` 的路由上调用 `req.contest()` 属于插件缺陷，请求会以内部错误失败。

其他路径参数（例如对局 ID 或提交 ID）由插件自行通过 `req.param::<T>("name")` 读取。缺失或无法解析的值返回 `400 INVALID_PATH_PARAMETER`。

## 错误

处理函数返回 `Result<PluginHttpResponse, ApiError>`，而 `ApiError` 只有两种：

- 客户端错误，通过 `PluginHttpResponse::refuse(status, code, message)` 构造。其响应体采用服务器的错误格式 `{"code": ..., "message": ...}`。`code` 是插件的拒绝码之一，用 `refusal_codes!` 统一声明（见下文），`message` 是写入日志的英文说明。
- 内部错误：`?` 转换的所有 `SdkError`（例如查询失败），以及通过 `ApiError::internal(detail)` 包装的任何内容。详情写入服务器日志，调用者收到带通用消息的 `500 INTERNAL_ERROR`，永远不会看到详情。

服务器以插件 ID 为前缀转发客户端错误：`cooldown` 插件的 `COOLDOWN_ACTIVE` 到达调用者时是 `cooldown.COOLDOWN_ACTIVE`，其文本来自翻译包中的 `cooldown.apiError.COOLDOWN_ACTIVE`（见[翻译](./translations.md)）。因此插件的错误码永远不会被当作服务器自己的错误码，例如 Web 客户端会据以行动的 `TOKEN_INVALID` 或 `PLUGIN_UNAVAILABLE`。客户端错误只以 `400`、`403`、`404`、`409`、`413` 或 `429` 发出；其他客户端错误状态码一律以 `400` 发出。调用者是否已登录由服务器判断：请在清单中声明 `access`，而不是返回 `401`。在 `public` 路由上，`req.require_user_id()` 对匿名调用者返回 `403 SIGN_IN_REQUIRED`。

把路由和钩子用到的每个拒绝码声明在一个枚举中，并用一个测试确保每个拒绝码在每个翻译包中都有文本，这样任何拒绝都不会以含义不符的通用提示出现在用户面前：

```rust
broccoli_server_sdk::refusal_codes! {
    pub enum Refusal {
        /// A token is spent while the contest is not running.
        ContestNotRunning = "CONTEST_NOT_RUNNING",
    }
}

#[test]
fn every_refusal_code_has_a_text_in_every_pack() {
    broccoli_server_sdk::testing::assert_refusal_codes_translated::<Refusal>();
}
```

该测试读取插件的 `plugin.toml` 及其声明的翻译包，并同时检查 SDK 自己对路由作出的拒绝码（`INVALID_PATH_PARAMETER`、`SIGN_IN_REQUIRED`）。

拒绝可以用 `.detail(name, value)` 给出它所涉及的数字，该方法只接受数字和词语。客户端把每一项作为参数传给该代码的文本，这样翻译包中的文本就能说明限制是多少：

```rust
PluginHttpResponse::refuse(413, Refusal::TooManyPages, "too long")
    .detail("pages", pages)
    .detail("limit", max_pages)
```

```toml
'print.apiError.TOO_MANY_PAGES' = '该文档共 {pages} 页，本场比赛每份最多打印 {limit} 页。'
```

插件路由返回的任何 `5xx` 都由服务器自己作答，因此即使处理函数手工构造了 `500` 响应，数据库或宿主的文本也不会出现在响应体中。既不是成功也不是客户端错误的状态码（例如重定向）同样如此处理。

响应只有状态码和 JSON 响应体：所有响应头都由服务器自己写入。响应体总是以 `application/json` 发送，并带有 `X-Content-Type-Options: nosniff` 和不允许运行任何内容的 `Content-Security-Policy`，因此路由永远无法在服务器的源上设置 Cookie、重定向或提供页面。

对于调用者无权查看的资源，返回与不存在的资源相同的 `404`，使路由不会向调用者透露哪些 ID 已被使用。

## 读取比赛结果

排行榜或比赛结果的其他视图通过 SDK 的比赛辅助函数读取结果，因此每个插件都按同样的规则读取：

- `contest::problems(host, contest_id)` 按比赛的顺序给出比赛的题目，以及比赛给每道题目的标签。请用这些标签命名排行榜的各列，与宿主自己的比赛页面一致。
- `contest::standing_result("s")` 是提交行 `s` 持有已确定结果的 SQL 条件。宿主把仍可能改变的结果（评测中的结果，或重试仍可能收回的系统错误）保持在非终态，因此只读取已确定结果的排行榜，不会在提交重新评测期间把某格降为 0。
- `contest::results_may_change(host, contest_id, phase)` 说明排行榜是否仍可能改变：比赛进行中，或仍有提交在评测。请把它作为排行榜的 `live` 返回，Web SDK 的 `liveBoardPace` 会据此安排排行榜的刷新节奏。
- `visibility::hide_outcome()` 是向查看者隐藏提交全部结果的可见性决定，与封榜或不显示任何信息的反馈级别一样。此后每个客户端都把该提交读作已隐藏，而不会读作已评测却没有结果。
- 改变查看者能看到提交哪些内容的插件操作（例如解锁隐藏结果的令牌），在完成后调用 Web SDK 的 `announceSubmissionVisibilityChange(submissionId)`。宿主随后重新读取该提交的每个视图，包括因结果被隐藏而停止刷新的视图。请根据你的路由给出的信息决定为哪些提交提供此类操作，而不要根据提交的状态：查看者读到的可能是已隐藏的状态。
