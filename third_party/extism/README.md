# extism 1.41.0, patched

This is the published `extism` 1.41.0 crate (the Extism runtime and Rust SDK,
<https://github.com/extism/extism>, BSD-3-Clause, see `LICENSE`) with the
patches below. The root `Cargo.toml` points the workspace at it through
`[patch.crates-io]`; a path patch keeps offline bundle builds working. Every
file except this README and `LICENSE` is the published crate as it is, apart
from the patches. The upstream SDK documentation is the crate's page on
crates.io and docs.rs.

The patches make a call's cancel (`CancelHandle::cancel`) and its manifest
timeout bound every piece of guest code the call runs. All of them are in
`src/plugin.rs`.

## The deadline is armed before the call registers

`Plugin::raw_call` arms the store's epoch deadline (`epoch_deadline_trap` and
`set_epoch_deadline(1)`) before it sends `TimerAction::Start` to the timer
thread.

The timer thread handles a `Cancel`, or a call's manifest timeout, by removing
the call's entry and incrementing the engine epoch. `set_epoch_deadline(1)` sets
the deadline one past the engine's current epoch. With the published ordering,
`Start` first and the arm after it, a `Cancel` the timer thread handles between
the two increments the epoch first; the deadline then lands one past the
incremented epoch, and the entry is gone, so that cancel, every later cancel and
the manifest timeout are all lost and the guest runs unbounded. Arming first
means every epoch increment the timer thread makes for the call happens after
the arm and traps the guest.

## Instantiation runs inside the registered call

- `raw_call` registers the call (arm, then `Start`) right after replacing the
  store when it needs replacing and before it instantiates the guest. The
  published crate registers after instantiation, so the guest's start function
  and its `_initialize` (or `__wasm_call_ctors`, `hs_init`) run with no cancel
  and no timeout. The steps between registration and the export call move into
  `Plugin::prepare_call`, and every error from them stops the timer through
  `Plugin::stop_timer`, the same step a finished call takes.
- `relink` does not link the main module into the linker unless another module
  imports it. wasmtime instantiates a module linked that way as a reactor at
  once, so the published crate runs the main module's start function and
  `_initialize` while it builds the plugin, outside any call, in an instance no
  call uses. The main module is instantiated through `instance_pre` inside each
  call, as before.
- When instantiation or initialization fails, a cancelled one included, the next
  call starts from a fresh store. The published crate keeps an instance whose
  `_initialize` trapped and runs the next call on it without initializing it
  again.

## The test hook

The patch emits a trace event, `call registered with the timer`, on the calling
thread right after `Start` is sent.
`packages/plugin-core/tests/extism_cancel.rs` delivers a cancel from inside that
event and checks that the guest traps, in an export, in a start function and in
`_initialize`, that a zero manifest timeout stops a start function, and that the
call after a cancelled `_initialize` initializes the guest again. Against the
published crate each of these tests hangs or fails.

Upstream report: `<link to upstream issue>`.

## Removing the patch

Once a released extism contains these fixes: bump the `extism` version in the
root `Cargo.toml`, delete this folder and the `[patch.crates-io]` entry, and
refresh `Cargo.lock`. If the release does not emit the trace event, rework
`packages/plugin-core/tests/extism_cancel.rs` to deliver its cancels through
whatever the release offers, or drop the tests upstream carries an equivalent
of.
