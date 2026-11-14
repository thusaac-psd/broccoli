# extism 1.41.0, patched

This is the published `extism` 1.41.0 crate (the Extism runtime and Rust
SDK, <https://github.com/extism/extism>, BSD-3-Clause, see `LICENSE`) with one
patch. The root `Cargo.toml` points the workspace at it through
`[patch.crates-io]`; a path patch keeps offline bundle builds working. Every
file except this README and `LICENSE` is the published crate as it is, apart
from the patch below. The upstream SDK documentation is the crate's page on
crates.io and docs.rs.

## The patch

`Plugin::raw_call` (`src/plugin.rs`) arms the store's epoch deadline
(`epoch_deadline_trap` and `set_epoch_deadline(1)`) before it sends
`TimerAction::Start` to the timer thread, not after.

The timer thread handles a `Cancel`, or a call's manifest timeout, by
removing the call's entry and incrementing the engine epoch.
`set_epoch_deadline(1)` sets the deadline one past the engine's current
epoch. With the published ordering, a `Cancel` the timer thread handles after
`Start` but before the arm increments the epoch first; the deadline then
lands one past the incremented epoch, and the entry is gone, so that cancel,
every later cancel and the manifest timeout are all lost and the guest runs
unbounded. Arming first means every epoch increment the timer thread makes
for the call happens after the arm and traps the guest.

The patch also emits a trace event, `call registered with the timer`, on the
calling thread right after `Start` is sent.
`packages/plugin-core/tests/extism_cancel.rs` delivers a cancel from inside
that event and checks that the guest traps; with the published ordering the
same test hangs.

Upstream report: `<link to upstream issue>`.

## Removing the patch

Once a released extism contains the fix: bump the `extism` version in the
root `Cargo.toml`, delete this folder and the `[patch.crates-io]` entry, and
refresh `Cargo.lock`. If the release does not emit the trace event, rework
the test in `packages/plugin-core/tests/extism_cancel.rs` to deliver its
cancel through whatever the release offers, or drop it if upstream carries
an equivalent test.
