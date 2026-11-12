pub mod operation_dlq;
pub mod operation_result;

pub use operation_dlq::consume_operation_dlq;
pub use operation_result::consume_operation_results;

use futures::FutureExt;

/// Run a `process_messages` handler future under a panic guard.
///
/// The vendored `broccoli_queue::process_messages(Some(N))` spawns N detached
/// consume tasks into a `FuturesUnordered` that it NEVER repolls, so a task that
/// panics is not drained and its slot is never refilled: one panicking handler
/// permanently removes a consume slot. For the DLQ consumer (`Some(1)`) that
/// silently halts all failed-job persistence; for the result consumer it quietly
/// erodes throughput. Catching the panic here keeps the consume task alive.
///
/// A caught panic returns `Ok(())` (a clean acknowledge), NOT `Err`: a panic is
/// almost always deterministic for the message, so rejecting it would only burn
/// the retry budget re-panicking before it lands in a never-drained failed
/// queue. Acknowledging drops the poison message without wedging or leaking it.
/// The panic is logged loudly.
pub(crate) async fn guard_handler<F>(handler: &'static str, fut: F) -> Result<(), mq::BroccoliError>
where
    F: std::future::Future<Output = Result<(), mq::BroccoliError>>,
{
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(result) => result,
        Err(_panic) => {
            tracing::error!(
                handler,
                "Consumer handler panicked; message acknowledged and dropped, consumer kept alive"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod guard_tests {
    use super::guard_handler;

    #[tokio::test]
    async fn passes_ok_and_err_through_unchanged() {
        assert!(guard_handler("t", async { Ok(()) }).await.is_ok());
        assert!(
            guard_handler("t", async { Err(mq::BroccoliError::Job("boom".into())) })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn converts_a_handler_panic_into_a_clean_ack() {
        let result = guard_handler("t", async {
            panic!("handler blew up");
            #[allow(unreachable_code)]
            Ok::<(), mq::BroccoliError>(())
        })
        .await;
        assert!(
            result.is_ok(),
            "a panicking handler must be caught and acknowledged, not propagated"
        );
    }
}
