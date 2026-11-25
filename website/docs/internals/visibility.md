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

A plugin's mask always holds. A mask that blanks a value the shape's API type
requires (an id, a contest problem's label) cannot be served as asked, since no
client could read the answer, so the kernel withholds the whole resource from
that viewer instead (`visible::admits`) and logs it: the viewer sees less than
the plugin asked, never more. Every answer a client receives parses as the type
it reads. The test
`every_answer_a_mask_or_withholding_leaves_parses_as_its_api_type` masks each
path of each shape and withholds it for each reason, and parses what is served.

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
  freshness check every mutating core request applies, so a stale token is a
  401 here too, on reads as well.
- `permission` is checked against the token's permissions.
- A declared contest is read once (`find_contest`), checked against the route's
  `contest_type`, handed to a kernel with `know_contest`, and decided together
  with the declared problem in one `decide_batch`. Registration comes from the
  same batch (`participates`), and `participant` routes apply
  `participant_gate`, the rule `require_contest_participant` uses.
- A route that declares a problem of the contest applies
  `require_contest_started` between the contest's decision and the problem's,
  the sequence `handlers::problem::require_problem_part_reachable` gives every
  core problem part route, so before the start both answer `400` whether or
  not the problem is in the contest.

`RouteAccess` is built from the manifest by `ServerRouteConfig::access`, which
refuses every combination of keys the proxy cannot enforce, so a route either
loads with a rule the proxy enforces or does not load.

What the plugin answers goes through `plugin_answer`: the host sends only its
status and JSON body and writes every header itself. A `2xx` is the body as
`application/json`. A client error is a `services::plugin_output::PluginRefusal`,
the plugin's code as `<plugin_id>.<CODE>` with one of the statuses a plugin
may refuse with, the same type a blocking hook's rejection becomes. Any other
status becomes `AppError::Internal`: the server logs the body and answers
`500 INTERNAL_ERROR` without it. The guest SDK sends every `SdkError` down this
path by failing the call (`run_api_handler`), so internal text never reaches a
caller. Every `/api` response carries `X-Content-Type-Options: nosniff` and a
`Content-Security-Policy` that lets nothing run.

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
  types: `disabled`, for what is read through those contests only, and only
  for the types it keeps closed. A type another enabled plugin provides is
  served, by the same rule submissions and the claim follow
  (`PluginLifecycle::disabled_provider`), so the disabled plugin withholds
  nothing there.

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
- An outage is one of four (`PluginLifecycle::outage`, the one place they
  are told apart): the plugin is disabled (`disabled`), it is enabled and no
  live replica serves it (`not_loaded`, judged by the replicas' reports as of
  the last reconcile), it is enabled and a peer with a fresh heartbeat serves
  it but this one does not (`not_loaded_here`,
  `PluginLifecycle::missing_here`), or it is enabled and this replica is
  loading it now (`unavailable`). They
  answer `503 PLUGIN_DISABLED`, `503 PLUGIN_NOT_LOADED`,
  `503 PLUGIN_NOT_LOADED_HERE` and `503 PLUGIN_UNAVAILABLE`, each with
  `Retry-After`: every 503 is retryable. A disabled plugin's or a
  loaded-nowhere plugin's answer asks again after 30 seconds
  (`DISABLED_RETRY_AFTER_SECS`), since what needs it is parked and serves once
  an administrator enables it or a replica loads it; a plugin missing here
  asks again after 10 seconds (`NOT_LOADED_HERE_RETRY_AFTER_SECS`), by when
  the shipped gateways have moved the browser to a replica that serves it.
  `missing_here` reads the one rule readiness reads too
  (`PluginLifecycle::missing_here_served_by_peer`), over the same fresh peers,
  so a stale peer refuses nothing here. All three
  name the plugin in `details.plugin_id`. A plugin's own routes answer the
  same, on a replica that lacks the plugin's files too (only a plugin no
  replica knows is `404`); a submission to a contest whose type its disabled
  plugin provides is `CONTEST_TYPE_DISABLED`, which names the plugin too.
- A request that names a key (a contest type, problem type, checker format
  or language) is checked against every key the fleet records
  (`AppState::known_keys`), never this replica's registries alone: a key
  whose plugin is out is refused by why, as above, never as unknown.

`ContestResponse.plugin_outage` and `ContestListItem.plugin_outage` come from
`plugin_lifecycle::contest_type_outage`: the contest's type is closed
(`PluginLifecycle::key_closure`, the one closure rule for a key: every
provider disabled, or its fleet-wide provider loaded on no live replica), or
no handler for it is loaded here. Clients use it to show a contest whose
submissions are refused and whose results are unavailable, naming the plugin
unless only this replica lacks it.

`SubmissionResponse.parked_by`, `SubmissionListItem.parked_by` and
`ProblemResponse.closed_by` come from `PluginLifecycle::closed_by`, the rule
that parks work: the disabled plugin that keeps the work from running, and
what it provides for it (`contest_type`, or `problem_type` for the problem
type's evaluator). A parked submission names it; a problem names it for the
contest type it is read under (the contest's, or its `default_contest_type`
on its own), so clients show one notice and offer no submit the server would
refuse, whichever plugin it is.

## Withheld outcomes

A submission's outcome is withheld from a viewer plugins narrow when a plugin
holds its feedback (`submission_feedback`) or a decision hides its `status`.
The submission is shown with its `Governed` fields, `status` `null` and
`withheld` `{"reason": "hidden"}`, so a client stops polling it. In SQL its
status is `NULL` (`submission_feedback::public_status`): no status filter
matches it. A status sort lists every row whose status is withheld as its own
group after the others, ordered by id alone, so its place says nothing about
its status.
