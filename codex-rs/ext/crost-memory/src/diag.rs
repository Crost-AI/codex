//! Diagnostics snapshot for a status/doctor surface.
//!
//! The snapshot NEVER contains memory bodies and never contains the API token.

use crate::state::CrostMemoryRuntime;
use crate::state::CrostMemoryThreadState;
use crate::state::LastRecallStats;

/// Body-free snapshot of the Crost memory subsystem.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryDiag {
    pub enabled: bool,
    /// Exact reason memory is inactive, when it is.
    pub disabled_reason: Option<String>,
    pub project_id: Option<String>,
    pub slug: Option<String>,
    pub bank_prefix: Option<String>,
    pub agent_id: String,
    pub provider: String,
    pub endpoint: Option<String>,
    pub healthy: Option<bool>,
    pub latency_ms: Option<u64>,
    pub detail: Option<String>,
    pub api_key_env: String,
    pub api_key_env_set: bool,
    pub outbox_depth: usize,
    pub outbox_oldest_age_secs: Option<u64>,
    pub last_recall: Option<LastRecallStats>,
    pub last_retention: Option<String>,
    pub last_flush_auth_failed: bool,
}

/// Builds a diagnostics snapshot, including a live provider status call.
pub async fn doctor(state: &CrostMemoryThreadState) -> MemoryDiag {
    match state {
        CrostMemoryThreadState::Enabled(runtime) => doctor_runtime(runtime).await,
        CrostMemoryThreadState::PendingIdentity(config) => MemoryDiag {
            enabled: false,
            disabled_reason: Some(
                "crost project identity not resolved yet for this thread".to_string(),
            ),
            agent_id: config.memory.agent_id.clone(),
            provider: config.memory.provider.as_str().to_string(),
            endpoint: config.memory.base_url.clone(),
            api_key_env: config.memory.api_key_env.clone(),
            api_key_env_set: config.memory.api_key_env_is_set(),
            ..MemoryDiag::default()
        },
        CrostMemoryThreadState::Disabled(reason) => MemoryDiag {
            enabled: false,
            disabled_reason: Some(reason.to_string()),
            ..MemoryDiag::default()
        },
    }
}

async fn doctor_runtime(runtime: &CrostMemoryRuntime) -> MemoryDiag {
    let status = runtime.provider.status().await;
    let last = runtime.last_activity();
    MemoryDiag {
        enabled: true,
        disabled_reason: None,
        project_id: Some(runtime.identity.project_id.clone()),
        slug: Some(runtime.identity.slug.clone()),
        bank_prefix: Some(runtime.identity.bank_prefix()),
        agent_id: runtime.config.agent_id.clone(),
        provider: status.provider.to_string(),
        endpoint: status.endpoint,
        healthy: Some(status.healthy),
        latency_ms: status.latency_ms,
        detail: status.detail,
        api_key_env: runtime.config.api_key_env.clone(),
        api_key_env_set: runtime.config.api_key_env_is_set(),
        outbox_depth: runtime.outbox.depth(),
        outbox_oldest_age_secs: runtime.outbox.oldest_age().map(|age| age.as_secs()),
        last_recall: last.recall,
        last_retention: last.retention,
        last_flush_auth_failed: last.auth_failed,
    }
}

/// Renders a plain-text report for a fork status command.
pub fn render_diag(diag: &MemoryDiag) -> String {
    let mut lines = vec!["crost memory".to_string()];
    if !diag.enabled {
        let reason = diag.disabled_reason.as_deref().unwrap_or("disabled");
        lines.push(format!("  status: disabled ({reason})"));
        if !diag.agent_id.is_empty() {
            let agent_id = &diag.agent_id;
            lines.push(format!("  agent: {agent_id}"));
        }
        return lines.join("\n");
    }

    let project_id = diag.project_id.as_deref().unwrap_or("unknown");
    let slug = diag.slug.as_deref().unwrap_or("unknown");
    let agent_id = &diag.agent_id;
    let provider = &diag.provider;
    lines.push("  status: enabled".to_string());
    lines.push(format!("  project: {slug} ({project_id})"));
    if let Some(bank_prefix) = diag.bank_prefix.as_deref() {
        lines.push(format!(
            "  banks: {bank_prefix}--shared, {bank_prefix}--{agent_id}-private"
        ));
    }
    lines.push(format!("  agent: {agent_id}"));
    lines.push(format!("  provider: {provider}"));
    let endpoint = diag.endpoint.as_deref().unwrap_or("unset");
    let health = match diag.healthy {
        Some(true) => "reachable",
        Some(false) => "unreachable",
        None => "unknown",
    };
    let latency = diag
        .latency_ms
        .map(|ms| format!(" ({ms} ms)"))
        .unwrap_or_default();
    lines.push(format!("  endpoint: {endpoint} — {health}{latency}"));
    if let Some(detail) = diag.detail.as_deref() {
        lines.push(format!("  endpoint detail: {detail}"));
    }
    let api_key_env = &diag.api_key_env;
    let api_key_set = if diag.api_key_env_set { "set" } else { "unset" };
    lines.push(format!("  auth: ${api_key_env} {api_key_set}"));

    let depth = diag.outbox_depth;
    let oldest = diag
        .outbox_oldest_age_secs
        .map(|secs| format!(", oldest {secs}s"))
        .unwrap_or_default();
    lines.push(format!("  outbox: {depth} queued{oldest}"));
    if diag.last_flush_auth_failed {
        lines.push("  outbox: paused — credentials were rejected".to_string());
    }

    match diag.last_recall {
        Some(recall) => {
            let LastRecallStats {
                private_n,
                shared_n,
                injected_tokens,
                latency_ms,
                degraded,
            } = recall;
            let degraded = if degraded { " (degraded)" } else { "" };
            lines.push(format!(
                "  last recall: {shared_n} shared + {private_n} private, ~{injected_tokens} tokens, {latency_ms} ms{degraded}"
            ));
        }
        None => lines.push("  last recall: none".to_string()),
    }
    let retention = diag.last_retention.as_deref().unwrap_or("none");
    lines.push(format!("  last retention: {retention}"));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrostMemoryConfig;
    use crate::config::ProviderKind;
    use crate::fake::FakeProvider;
    use crate::identity::ProjectIdentity;
    use crate::outbox::Outbox;
    use crate::recall::RecallOutcome;
    use crate::state::DisabledReason;
    use std::sync::Arc;

    fn runtime(root: &std::path::Path) -> Arc<CrostMemoryRuntime> {
        let identity = ProjectIdentity {
            project_id: "p1".to_string(),
            slug: "ohm".to_string(),
            bank_prefix: None,
        };
        Arc::new(CrostMemoryRuntime::new(
            CrostMemoryConfig {
                enabled: true,
                provider: ProviderKind::Fake,
                agent_id: "codex".to_string(),
                ..CrostMemoryConfig::default()
            },
            identity.clone(),
            Arc::new(FakeProvider::new()),
            Arc::new(Outbox::new(root, &identity.project_id)),
        ))
    }

    #[tokio::test]
    async fn enabled_report_covers_identity_health_and_queue() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let runtime = runtime(tmp.path());
        runtime.record_recall(&RecallOutcome {
            block: Some("<crost-memory>secret lore</crost-memory>".to_string()),
            private_n: 2,
            shared_n: 1,
            injected_tokens: 120,
            latency_ms: 34,
            degraded: false,
        });
        runtime.record_retention("queued 1 op");

        let diag = doctor(&CrostMemoryThreadState::Enabled(runtime)).await;
        let rendered = render_diag(&diag);

        assert!(diag.enabled);
        assert_eq!(diag.provider, "fake");
        assert!(rendered.contains("project: ohm (p1)"));
        assert!(rendered.contains("banks: crost--ohm--shared, crost--ohm--codex-private"));
        assert!(rendered.contains("last recall: 1 shared + 2 private, ~120 tokens, 34 ms"));
        assert!(rendered.contains("last retention: queued 1 op"));
        assert!(rendered.contains("$HINDSIGHT_API_KEY"));
        // Never leak memory bodies.
        assert!(!rendered.contains("secret lore"));
    }

    #[tokio::test]
    async fn disabled_report_states_the_exact_reason() {
        let diag = doctor(&CrostMemoryThreadState::Disabled(
            DisabledReason::NoProjectIdentity("no .crost/project.yaml".to_string()),
        ))
        .await;

        let rendered = render_diag(&diag);

        assert!(!diag.enabled);
        assert!(rendered.contains("disabled (no crost project identity: no .crost/project.yaml)"));
    }
}
