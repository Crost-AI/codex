//! The only abstraction that knows how memories are stored.
//!
//! Everything above this layer (lifecycle, injection, retention, promotion,
//! diagnostics) is provider-agnostic.

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::CrostMemoryConfig;
use crate::config::ProviderKind;
use crate::fake::FakeProvider;
use crate::hindsight::HindsightProvider;
use crate::identity::ProjectIdentity;
use crate::types::PromoteOp;
use crate::types::ProviderStatus;
use crate::types::RecallItem;
use crate::types::RecallScope;
use crate::types::RetainOp;

/// Failure classes that callers must treat differently.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum MemoryError {
    /// Network error, timeout, or 5xx. Safe to retry with the same op id.
    #[error("memory backend unavailable: {0}")]
    Unavailable(String),
    /// 401/403. Surface one visible warning; retrying cannot help.
    #[error("memory backend rejected credentials: {0}")]
    Auth(String),
    /// Other 4xx or a malformed request. Drop the operation and log.
    #[error("memory request was invalid: {0}")]
    Invalid(String),
}

impl MemoryError {
    /// Whether an operation that failed this way should be retried later.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }

    /// Stable label used in diagnostics and tracing fields.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "unavailable",
            Self::Auth(_) => "auth",
            Self::Invalid(_) => "invalid",
        }
    }
}

/// Storage driver for project memory.
///
/// All futures must be cancel-safe: dropping one leaves no partial state
/// behind, because every write is idempotent under its stable op id.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Reads the highest-scoring memories for `query` within one scope.
    async fn recall(
        &self,
        scope: RecallScope,
        query: &str,
        max_tokens: usize,
        max_items: usize,
    ) -> Result<Vec<RecallItem>, MemoryError>;

    /// Writes one turn summary to the calling agent's private bank.
    async fn retain_private(&self, op: &RetainOp) -> Result<(), MemoryError>;

    /// Writes one explicitly promoted record to the shared bank.
    async fn promote_shared(&self, op: &PromoteOp) -> Result<String, MemoryError>;

    /// Reports endpoint reachability and latency.
    async fn status(&self) -> ProviderStatus;
}

/// Builds the provider selected by configuration.
pub fn build_provider(
    config: &CrostMemoryConfig,
    identity: &ProjectIdentity,
) -> Result<Arc<dyn MemoryProvider>, MemoryError> {
    match config.provider {
        ProviderKind::Hindsight => Ok(Arc::new(HindsightProvider::new(config, identity)?)),
        ProviderKind::Fake => Ok(Arc::new(FakeProvider::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn identity() -> ProjectIdentity {
        ProjectIdentity {
            project_id: "p1".to_string(),
            slug: "ohm".to_string(),
            bank_prefix: None,
        }
    }

    #[test]
    fn error_classes_drive_retry_policy() {
        assert!(MemoryError::Unavailable("timeout".to_string()).is_retryable());
        assert!(!MemoryError::Auth("401".to_string()).is_retryable());
        assert!(!MemoryError::Invalid("400".to_string()).is_retryable());
        assert_eq!(MemoryError::Auth("401".to_string()).kind(), "auth");
    }

    #[tokio::test]
    async fn build_provider_selects_the_fake_kind_without_touching_the_network() {
        let config = CrostMemoryConfig {
            provider: ProviderKind::Fake,
            ..CrostMemoryConfig::default()
        };

        let fake = build_provider(&config, &identity()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(fake.status().await.provider, ProviderKind::Fake.as_str());
    }

    #[test]
    fn build_provider_constructs_the_hindsight_driver_when_configured() {
        let config = CrostMemoryConfig {
            provider: ProviderKind::Hindsight,
            base_url: Some("https://hindsight.example".to_string()),
            ..CrostMemoryConfig::default()
        };

        assert!(build_provider(&config, &identity()).is_ok());
    }

    #[test]
    fn hindsight_requires_a_base_url() {
        let config = CrostMemoryConfig {
            provider: ProviderKind::Hindsight,
            base_url: None,
            ..CrostMemoryConfig::default()
        };

        let err = build_provider(&config, &identity()).err();

        assert!(matches!(err, Some(MemoryError::Invalid(_))));
    }
}
