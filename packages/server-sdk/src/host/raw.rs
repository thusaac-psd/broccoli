use extism_pdk::host_fn;

// Host functions with NO return value. The `#[host_fn]` macro is fine here: it
// frees the input (via `ManagedMemory`) and there is no return value to leak.
#[host_fn]
extern "ExtismHost" {
    pub fn log_info(msg: String);
    pub fn register_contest_type(input: String);
    pub fn register_evaluator(input: String);
    pub fn register_checker_resolver(input: String);
    pub fn cancel_evaluate_batch(input: String);
    pub fn cancel_operation_batch(input: String);
    pub fn register_language_resolver(input: String);
    pub fn store_set(input: String);
    pub fn store_delete(input: String);
    pub fn config_set(input: String);
}

// ---------------------------------------------------------------------------
// Data-returning host functions are bound MANUALLY (not via `#[host_fn]`).
//
// extism-pdk 1.4.1's `#[host_fn]` macro reads a host function's return value
// with `Memory::from(offset).to_vec()` but never calls `.free()` on it, so the
// extism memory block holding each return is retained until the *plugin call*
// ends. wasmtime's linear memory only grows (never shrinks) and the extism pool
// never evicts instances, so within a single plugin call the returns of every
// host call ratchet the instance's linear memory up to the sum of all return
// sizes, and that high-water mark is then pinned for the whole process lifetime.
// For the streaming checker (~one `blob_read_range` per ~1 MB of test data) a
// single 30 MB comparison alone leaked ~80 MB; across all evaluator/contest host
// calls, testcases and concurrency it accumulates to gigabytes and OOM-kills the
// server.
//
// The macro below mirrors exactly what `#[host_fn]` generates for a
// `(String, ...) -> String` import, but additionally frees both the input
// memory and the return memory, keeping each instance's linear-memory high-water
// at a single in-flight value.
// ---------------------------------------------------------------------------
//
// Gated to wasm32 along with its sole invocation below: on host builds
// nothing calls this macro (see the comment on the invocation), and an
// unused `macro_rules!` definition warns just like an unused function would.
#[cfg(target_arch = "wasm32")]
macro_rules! str_host_fns {
    ( $( $name:ident ( $($arg:ident),+ $(,)? ) ),+ $(,)? ) => {
        mod raw_imports {
            #[link(wasm_import_module = "extism:host/user")]
            unsafe extern "C" {
                $(
                    #[link_name = stringify!($name)]
                    pub unsafe fn $name( $($arg: u64),+ ) -> u64;
                )+
            }
        }

        $(
            #[doc = concat!("Manual binding for host fn `", stringify!($name),
                "` that frees the host return memory after reading it.")]
            ///
            /// # Safety
            /// FFI into the Extism host runtime; only valid inside a plugin call.
            pub unsafe fn $name( $($arg: String),+ ) -> Result<String, extism_pdk::Error> {
                // Copy each input into extism memory; keep the handles so we can
                // free them after the call.
                $( let $arg = extism_pdk::Memory::from_bytes($arg.as_bytes())?; )+
                let __out_offset = unsafe { raw_imports::$name( $( $arg.offset() ),+ ) };
                $( $arg.free(); )+
                let __out = extism_pdk::Memory::from(__out_offset);
                let __s = __out.to_string()?;
                __out.free();
                Ok(__s)
            }
        )+
    };
}

// Gated to match their only consumers: every one of these 24 wrappers is
// called exclusively from `#[cfg(target_arch = "wasm32")]`-gated code in
// `sdk/db.rs`, `sdk/eval.rs`, `sdk/config.rs`, `sdk/submissions.rs`,
// `sdk/operations.rs`, `sdk/checker.rs`, `sdk/language.rs` and
// `sdk/registry.rs` (the host-target `Mock` counterparts hold their own
// in-memory state and never reach into these FFI wrappers). Without the gate
// they are unused on host targets and every plugin's host-target build
// (`cargo test`, `cargo clippy`) emits a "function is never used" warning for
// each. Same root cause as the `push_judge_sets` re-export fixed for E-2:
// `-D warnings` only lints the crate under test, and no workspace-scoped
// gate builds `broccoli-server-sdk` with the `guest` feature that plugins use.
#[cfg(target_arch = "wasm32")]
str_host_fns! {
    db_query(sql, args),
    db_execute(sql, args),
    db_begin(input),
    db_query_in(txn_id, sql, args),
    db_execute_in(txn_id, sql, args),
    db_commit(txn_id),
    db_rollback(txn_id),
    start_evaluate_batch(input),
    start_detached_windowed_evaluate(input),
    get_next_evaluate_result(input),
    cancel_evaluate_test_cases(input),
    start_operation_batch(input),
    start_detached_windowed_operation(input),
    get_next_operation_result(input),
    resolve_checker(input),
    interpret_checker_result(input),
    resolve_language(input),
    store_get(input),
    store_compare_and_set(input),
    config_get(input),
    submission_update(input),
    submission_insert_results(input),
    submission_delete_results(input),
    submission_query_test_cases(input),
}
