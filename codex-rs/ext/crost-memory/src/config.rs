//! Crate-owned configuration for the Crost memory client.
//!
//! Credentials are only ever referenced by environment-variable NAME. The token
//! value is read at client construction and is wrapped so it can never leak
//! through `Debug`.

use serde::Deserialize;
use serde::Serialize;

use crate::identity::DEFAULT_PROJECT_FILE;

/// Environment variable that overrides the configured agent id.
pub const AGENT_ID_ENV: &str = "CROST_AGENT_ID";

/// Default agent id for this fork.
pub const DEFAULT_AGENT_ID: &str = "codex";

/// Default environment variable holding the Hindsight API key.
pub const DEFAULT_API_KEY_ENV: &str = "HINDSIGHT_API_KEY";

/// Backing implementation used to talk to memory storage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Production driver for a Hindsight deployment.
    #[default]
    Hindsight,
    /// Deterministic in-memory driver used by tests.
    Fake,
}

impl ProviderKind {
    /// Stable lowercase label used in diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hindsight => "hindsight",
            Self::Fake => "fake",
        }
    }
}

/// Effective Crost memory configuration for one thread.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CrostMemoryConfig {
    pub enabled: bool,
    pub provider: ProviderKind,
    pub base_url: Option<String>,
    pub agent_id: String,
    pub api_key_env: String,
    pub project_file: String,
    pub recall_timeout_ms: u64,
    pub recall_max_items: usize,
    pub private_token_budget: usize,
    pub shared_token_budget: usize,
    pub retain_enabled: bool,
    pub shared_promotion_enabled: bool,
    pub fail_open: bool,
}

impl Default for CrostMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: ProviderKind::Hindsight,
            base_url: None,
            agent_id: DEFAULT_AGENT_ID.to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            project_file: DEFAULT_PROJECT_FILE.to_string(),
            recall_timeout_ms: 1500,
            recall_max_items: 8,
            private_token_budget: 1200,
            shared_token_budget: 800,
            retain_enabled: true,
            shared_promotion_enabled: true,
            fail_open: true,
        }
    }
}

impl CrostMemoryConfig {
    /// Applies `CROST_AGENT_ID` when it is set to a non-empty value.
    #[must_use]
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(agent_id) = std::env::var(AGENT_ID_ENV) {
            let agent_id = agent_id.trim();
            if !agent_id.is_empty() {
                self.agent_id = agent_id.to_string();
            }
        }
        self
    }

    /// Per-scope token budget.
    pub fn token_budget(&self, scope: crate::types::RecallScope) -> usize {
        match scope {
            crate::types::RecallScope::Private => self.private_token_budget,
            crate::types::RecallScope::Shared => self.shared_token_budget,
        }
    }

    /// Hard timeout applied to each recall scope.
    pub fn recall_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.recall_timeout_ms)
    }

    /// Reads the API token from the environment variable named by `api_key_env`.
    ///
    /// A missing variable is not an error: Hindsight may run authless.
    pub fn read_api_token(&self) -> ApiToken {
        ApiToken(
            std::env::var(&self.api_key_env)
                .ok()
                .filter(|token| !token.trim().is_empty()),
        )
    }

    /// Whether the configured API key environment variable is set.
    pub fn api_key_env_is_set(&self) -> bool {
        self.read_api_token().is_present()
    }
}

/// API token that never reveals its value through `Debug`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ApiToken(Option<String>);

impl ApiToken {
    /// Wraps an already-read token value.
    pub fn new(value: Option<String>) -> Self {
        Self(value.filter(|token| !token.trim().is_empty()))
    }

    /// Whether a token is available.
    pub fn is_present(&self) -> bool {
        self.0.is_some()
    }

    /// Borrows the raw token for outbound request construction only.
    pub fn expose(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ApiToken(***)"
        } else {
            "ApiToken(unset)"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RecallScope;
    use pretty_assertions::assert_eq;

    #[test]
    fn defaults_match_the_contract() {
        let config = CrostMemoryConfig::default();

        assert!(!config.enabled);
        assert_eq!(config.provider, ProviderKind::Hindsight);
        assert_eq!(config.base_url, None);
        assert_eq!(config.agent_id, "codex");
        assert_eq!(config.api_key_env, "HINDSIGHT_API_KEY");
        assert_eq!(config.project_file, ".crost/project.yaml");
        assert_eq!(config.recall_timeout_ms, 1500);
        assert_eq!(config.recall_max_items, 8);
        assert_eq!(config.token_budget(RecallScope::Private), 1200);
        assert_eq!(config.token_budget(RecallScope::Shared), 800);
        assert!(config.retain_enabled);
        assert!(config.shared_promotion_enabled);
        assert!(config.fail_open);
    }

    #[test]
    fn api_token_debug_never_reveals_the_value() {
        let token = ApiToken::new(Some("hs-super-secret-value".to_string()));

        assert_eq!(format!("{token:?}"), "ApiToken(***)");
        assert!(!format!("{token:?}").contains("super-secret"));
        assert_eq!(token.expose(), Some("hs-super-secret-value"));
        assert_eq!(format!("{:?}", ApiToken::new(None)), "ApiToken(unset)");
        assert!(!ApiToken::new(Some("   ".to_string())).is_present());
    }
}
