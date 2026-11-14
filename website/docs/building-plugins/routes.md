---
title: Routes
sidebar_label: Routes
sidebar_position: 2
---

# Routes

A plugin route is an HTTP endpoint the server forwards to one exported function
of your plugin. You declare in `plugin.toml` who may call it and which contest
or problem its path names. The server checks both before your handler runs, with
the same rules as its own contest and problem endpoints, and hands the handler
the contest it resolved. Your handler never decides whether a contest is visible
or running.

Routes are served under `/api/v1/p/<plugin-id>`, followed by the route's `path`.

## Declare a route

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

| Key | Meaning |
| --- | --- |
| `method`, `path`, `handler` | The HTTP method, the path pattern and the exported function that answers it. |
| `access` | Who may call the route: `public`, `user` (the default) or `participant`. |
| `permission` | A permission the caller must hold, such as `contest:manage`. |
| `contest` | The path parameter that names the contest the route acts on, written as it appears in `path`. |
| `problem` | The path parameter that names a problem: one of the route's contest when it declares `contest`, else a standalone problem. |
| `contest_type` | The only contest type the route serves. Needs `contest`. |

A manifest that combines these keys in a way the server cannot enforce does not
load, and `broccoli-dev plugin build` reports it: a `public` route that names a
contest, a problem or a permission, `participant` without `contest`,
`contest_type` without `contest`, a parameter missing from `path`, or an unknown
key.

## Who may call it

| `access` | Caller |
| --- | --- |
| `public` | Anyone, signed in or not. Use it for routes that authenticate their own callers, like the print plugin's station endpoints. |
| `user` | A signed-in user. |
| `participant` | A signed-in user registered in the route's contest, or one holding `contest:manage`. |

A request with an invalid, expired or stale access token is answered `401` on
every route, `public` ones included, so the web client refreshes the token. A
token is stale once the user's roles, password or account change after it was
issued. Your handler therefore always sees the caller's current permissions in
`req.auth`.

## What the server checks

The server answers these before your handler runs, exactly like its own
endpoint for the same resource:

| Status | Code | When |
| --- | --- | --- |
| `400` | `VALIDATION_ERROR` | A declared `contest` or `problem` parameter is not an id. |
| `401` | `TOKEN_MISSING`, `TOKEN_INVALID` | No token on a route that needs one, or a token that is invalid or stale. |
| `403` | `PERMISSION_DENIED` | The caller lacks `permission`, or is not a participant of a public contest on a `participant` route. |
| `404` | `NOT_FOUND` | The caller cannot read the contest or problem, it does not exist, the problem is not in the contest, or the contest has another `contest_type`. |
| `503` | `PLUGIN_UNAVAILABLE` | The contest's visibility depends on a plugin that is not loaded right now. Carries `Retry-After`. |

A contest that is not activated or already deactivated is visible to
`contest:manage` only. A private contest is visible to its participants and to
`contest:manage`.

## What the handler receives

`req.contest()` is the resolved contest of a route that declares `contest`, and
`req.problem_id()` the problem of a route that declares `problem`:

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

`RouteContest` carries `id`, `contest_type`, `is_public`, `participant` (whether
the caller is registered) and `phase`. `phase` is the server's `ContestPhase`:
`Unactivated`, `Upcoming`, `Running`, `Ended` or `Deactivated`. Compare against
it, or use `has_started()`, `has_ended()` and `stage()` (`"before"`, `"during"`,
`"after"`) for a scoreboard. Do not compute a phase from `start_time` and
`end_time` in your own SQL.

Calling `req.contest()` on a route that does not declare `contest` is a bug in
the plugin and fails the request as an internal error.

Other path parameters, such as a match or submission id, are yours to read with
`req.param::<T>("name")`. A missing or unparsable value is a `400
VALIDATION_ERROR` response.

## Errors

A handler returns `Result<PluginHttpResponse, ApiError>`, and an `ApiError` is
one of two things:

- A client error, built with `PluginHttpResponse::error(status, message)`. Its
  body is the server's error shape, `{"code": ..., "message": ...}`, with the
  server's code for the status (`404` is `NOT_FOUND`, `409` is `CONFLICT`, and so
  on). Use `PluginHttpResponse::error_with_code(status, code, message)` when your
  frontend needs to tell one condition apart from others with the same status.
- An internal error: every `SdkError` that `?` converts, such as a failed
  query, and anything you wrap with `ApiError::internal(detail)`. The detail
  goes to the server log. The caller receives `500 INTERNAL_ERROR` with a
  generic message, never the detail.

The server answers every `5xx` from a plugin route itself, so database or host
text can never reach a response body, even from a handler that builds a `500`
response by hand.

Answer `404` for a resource the caller may not see, the same as for one that
does not exist, so a route never tells a caller which ids are taken.
