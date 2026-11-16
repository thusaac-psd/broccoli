pub mod operation_dlq;
pub mod operation_result;

pub use operation_dlq::consume_operation_dlq;
pub use operation_result::consume_operation_results;

use futures::FutureExt;

/// Run one message's handler under a panic guard, so a handler that panics
/// on one message does not end the consumer loop for the life of the process.
/// A panic is logged and the message is left as the handler left it.
pub(crate) async fn guard_handler<F>(handler: &'static str, fut: F)
where
    F: std::future::Future<Output = ()>,
{
    if std::panic::AssertUnwindSafe(fut)
        .catch_unwind()
        .await
        .is_err()
    {
        tracing::error!(
            handler,
            "Consumer handler panicked; the consumer keeps running"
        );
    }
}

#[cfg(test)]
mod guard_tests {
    use super::guard_handler;

    #[tokio::test]
    async fn a_panicking_handler_does_not_propagate() {
        guard_handler("t", async { panic!("handler blew up") }).await;
    }
}
