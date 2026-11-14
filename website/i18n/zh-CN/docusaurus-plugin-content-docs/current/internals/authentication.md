---
title: 身份认证
sidebar_label: 身份认证
sidebar_position: 10
---

# 身份认证

已登录的客户端持有一个短期的访问令牌，即以 `Authorization: Bearer` 发送的 JWT，以及一个长期的刷新令牌，用于换取新的访问令牌（使用 HttpOnly Cookie 调用 `POST /auth/refresh`，或在请求体中携带令牌调用 `POST /auth/cli-refresh`）。每个访问令牌都由 `services::auth_tokens::issue_access_token` 根据用户在数据库中的当前角色和权限签发。

## 新鲜度

当令牌所代表的内容终止时，令牌即失效：用户的密码、角色或账户发生变更（`user.credentials_changed_at`），或用户被删除。`extractors::auth::fresh_user` 是唯一的检查，仅需一次索引读取。失效的令牌返回 `401 TOKEN_INVALID`，客户端随即刷新，获得反映用户当前所持权限的令牌。

哪些请求需要承担这项检查，由请求方法决定：

| 请求 | 检查 |
| --- | --- |
| 核心路由上的 `POST`、`PUT`、`PATCH`、`DELETE` | `fresh_user`，经由 `AuthUser` |
| 核心路由上的 `GET`、`HEAD` | 仅校验签名和有效期 |
| `GET /auth/me` | `fresh_user`，经由 `FreshAuthUser` |
| 插件路由（`/p/...`）上的任意方法 | `fresh_user`，在代理中执行 |

`AuthUser` 依据请求方法作出判断，因此修改状态的路由的处理函数无法通过选用开销更小的提取器绕过检查；提交、运行代码以及其他消耗配额或修改状态的请求，都会在处理函数运行之前拒绝失效的令牌。集成测试 `every_state_changing_route_refuses_a_stale_token` 会向 OpenAPI 文档中每个需要认证的修改类操作发送失效令牌。

读请求保持无状态路径，因此热点读路由在认证时不访问数据库。失效的令牌因此最多还能读取一个访问令牌有效期，即 `jwt::ACCESS_TOKEN_TTL`（5 分钟）。需要依据调用者当前账户作答的读请求使用 `FreshAuthUser`。
