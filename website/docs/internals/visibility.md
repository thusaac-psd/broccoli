---
title: Visibility
sidebar_label: Visibility
sidebar_position: 7
---

# Visibility

Who may read a contest-scoped resource is decided in one place, the visibility
kernel in `packages/server/src/visibility`. Core handlers and plugin routes both
go through it, so a contest, problem or submission shows the same to a caller
wherever it is read.

## The kernel

`VisibilityKernel::new(state, subject)` is built once per request for one
`Subject`. `decide_batch(action, resources)` answers one `Decision` (`Allow`,
`Redact(mask)` or `Deny`) per resource:

1. `host_rules::host_decide` applies the host rules in a fixed number of
   queries for the whole batch: activation windows, public and private
   contests, registration, problem membership, submission ownership.
2. Every resource the host did not deny goes to the loaded plugins that
   declare `[[server.queries]] topic = "visibility"`, routed by contest type,
   with its contest id and contest phase on the wire (`QueryResource`).
3. The answers are combined with `Decision::meet`, which never widens.

Decisions are memoized per kernel. `know_submission` and `know_contest` hand
the kernel rows the request already read, so it does not read them again.

## Contest phase

`utils::contest::contest_phase` is the only function that turns a contest's
timeline into a `ContestPhase`. The enum lives in `broccoli-types` so plugins
read the same values: the kernel stamps every resolved contest's phase on the
visibility query, and the proxy puts it in a plugin route's `RouteContest`.
Plugins never derive a phase in their own SQL.

## Plugin routes

The proxy (`handlers/proxy.rs`) authenticates the caller, matches the route,
and calls `services::plugin_route_access::authorize` with the route's checked
`RouteAccess` before it calls the plugin:

- The bearer token goes through `extractors::auth::fresh_user`, the same
  freshness rule `FreshAuthUser` applies, so a stale token is a 401 here too.
- `permission` is checked against the token's permissions.
- A declared contest is read once (`find_contest`), checked against the route's
  `contest_type`, handed to a kernel with `know_contest`, and decided together
  with the declared problem in one `decide_batch`. Registration comes from the
  same batch (`participates`), and `participant` routes apply
  `participant_gate`, the rule `require_contest_participant` uses.

`RouteAccess` is built from the manifest by `ServerRouteConfig::access`, which
refuses every combination of keys the proxy cannot enforce, so a route either
loads with a rule the proxy enforces or does not load.

A plugin response with a `5xx` status becomes `AppError::Internal`: the server
logs the body and answers `500 INTERNAL_ERROR` without it. The guest SDK sends
every `SdkError` down this path by failing the call (`run_api_handler`), so
internal text never reaches a caller.

## When a visibility plugin is missing

`plugin_query::visibility_queriers` also reports what this replica is blind
to: resources governed by a visibility plugin that is not loaded here.

- An enabled plugin that is not loaded here (it failed to load, its files are
  missing, the replica has not converged): the contest types its last load
  recorded in `Governs`, or everything when it recorded none.
- A disabled or reloading plugin that last loaded as the visibility plugin of
  some contest types: those contest types.

A batch that touches a blind resource is `503 PLUGIN_UNAVAILABLE` with
`Retry-After`, never the unmasked data and never a `Deny` that would read as
"not found". Contests of other types and standalone problems outside governed
contests stay readable.
