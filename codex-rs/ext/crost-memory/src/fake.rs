//! Deterministic in-memory provider used by tests.
//!
//! It is public so host integration tests can drive recall, retention, and
//! promotion without any network access.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::ProviderKind;
use crate::provider::MemoryError;
use crate::provider::MemoryProvider;
use crate::types::PromoteOp;
use crate::types::ProviderStatus;
use crate::types::RecallItem;
use crate::types::RecallScope;
use crate::types::RetainOp;

/// Per-scope knobs and seeded data.
#[derive(Clone, Debug, Default)]
pub struct FakeScopeState {
    /// Items returned by `recall` for this scope, in seeded order.
    pub items: Vec<RecallItem>,
    /// When set, `recall` fails with this error.
    pub failure: Option<MemoryError>,
    /// Artificial latency applied before `recall` resolves.
    pub delay: Option<Duration>,
}

/// Observable state of the fake provider.
#[derive(Clone, Debug, Default)]
pub struct FakeState {
    pub private: FakeScopeState,
    pub shared: FakeScopeState,
    /// Every retention the provider accepted, in order.
    pub retained: Vec<RetainOp>,
    /// Every promotion the provider accepted, in order.
    pub promoted: Vec<PromoteOp>,
    /// When set, writes fail with this error.
    pub write_failure: Option<MemoryError>,
    /// When true, every call fails with `MemoryError::Auth`.
    pub auth_failure: bool,
    /// Whether `status()` reports healthy.
    pub healthy: bool,
}

/// Deterministic provider backed by shared in-memory state.
#[derive(Clone, Debug, Default)]
pub struct FakeProvider {
    state: Arc<Mutex<FakeState>>,
}

impl FakeProvider {
    /// Creates a provider with empty state that reports healthy.
    pub fn new() -> Self {
        let provider = Self::default();
        provider.with_state(|state| state.healthy = true);
        provider
    }

    /// Returns the shared state handle so tests can assert on it.
    pub fn state(&self) -> Arc<Mutex<FakeState>> {
        Arc::clone(&self.state)
    }

    /// Mutates the shared state under the lock.
    pub fn with_state<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut state)
    }

    /// Reads a projection of the shared state under the lock.
    pub fn read_state<R>(&self, f: impl FnOnce(&FakeState) -> R) -> R {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&state)
    }

    /// Seeds the items one scope returns.
    pub fn seed(&self, scope: RecallScope, items: Vec<RecallItem>) -> &Self {
        self.with_state(|state| scope_mut(state, scope).items = items);
        self
    }

    /// Makes recalls for one scope fail.
    pub fn fail_recall(&self, scope: RecallScope, error: MemoryError) -> &Self {
        self.with_state(|state| scope_mut(state, scope).failure = Some(error));
        self
    }

    /// Makes retentions and promotions fail.
    pub fn fail_retain(&self, error: MemoryError) -> &Self {
        self.with_state(|state| state.write_failure = Some(error));
        self
    }

    /// Clears the write failure knob.
    pub fn clear_write_failure(&self) -> &Self {
        self.with_state(|state| state.write_failure = None);
        self
    }

    /// Delays recalls for one scope.
    pub fn delay(&self, scope: RecallScope, delay: Duration) -> &Self {
        self.with_state(|state| scope_mut(state, scope).delay = Some(delay));
        self
    }

    /// Makes every call fail with an auth error.
    pub fn set_auth_failure(&self, auth_failure: bool) -> &Self {
        self.with_state(|state| state.auth_failure = auth_failure);
        self
    }

    /// Sets what `status()` reports.
    pub fn set_healthy(&self, healthy: bool) -> &Self {
        self.with_state(|state| state.healthy = healthy);
        self
    }

    /// Snapshot of accepted retentions.
    pub fn retained(&self) -> Vec<RetainOp> {
        self.read_state(|state| state.retained.clone())
    }

    /// Snapshot of accepted promotions.
    pub fn promoted(&self) -> Vec<PromoteOp> {
        self.read_state(|state| state.promoted.clone())
    }

    fn write_error(&self) -> Option<MemoryError> {
        self.read_state(|state| {
            if state.auth_failure {
                Some(MemoryError::Auth("fake auth failure".to_string()))
            } else {
                state.write_failure.clone()
            }
        })
    }
}

fn scope_mut(state: &mut FakeState, scope: RecallScope) -> &mut FakeScopeState {
    match scope {
        RecallScope::Private => &mut state.private,
        RecallScope::Shared => &mut state.shared,
    }
}

#[async_trait]
impl MemoryProvider for FakeProvider {
    async fn recall(
        &self,
        scope: RecallScope,
        _query: &str,
        _max_tokens: usize,
        max_items: usize,
    ) -> Result<Vec<RecallItem>, MemoryError> {
        let (delay, failure, auth_failure, mut items) = self.read_state(|state| {
            let scope_state = match scope {
                RecallScope::Private => &state.private,
                RecallScope::Shared => &state.shared,
            };
            (
                scope_state.delay,
                scope_state.failure.clone(),
                state.auth_failure,
                scope_state.items.clone(),
            )
        });

        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if auth_failure {
            return Err(MemoryError::Auth("fake auth failure".to_string()));
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        items.truncate(max_items);
        Ok(items)
    }

    async fn retain_private(&self, op: &RetainOp) -> Result<(), MemoryError> {
        if let Some(error) = self.write_error() {
            return Err(error);
        }
        self.with_state(|state| {
            if let Some(existing) = state
                .retained
                .iter_mut()
                .find(|existing| existing.op_id == op.op_id)
            {
                // Idempotent upsert: a retried op never creates a duplicate.
                *existing = op.clone();
            } else {
                state.retained.push(op.clone());
            }
        });
        Ok(())
    }

    async fn promote_shared(&self, op: &PromoteOp) -> Result<String, MemoryError> {
        if let Some(error) = self.write_error() {
            return Err(error);
        }
        self.with_state(|state| {
            if let Some(existing) = state
                .promoted
                .iter_mut()
                .find(|existing| existing.op_id == op.op_id)
            {
                *existing = op.clone();
            } else {
                state.promoted.push(op.clone());
            }
        });
        Ok(op.op_id.clone())
    }

    async fn status(&self) -> ProviderStatus {
        let (healthy, auth_failure) = self.read_state(|state| (state.healthy, state.auth_failure));
        ProviderStatus {
            healthy: healthy && !auth_failure,
            provider: ProviderKind::Fake.as_str(),
            endpoint: Some("fake://in-memory".to_string()),
            latency_ms: Some(0),
            detail: auth_failure.then(|| "fake auth failure".to_string()),
        }
    }
}

/// Builds a recall item with only the fields a test cares about.
pub fn test_item(id: &str, content: &str, score: f64) -> RecallItem {
    RecallItem {
        id: id.to_string(),
        content: content.to_string(),
        score,
        ..RecallItem::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TurnRecord;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn seeded_items_are_returned_and_capped() {
        let provider = FakeProvider::new();
        provider.seed(
            RecallScope::Shared,
            vec![
                test_item("a", "alpha", 0.9),
                test_item("b", "beta", 0.5),
                test_item("c", "gamma", 0.1),
            ],
        );

        let items = provider
            .recall(RecallScope::Shared, "q", 1000, 2)
            .await
            .unwrap_or_default();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "a");
        assert!(
            provider
                .recall(RecallScope::Private, "q", 1000, 8)
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn scope_failures_are_isolated() {
        let provider = FakeProvider::new();
        provider.seed(RecallScope::Private, vec![test_item("p", "private", 0.3)]);
        provider.fail_recall(
            RecallScope::Shared,
            MemoryError::Unavailable("boom".to_string()),
        );

        assert!(
            provider
                .recall(RecallScope::Shared, "q", 1000, 8)
                .await
                .is_err()
        );
        assert_eq!(
            provider
                .recall(RecallScope::Private, "q", 1000, 8)
                .await
                .unwrap_or_default()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn retaining_the_same_op_id_twice_does_not_duplicate() {
        let provider = FakeProvider::new();
        let op = RetainOp {
            op_id: "cm-1".to_string(),
            record: TurnRecord {
                objective: Some("build".to_string()),
                ..TurnRecord::default()
            },
        };

        provider.retain_private(&op).await.unwrap_or_default();
        provider.retain_private(&op).await.unwrap_or_default();

        assert_eq!(provider.retained().len(), 1);
    }

    #[tokio::test]
    async fn auth_failure_mode_affects_every_call() {
        let provider = FakeProvider::new();
        provider.set_auth_failure(true);

        assert_eq!(
            provider.recall(RecallScope::Shared, "q", 10, 1).await,
            Err(MemoryError::Auth("fake auth failure".to_string()))
        );
        assert!(!provider.status().await.healthy);
    }
}
