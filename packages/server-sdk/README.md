# broccoli-server-sdk

SDK for building [Broccoli](https://github.com/THUSAAC-PSD/broccoli) online
judge WASM plugins (backend).

## Feature flags

| Feature | Default | Description                                                                                                       |
| ------- | ------- | ----------------------------------------------------------------------------------------------------------------- |
| `guest` | off     | Enables host function wrappers (`db`, `evaluator`, `host`) and the `WasmHost` runtime for use inside WASM plugins |

Without `guest`, only shared types, traits, and error definitions are available
(useful for host-side code that needs the same type definitions).

## Usage

Add the dependency with the `guest` feature in your plugin's `Cargo.toml`:

```toml
[dependencies]
broccoli-server-sdk = { version = "0.1", features = ["guest"] }
```

Then import the prelude:

```rust
use broccoli_server_sdk::prelude::*;
```

### Evaluation cancellation

Evaluation plugins can cancel outstanding work through a `WindowedEvalSession`:

- `cancel_test_cases(&[test_case_id])` on `WindowedEvalSession` cancels specific
  active testcases and drops matching queued testcases. It is intended for
  scoring short-circuits such as IOI `GroupMin` and `GroupMul`.
- `cancel_all()` cancels active evaluate batches and clears queued testcases.

A detached eval session cancels outstanding work by returning the callback
output action `cancel`, or by listing specific pending/active test cases in the
callback output. Synchronous operation `collect(...)` cancels its active
operation batches internally on timeout or host error.

### Detached windowed callbacks

Long-running judging exports should avoid polling worker results while holding a
WASM plugin instance. The detached windowed eval API starts bounded work on the
host, returns a session id immediately, and invokes a named plugin export for
each result, timeout, or exhausted session event:

```rust
host.eval
    .windowed(&batch)
    .concurrency(8)
    .fire_after_judging_for_submission(&submission)
    .start_detached("on_eval_result", serde_json::json!({}))?;
```

The callback input and output are the shared SDK types
`DetachedEvaluateCallbackInput` and `DetachedEvaluateCallbackOutput`. Callbacks
return updated opaque `state`, an action (`continue`, `finish`, or `cancel`), a
`refill` decision, and optional active/pending work to cancel. The detached
result timeout defaults from the testcase time limits; callers can override it
with `.result_timeout_ms(...)`. Submission-judging eval flows should call
`.fire_after_judging_for_submission(...)` with the submission request; the
server then emits the normal `after_judging` hook after the terminal detached
callback persists results, subject to the original dispatch's current-judgement
hook policy.

Detached refill policy lives in the callback output because the host continues
the window after the original plugin export has returned:

```rust
let output = DetachedEvaluateCallbackOutput::continue_with(next_state)
    .refill_while(&input, |result| result.verdict == Verdict::Accepted);
```

Worker operations run through a windowed builder that collects the results in
the calling export:

```rust
let results = host
    .operations
    .windowed(&operations)
    .concurrency(4)
    .collect(result_timeout_ms)?;
```

`collect` holds the current plugin export while the operations run, so a judging
flow that waits on many results uses the detached eval session above.

Synchronous eval sessions use the same builder naming, but intentionally do not
offer a `collect` shortcut:

```rust
let mut session = host.eval.windowed(&batch).concurrency(4).start()?;
while let Some(result) = session.next_result(result_timeout_ms)? {
    // persist or score result
}
```

## License

MIT
