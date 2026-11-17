---
title: Translating a plugin
sidebar_label: Translations
sidebar_position: 2
---

# Translating a plugin

The web UI speaks the language the user picked. A plugin ships a catalog per
language, and the host looks up the plugin's own text in it: the strings its
frontend shows, the names of what it registers, the reasons it refuses a
request, and the text of its manifest and config schemas. The host knows none
of these keys; it only knows where to look.

## Ship a catalog per language

List the catalogs in `plugin.toml`. `en` is the source; every other catalog
translates exactly its keys, with the same placeholders.

```toml
[translations]
en = "i18n/en.toml"
zh-CN = "i18n/zh-CN.toml"
```

```toml
# plugins/submission-limit/i18n/en.toml
'submission-limit.usedCount' = '{used} / {total} used'
```

```toml
# plugins/submission-limit/i18n/zh-CN.toml
'submission-limit.usedCount' = '已用 {used} / {total}'
```

```toml
# plugins/cooldown/i18n/en.toml
'cooldown.apiError.COOLDOWN_ACTIVE' = '{remaining_seconds, plural, one {Wait # more second before submitting again.} other {Wait # more seconds before submitting again.}}'
```

Every key starts with your plugin's id, so a plugin can never reword the
host's text or another plugin's. For the same reason a plugin id may not be a
namespace of the host's messages: the first segment of any key in the English
pack (`apiError`, `toast`, `nav`, …) and the key families `plugin`, `config`,
`identifier` and `apiError`, compared ignoring case. A plugin with such an id
does not load and its upload is refused. The only other keys a catalog may hold are
the text of your manifest (`plugin.<plugin id>.…` and `config.<plugin id>.…`,
below) and the names of what you register (`identifier.contestType.…`,
`identifier.problemType.…` and `identifier.checker.…`). A catalog with any
other key does not load, and an upload with one is refused; the error names
the keys. An identifier's name is shown only from the plugin the identifier
resolves to while that plugin is loaded: naming a contest type you do not
register shows nothing. A plugin that is disabled, or failed to load, keeps
only its manifest text, which the plugin list shows.

A frontend reads them with `t()` from `useTranslation()` in
`@broccoli/web-sdk/i18n`. `{name}` inserts a parameter.
`{count, plural, one {...} other {...}}` picks the form the language's plural
rules choose for `count`, and `#` inside a branch is the number. Dates and
relative times come from `useFormat()`, which formats for the user's language;
measure server timestamps against `useFormat().relative`, which uses the
server's clock.

## Add a language

The host's own messages are language packs: a plugin whose manifest declares
`[language_pack]` with the locale it provides. `plugins/broccoli-en` holds the
English messages, which are the keys every other language translates, and
`plugins/broccoli-zh-cn` holds Simplified Chinese. To add a language, ship a
pack in the same shape:

```toml
# plugin.toml
name = 'ja-language'
version = '0.1.0'

[language_pack]
locale = "ja"

[translations]
ja = "locales/ja.toml"
```

A language pack's catalog for its locale holds the host's own keys, so it is
not confined to its id; its catalogs for other locales are, as any plugin's
are. A plugin that ships translations without declaring `[language_pack]` is
an ordinary plugin. Install a pack like any plugin and the language appears in
the language menu, without rebuilding the web app.

One enabled pack provides a locale's host text. Enabling or uploading a second
pack for a locale another enabled pack provides is refused
(`LANGUAGE_PACK_TAKEN`): disable the first, then enable the second. Your own
keys and names stay yours whatever the pack says: a pack's text for a key under
your plugin's id is shown only where your catalog has none.

The web build embeds the packs in the repository, so the UI speaks them before
the server answers and while it is unreachable; what the server serves
overrides them. A key a pack lacks shows in English.

## Name what you register

The host shows contest types, problem types and checker formats by name. Add
the name under `identifier.<kind>.<id>`; without one the id itself is shown.

| What you register | Key |
| --- | --- |
| `register_contest_type("icpc", ...)` | `identifier.contestType.icpc` |
| `register_staged_evaluator("batch", ...)` | `identifier.problemType.batch` |
| `register_checker_resolver("exact", ...)` | `identifier.checker.exact` |

```toml
'identifier.contestType.icpc' = 'ICPC'
```

## Translate your refusals

A hook that rejects a submission returns a machine-readable code. The host
sends it under your plugin's id, so `SUBMISSION_LIMIT_EXCEEDED` from
`submission-limit` reaches the UI as
`submission-limit.SUBMISSION_LIMIT_EXCEEDED`. The UI shows the translation of
`<plugin id>.apiError.<CODE>` from your catalog, with the rejection's
`details` as parameters, and never the English `message`:

```rust
HookResponse::reject(
    Refusal::SubmissionLimitExceeded,
    format!("Submission limit reached ({count}/{max})"),
    429,
    Some(serde_json::json!({ "submissions_made": count, "max_submissions": max })),
)
```

```toml
'submission-limit.apiError.SUBMISSION_LIMIT_EXCEEDED' = 'You have used all {max_submissions} submissions for this problem.'
```

The code is one of the plugin's refusal codes, declared once with
`refusal_codes!`, and the plugin's test
`broccoli_server_sdk::testing::assert_refusal_codes_translated` fails while any
of them lacks a text in one of your packs (see
[Routes](./routes.md#errors)). A code with no translation falls back to a
generic message for its HTTP status class, never the host's text for a code of
the same name. The same lookup
applies to errors from your plugin's own HTTP routes. A rejection goes out as
`400`, `403`, `404`, `409`, `413` or `429`; any other status is sent as `400`.

## Translate your judging errors

A judgement or code run your plugin ends as a system error can carry an
`error_code`. The host records it under your plugin's id, so `NO_TEST_CASES`
from `icpc` is stored as `icpc.NO_TEST_CASES` and never collides with another
plugin's code or passes for one of the host's. The UI shows the translation of
`<plugin id>.apiError.<CODE>` from your catalog, and only that: a code your
catalog does not translate shows a generic system-error message, never the
host's text for a code of the same name. For example:

```rust
host.submission.update(&SubmissionUpdate::standing_system_error(
    req.submission_id,
    req.judgement_id,
    req.judge_epoch,
    "NO_TEST_CASES",
    "Problem has no test cases; cannot be judged",
))?;
```

```toml
'icpc.apiError.NO_TEST_CASES' = 'This problem has no test cases, so it cannot be judged. Ask the contest staff to add them.'
```

The host re-judges a system error while the submission's system-error budget
lasts, and the submission shows it only once the budget is spent. A failure
every attempt would repeat, such as a problem that cannot be judged as
configured, should stand at once instead: `standing_system_error` sets
`SubmissionUpdate::stands`, so the submission shows the error right away and
nothing judges it again until the problem changes and staff rejudge it. Leave
`stands` unset for a failure a later attempt might not meet. Never end such a
submission as `Accepted` with score 0: it would read as a solve.

## Translate your manifest and config schemas

The plugin list and the config forms show the manifest's `name` and
`description` and the schema's `title` and `description` fields. Each has a key
in your catalog; a missing key shows the text from `plugin.toml`.

| Text | Key |
| --- | --- |
| Plugin name | `plugin.<plugin id>.name` |
| Plugin description | `plugin.<plugin id>.description` |
| Config schema description | `config.<plugin id>.<namespace>.description` |
| Field title | `config.<plugin id>.<namespace>.<field path>.title` |
| Field description | `config.<plugin id>.<namespace>.<field path>.description` |

The field path joins nested property names with dots, so the field
`[config.sandbox.properties.compile_time_limit_s]` in `batch-evaluator` has
the title key `config.batch-evaluator.sandbox.compile_time_limit_s.title`.

```toml
'plugin.batch-evaluator.description' = 'Judges batch problems: compile, run against each test, check the output.'
'config.batch-evaluator.sandbox.description' = 'Sandbox resource limits for compilation and execution steps'
```
