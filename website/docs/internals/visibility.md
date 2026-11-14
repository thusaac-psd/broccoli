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
`Redact(mask)`, `Withheld` or `Deny`) per resource:

1. `host_rules::host_decide` applies the host rules in a fixed number of
   queries for the whole batch: activation windows, public and private
   contests, registration, problem membership, submission ownership.
2. Every resource the host did not deny goes to the loaded plugins that
   declare `[[server.queries]] topic = "visibility"`, routed by contest type,
   with its contest id and contest phase on the wire (`QueryResource`).
3. The answers are combined with `Decision::meet`, which never widens:
   `Allow > Redact > Withheld > Deny`.

Plugins never narrow staff: a subject holding `contest:manage` or
`submission:view_all` (`Subject::narrowed_by_plugins`) gets the host decision
with no plugin consulted, and sees withheld feedback too. A visibility query
is never asked about staff.

A DTO reaches a response body only through `Visible::into_masked_json`, which
applies the decision. Every shape that goes through it implements `Governed`
(`visibility/shapes.rs`), naming the top-level fields the host decides alone.

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
to: resources governed by a visibility plugin that is not loaded here, each
with the `PluginOutage` that says why
(`PluginLifecycle::outage`, the one place that tells the two apart):

- An enabled plugin that is not loaded here (it failed to load, its files are
  missing, the replica has not converged): `unavailable`, for the contest types
  its last load recorded in `Governs`, or for everything when it recorded none.
  This also covers problems and submissions read outside a contest whose
  problem sits in one of those contests, since every other replica enforces the
  plugin there.
- A disabled plugin that last loaded as the visibility plugin of some contest
  types: `disabled`, for what is read through those contests only.

A querier that is loaded but cannot answer in time is `unavailable` for what it
governs in that request.

Degrading is per resource and per field, never per batch:

- A contest and a clarification are host records
  (`Resource::degrades_in_outage`); an outage takes nothing from them.
- A blind problem part or submission is `Decision::Withheld`. Its response
  shows only the fields its `Governed` shape names, every other field is
  `null`, and `withheld` is `{"reason": "plugin_outage", "outage": ...}`. A list
  keeps the row. A shape with no host-decided fields (a problem statement, its
  samples, its attachments) fails the request with the outage, and so does
  every action other than reading.
- An outage is `503 PLUGIN_DISABLED` (no `Retry-After`) or
  `503 PLUGIN_UNAVAILABLE` (with `Retry-After`). A disabled plugin's own routes
  answer `PLUGIN_DISABLED` too; a submission to a contest whose type its
  disabled plugin provides is `CONTEST_TYPE_DISABLED`.

`ContestResponse.plugin_outage` and `ContestListItem.plugin_outage` come from
`plugin_lifecycle::contest_type_outage`: the plugin providing the contest's type
is disabled, or no handler for it is loaded here. Clients use it to show a
contest whose submissions are refused and whose results are unavailable.

## Withheld outcomes

A submission's outcome is withheld from a viewer plugins narrow when a plugin
holds its feedback (`submission_feedback`) or a decision hides its `status`.
The submission is shown with its `Governed` fields, `status` `null` and
`withheld` `{"reason": "hidden"}`, so a client stops polling it. In SQL its
status is `NULL` (`submission_feedback::public_status`): no status filter
matches it. A status sort lists every row whose status is withheld as its own
group after the others, ordered by id alone, so its place says nothing about
its status.
