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
key. A manifest that declares the same `method` and `path` twice does not load
either, so a route's access is never replaced by a later declaration.

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
| `400` | `VALIDATION_ERROR` | A declared `contest` or `problem` parameter is not an id, or the route declares a problem of a contest that has not started (answered whether or not the problem is in the contest; `contest:manage` reads it early). |
| `401` | `TOKEN_MISSING`, `TOKEN_INVALID` | No token on a route that needs one, or a token that is invalid or stale. |
| `403` | `PERMISSION_DENIED` | The caller lacks `permission`, or is not a participant of a public contest on a `participant` route. |
| `404` | `NOT_FOUND` | The caller cannot read the contest or problem, it does not exist, the problem is not in the contest, or the contest has another `contest_type`. |
| `503` | `PLUGIN_DISABLED` | Your plugin, or the plugin that decides who may read the route's problem, is disabled. Carries a `Retry-After` of 30 seconds: it serves again once an administrator enables it. |
| `503` | `PLUGIN_UNAVAILABLE` | The plugin that decides who may read the route's problem cannot answer right now. Carries `Retry-After`. |

While a contest-type plugin is disabled or not loaded, the server keeps its
contests readable and marks them with `plugin_outage`. Problems and submissions
the plugin governs are listed with their plugin-governed fields `null` and a
`withheld` field saying why, and a read whose whole answer is the plugin's
answers one of the `503`s above. A visibility query is never asked about staff
(`contest:manage` or `submission:view_all`); they see what the host rules show.
A route that shows staff more than a contestant decides staff the same way,
with `req.is_staff()`, and a plugin frontend with `isStaff` from
`@broccoli/web-sdk/permissions`.

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
INVALID_PATH_PARAMETER` response.

## Errors

A handler returns `Result<PluginHttpResponse, ApiError>`, and an `ApiError` is
one of two things:

- A client error, built with `PluginHttpResponse::refuse(status, code,
  message)`. Its body is the server's error shape, `{"code": ..., "message":
  ...}`. `code` is one of your plugin's refusal codes, which you declare once
  with `refusal_codes!` (see below), and `message` is your English text for
  logs.
- An internal error: every `SdkError` that `?` converts, such as a failed
  query, and anything you wrap with `ApiError::internal(detail)`. The detail
  goes to the server log. The caller receives `500 INTERNAL_ERROR` with a
  generic message, never the detail.

The server sends a client error on under your plugin's id: a `COOLDOWN_ACTIVE`
from the `cooldown` plugin reaches the caller as `cooldown.COOLDOWN_ACTIVE`,
and its text comes from `cooldown.apiError.COOLDOWN_ACTIVE` in your
translation pack (see [Translations](./translations.md)). A plugin's code
therefore never reads as one of the server's own, such as `TOKEN_INVALID` or
`PLUGIN_UNAVAILABLE`, which the web client acts on. A client error goes out as
`400`, `403`, `404`, `409`, `413` or `429`; any other client error status is
sent as `400`. Whether the caller is signed in is the server's to say: declare
`access` in the manifest rather than answering `401`. `req.require_user_id()`
on a `public` route answers an anonymous caller `403 SIGN_IN_REQUIRED`.

Declare every code your routes and hooks refuse with in one enum, and pin with
one test that each has a text in every pack, so no refusal reaches a user as a
generic message that means something else:

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

The test reads your `plugin.toml` and the packs it declares, and also asks for
the codes the SDK itself answers your routes with (`INVALID_PATH_PARAMETER`,
`SIGN_IN_REQUIRED`).

A refusal names the numbers it is about with `.detail(name, value)`, which
takes only numbers and words. The client passes each to the code's text as a
parameter, so the pack text says what the limit is:

```rust
PluginHttpResponse::refuse(413, Refusal::TooManyPages, "too long")
    .detail("pages", pages)
    .detail("limit", max_pages)
```

```toml
'print.apiError.TOO_MANY_PAGES' = 'This document has {pages} pages; this contest prints at most {limit} pages.'
```

The server answers every `5xx` from a plugin route itself, so database or host
text can never reach a response body, even from a handler that builds a `500`
response by hand. A status that is neither a success nor a client error, such
as a redirect, is answered the same way.

A response is a status and a JSON body, nothing else: the server writes every
header itself. The body is always sent as `application/json`, with
`X-Content-Type-Options: nosniff` and a `Content-Security-Policy` that lets
nothing run, so a route can never set a cookie, redirect or serve a page on the
server's origin.

Answer `404` for a resource the caller may not see, the same as for one that
does not exist, so a route never tells a caller which ids are taken.

## Read a contest's results

A board or a view of a contest's results reads them through the SDK's contest
helpers, so every plugin reads them by the same rules:

- `contest::problems(host, contest_id)` gives the contest's problems in its
  order, each with the label the contest gives it. Name a board's columns by
  these labels, as the host's own contest pages do.
- `contest::standing_result("s")` is the SQL condition that submission row
  `s` holds its standing result. The host keeps a result that may still
  change (one in flight, or a system error the retry may still take back) at a
  status that is not terminal, so a board that reads only standing results
  never drops a cell to 0 while a submission is judged again.
- `contest::results_may_change(host, contest_id, phase)` says whether a board
  may still change: the contest runs, or a submission is still being judged.
  Answer it as the board's `live`, and the web SDK's `liveBoardPace` paces the
  board's refreshes by it.
- `visibility::hide_outcome()` is the visibility decision that withholds a
  submission's whole outcome from a viewer, as a scoreboard freeze or a
  feedback level that shows nothing does. Every client then reads the
  submission as withheld, never as judged without a result.
