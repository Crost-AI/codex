//! Detached outbox delivery. Turn paths never block on the network.

use std::sync::Arc;

use crate::state::CrostMemoryRuntime;

/// Flushes the outbox on a detached task when a runtime is available.
///
/// Returns `false` when no tokio runtime is available; callers that must
/// guarantee delivery (tests) should await [`flush_now`] instead.
pub fn spawn_flush(runtime: Arc<CrostMemoryRuntime>) -> bool {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::debug!("crost memory flush skipped: no tokio runtime on this thread");
        return false;
    };
    handle.spawn(async move {
        flush_now(&runtime).await;
    });
    true
}

/// Flushes the outbox inline and records the result for diagnostics.
pub async fn flush_now(runtime: &Arc<CrostMemoryRuntime>) {
    let outcome = runtime.outbox.flush(runtime.provider.as_ref()).await;
    runtime.record_auth_failed(outcome.auth_failed);
    if outcome.auth_failed {
        tracing::warn!(
            "crost memory could not deliver queued records: the configured API key was rejected"
        );
    }
    tracing::debug!(
        sent = outcome.sent,
        retried = outcome.retried,
        dropped = outcome.dropped,
        remaining = outcome.remaining,
        "crost memory outbox flushed"
    );
}
