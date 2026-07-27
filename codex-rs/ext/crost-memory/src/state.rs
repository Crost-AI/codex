//! Typed extension state seeded into the host thread and turn stores.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::capture::CrostTurnCapture;
use crate::config::CrostMemoryConfig;
use crate::identity::ProjectIdentity;
use crate::outbox::Outbox;
use crate::provider::MemoryProvider;
use crate::recall::RecallOutcome;

/// Host-resolved configuration for one thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrostMemoryExtensionConfig {
    /// Provider-agnostic memory settings.
    pub memory: CrostMemoryConfig,
    /// Directory used to resolve project identity. Identity itself comes from
    /// committed content found by walking this directory's ancestors.
    pub cwd: PathBuf,
    /// Root under which per-project outboxes are created.
    pub outbox_root: PathBuf,
}

impl Default for CrostMemoryExtensionConfig {
    fn default() -> Self {
        Self {
            memory: CrostMemoryConfig::default(),
            cwd: PathBuf::from("."),
            outbox_root: PathBuf::from("."),
        }
    }
}

/// Why memory is inactive for a thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledReason {
    /// Configuration has `enabled = false`.
    ConfiguredOff,
    /// No usable `.crost/project.yaml` was found.
    NoProjectIdentity(String),
    /// The provider could not be constructed.
    ProviderUnavailable(String),
}

impl std::fmt::Display for DisabledReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfiguredOff => f.write_str("crost memory is disabled by configuration"),
            Self::NoProjectIdentity(detail) => {
                write!(f, "no crost project identity: {detail}")
            }
            Self::ProviderUnavailable(detail) => {
                write!(f, "crost memory provider unavailable: {detail}")
            }
        }
    }
}

/// Live per-thread runtime shared by every contributor role.
pub struct CrostMemoryRuntime {
    pub config: CrostMemoryConfig,
    pub identity: ProjectIdentity,
    pub provider: Arc<dyn MemoryProvider>,
    pub outbox: Arc<Outbox>,
    last: Mutex<LastActivity>,
}

impl std::fmt::Debug for CrostMemoryRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrostMemoryRuntime")
            .field("config", &self.config)
            .field("identity", &self.identity)
            .field("outbox_dir", &self.outbox.dir())
            .finish()
    }
}

/// Diagnostics-only summary of the most recent memory activity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LastActivity {
    /// Stats from the most recent recall. Never memory bodies.
    pub recall: Option<LastRecallStats>,
    /// Human-readable outcome of the most recent retention attempt.
    pub retention: Option<String>,
    /// Whether the last flush saw credentials rejected.
    pub auth_failed: bool,
}

/// Body-free summary of one recall.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LastRecallStats {
    pub private_n: usize,
    pub shared_n: usize,
    pub injected_tokens: usize,
    pub latency_ms: u64,
    pub degraded: bool,
}

impl From<&RecallOutcome> for LastRecallStats {
    fn from(outcome: &RecallOutcome) -> Self {
        Self {
            private_n: outcome.private_n,
            shared_n: outcome.shared_n,
            injected_tokens: outcome.injected_tokens,
            latency_ms: outcome.latency_ms,
            degraded: outcome.degraded,
        }
    }
}

impl CrostMemoryRuntime {
    /// Builds a runtime from already-resolved parts.
    pub fn new(
        config: CrostMemoryConfig,
        identity: ProjectIdentity,
        provider: Arc<dyn MemoryProvider>,
        outbox: Arc<Outbox>,
    ) -> Self {
        Self {
            config,
            identity,
            provider,
            outbox,
            last: Mutex::new(LastActivity::default()),
        }
    }

    /// Records recall stats for diagnostics.
    pub fn record_recall(&self, outcome: &RecallOutcome) {
        self.with_last(|last| last.recall = Some(LastRecallStats::from(outcome)));
    }

    /// Records the outcome of a retention attempt for diagnostics.
    pub fn record_retention(&self, detail: impl Into<String>) {
        self.with_last(|last| last.retention = Some(detail.into()));
    }

    /// Records whether the last flush hit an auth failure.
    pub fn record_auth_failed(&self, auth_failed: bool) {
        self.with_last(|last| last.auth_failed = auth_failed);
    }

    /// Snapshot of the most recent activity.
    pub fn last_activity(&self) -> LastActivity {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn with_last(&self, f: impl FnOnce(&mut LastActivity)) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut last);
    }
}

/// Thread-scoped state seeded by the thread-lifecycle contributor.
#[derive(Clone, Debug)]
pub enum CrostMemoryThreadState {
    /// Memory is off for this thread; every later hook returns immediately.
    Disabled(DisabledReason),
    /// Configuration is on but identity could not be resolved from the config
    /// cwd yet. Resolution is retried once from the turn's primary environment.
    PendingIdentity(Box<CrostMemoryExtensionConfig>),
    /// Memory is live.
    Enabled(Arc<CrostMemoryRuntime>),
}

impl CrostMemoryThreadState {
    /// Live runtime, when memory is enabled.
    pub fn runtime(&self) -> Option<Arc<CrostMemoryRuntime>> {
        match self {
            Self::Enabled(runtime) => Some(Arc::clone(runtime)),
            _ => None,
        }
    }

    /// Reason memory is off, when it is.
    pub fn disabled_reason(&self) -> Option<&DisabledReason> {
        match self {
            Self::Disabled(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Turn-scoped observation state.
#[derive(Debug, Default)]
pub struct CrostMemoryTurnState {
    capture: Mutex<CrostTurnCapture>,
    discarded: AtomicBool,
}

impl CrostMemoryTurnState {
    /// Mutates the capture under the lock.
    pub fn with_capture<R>(&self, f: impl FnOnce(&mut CrostTurnCapture) -> R) -> R {
        let mut capture = self.capture.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut capture)
    }

    /// Snapshot of the capture.
    pub fn snapshot(&self) -> CrostTurnCapture {
        self.capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Marks the turn as aborted or errored so nothing is retained.
    pub fn discard(&self) {
        self.discarded.store(true, Ordering::Release);
        self.with_capture(|capture| *capture = CrostTurnCapture::default());
    }

    /// Whether this turn's capture was discarded.
    pub fn is_discarded(&self) -> bool {
        self.discarded.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn discarding_a_turn_clears_the_capture() {
        let state = CrostMemoryTurnState::default();
        state.with_capture(|capture| capture.objective = Some("do the thing".to_string()));

        state.discard();

        assert!(state.is_discarded());
        assert_eq!(state.snapshot(), CrostTurnCapture::default());
    }

    #[test]
    fn disabled_reasons_render_precisely() {
        assert_eq!(
            DisabledReason::NoProjectIdentity("no file".to_string()).to_string(),
            "no crost project identity: no file"
        );
        assert_eq!(
            DisabledReason::ConfiguredOff.to_string(),
            "crost memory is disabled by configuration"
        );
    }
}
