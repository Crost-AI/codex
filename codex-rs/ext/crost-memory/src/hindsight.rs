//! Production driver for a Hindsight deployment.
//!
//! This is the ONLY module that knows Hindsight's API shape or bank naming.

use std::collections::BTreeMap;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::config::ApiToken;
use crate::config::CrostMemoryConfig;
use crate::config::ProviderKind;
use crate::identity::ProjectIdentity;
use crate::provider::MemoryError;
use crate::provider::MemoryProvider;
use crate::types::PromoteOp;
use crate::types::ProviderStatus;
use crate::types::RecallItem;
use crate::types::RecallScope;
use crate::types::RetainOp;

/// Longest recall query forwarded to the server.
const MAX_QUERY_CHARS: usize = 2_000;

/// Record kind stored on automatic private retentions.
const TURN_SUMMARY_RECORD_KIND: &str = "turn_summary";

/// Direct HTTP driver for Hindsight.
pub struct HindsightProvider {
    client: reqwest::Client,
    base_url: String,
    agent_id: String,
    bank_prefix: String,
    token: ApiToken,
}

impl std::fmt::Debug for HindsightProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HindsightProvider")
            .field("base_url", &self.base_url)
            .field("agent_id", &self.agent_id)
            .field("bank_prefix", &self.bank_prefix)
            .field("token", &self.token)
            .finish()
    }
}

impl HindsightProvider {
    /// Builds a driver bound to one project identity.
    pub fn new(
        config: &CrostMemoryConfig,
        identity: &ProjectIdentity,
    ) -> Result<Self, MemoryError> {
        let base_url = config
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                MemoryError::Invalid("crost memory requires `base_url` to be set".to_string())
            })?
            .trim_end_matches('/')
            .to_string();
        let timeout = config.recall_timeout().max(Duration::from_millis(250));
        let client = reqwest::Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(|err| MemoryError::Invalid(format!("could not build http client: {err}")))?;

        Ok(Self {
            client,
            base_url,
            agent_id: config.agent_id.clone(),
            bank_prefix: identity.bank_prefix(),
            token: config.read_api_token(),
        })
    }

    /// Bank name for one scope.
    ///
    /// Crost contract (`MEMORY-BANKS.md`): `{prefix}--shared` plus one
    /// private bank per CLI (`{prefix}--codex-private`, `--grok-private`,
    /// `--claude-private`). `agent_id` is what selects the private bank.
    pub fn bank_name(&self, scope: RecallScope) -> String {
        let prefix = &self.bank_prefix;
        match scope {
            RecallScope::Shared => format!("{prefix}--shared"),
            RecallScope::Private => {
                let agent = &self.agent_id;
                format!("{prefix}--{agent}-private")
            }
        }
    }

    fn memories_url(&self, scope: RecallScope) -> String {
        let base = &self.base_url;
        let bank = self.bank_name(scope);
        format!("{base}/v1/default/banks/{bank}/memories")
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.token.expose() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    async fn send_write(
        &self,
        scope: RecallScope,
        body: Value,
        op_id: &str,
    ) -> Result<(), MemoryError> {
        let response = self
            .authorize(self.client.post(self.memories_url(scope)))
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;
        classify_response(response).await?;
        tracing::debug!(
            scope = scope.as_str(),
            op_id = op_id,
            "crost memory write accepted"
        );
        Ok(())
    }
}

#[async_trait]
impl MemoryProvider for HindsightProvider {
    async fn recall(
        &self,
        scope: RecallScope,
        query: &str,
        max_tokens: usize,
        max_items: usize,
    ) -> Result<Vec<RecallItem>, MemoryError> {
        let url = self.memories_url(scope);
        let body = json!({
            "query": truncate_chars(query, MAX_QUERY_CHARS),
            "max_tokens": max_tokens,
        });
        let response = self
            .authorize(self.client.post(format!("{url}/recall")))
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;
        let response = classify_response(response).await?;
        let payload: RecallResponse = response.json().await.map_err(|err| {
            MemoryError::Invalid(format!("could not decode recall response: {err}"))
        })?;
        let mut items = payload
            .results
            .into_iter()
            .map(WireRecallItem::into_item)
            .collect::<Vec<_>>();
        items.truncate(max_items);
        tracing::debug!(
            scope = scope.as_str(),
            item_count = items.len(),
            "crost memory recall returned items"
        );
        Ok(items)
    }

    async fn retain_private(&self, op: &RetainOp) -> Result<(), MemoryError> {
        let mut metadata = BTreeMap::new();
        metadata.insert("agent".to_string(), self.agent_id.clone());
        metadata.insert(
            "record_kind".to_string(),
            TURN_SUMMARY_RECORD_KIND.to_string(),
        );
        insert_metadata(&mut metadata, "task_id", op.record.task_id.as_deref());
        insert_metadata(&mut metadata, "branch", op.record.branch.as_deref());
        insert_metadata(&mut metadata, "commit", op.record.commit.as_deref());

        let agent = &self.agent_id;
        let body = json!({
            "items": [{
                "content": op.record.render(),
                "document_id": op.op_id,
                "metadata": metadata,
                "tags": [format!("agent:{agent}")],
                "update_mode": "replace",
            }],
            "async": true,
            "operation_id": op.op_id,
        });
        self.send_write(RecallScope::Private, body, &op.op_id).await
    }

    async fn promote_shared(&self, op: &PromoteOp) -> Result<String, MemoryError> {
        let mut metadata = BTreeMap::new();
        metadata.insert("agent".to_string(), self.agent_id.clone());
        metadata.insert("record_kind".to_string(), op.kind.as_str().to_string());
        insert_metadata(&mut metadata, "status", op.record.status.as_deref());
        insert_metadata(&mut metadata, "task_id", op.record.task_id.as_deref());
        insert_metadata(&mut metadata, "next_owner", op.record.next_owner.as_deref());
        insert_metadata(
            &mut metadata,
            "next_action",
            op.record.next_action.as_deref(),
        );
        insert_metadata(
            &mut metadata,
            "commit",
            op.record.evidence.commit.as_deref(),
        );
        insert_metadata(
            &mut metadata,
            "test_cmd",
            op.record.evidence.test_cmd.as_deref(),
        );
        insert_metadata(
            &mut metadata,
            "test_result",
            op.record.evidence.test_result.as_deref(),
        );
        insert_metadata(&mut metadata, "pr", op.record.evidence.pr.as_deref());
        insert_metadata(&mut metadata, "supersedes", op.supersedes.as_deref());

        let agent = &self.agent_id;
        let kind = op.kind.as_str();
        let body = json!({
            "items": [{
                "content": op.record.render(op.kind),
                "document_id": op.op_id,
                "metadata": metadata,
                "tags": [format!("kind:{kind}"), format!("agent:{agent}")],
                "update_mode": "replace",
            }],
            "async": true,
            "operation_id": op.op_id,
        });
        self.send_write(RecallScope::Shared, body, &op.op_id)
            .await?;
        Ok(op.op_id.clone())
    }

    async fn status(&self) -> ProviderStatus {
        let base = &self.base_url;
        let started = Instant::now();
        let result = self
            .authorize(self.client.get(format!("{base}/version")))
            .send()
            .await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match result {
            Ok(response) if response.status().is_success() => ProviderStatus {
                healthy: true,
                provider: ProviderKind::Hindsight.as_str(),
                endpoint: Some(self.base_url.clone()),
                latency_ms: Some(latency_ms),
                detail: None,
            },
            Ok(response) => ProviderStatus {
                healthy: false,
                provider: ProviderKind::Hindsight.as_str(),
                endpoint: Some(self.base_url.clone()),
                latency_ms: Some(latency_ms),
                detail: Some(format!("unexpected status {}", response.status().as_u16())),
            },
            Err(err) => ProviderStatus {
                healthy: false,
                provider: ProviderKind::Hindsight.as_str(),
                endpoint: Some(self.base_url.clone()),
                latency_ms: Some(latency_ms),
                detail: Some(transport_detail(&err)),
            },
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RecallResponse {
    #[serde(default)]
    results: Vec<WireRecallItem>,
}

#[derive(Debug, Default, Deserialize)]
struct WireRecallItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, Value>,
    #[serde(default)]
    mentioned_at: Option<String>,
    #[serde(default)]
    scores: WireScores,
}

#[derive(Debug, Default, Deserialize)]
struct WireScores {
    #[serde(default, rename = "final")]
    final_score: Option<f64>,
}

impl WireRecallItem {
    fn into_item(self) -> RecallItem {
        let source_agent = metadata_string(&self.metadata, "source_agent")
            .or_else(|| metadata_string(&self.metadata, "agent"));
        let task_id = metadata_string(&self.metadata, "task_id");
        RecallItem {
            id: self.id,
            content: self.text,
            kind: self.r#type,
            created_at: self.mentioned_at,
            source_agent,
            task_id,
            score: self.scores.final_score.unwrap_or_default(),
        }
    }
}

fn metadata_string(metadata: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn insert_metadata(metadata: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        metadata.insert(key.to_string(), value.to_string());
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        out.push(ch);
    }
    out
}

fn transport_detail(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "request timed out".to_string()
    } else if err.is_connect() {
        "could not connect".to_string()
    } else {
        "transport error".to_string()
    }
}

fn transport_error(err: reqwest::Error) -> MemoryError {
    MemoryError::Unavailable(transport_detail(&err))
}

async fn classify_response(response: reqwest::Response) -> Result<reqwest::Response, MemoryError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let code = status.as_u16();
    if matches!(code, 401 | 403) {
        return Err(MemoryError::Auth(format!("status {code}")));
    }
    if status.is_server_error() || code == 408 || code == 429 {
        return Err(MemoryError::Unavailable(format!("status {code}")));
    }
    Err(MemoryError::Invalid(format!("status {code}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn provider(bank_prefix: Option<&str>) -> HindsightProvider {
        let config = CrostMemoryConfig {
            base_url: Some("https://hindsight.example/".to_string()),
            agent_id: "codex".to_string(),
            ..CrostMemoryConfig::default()
        };
        let identity = ProjectIdentity {
            project_id: "p1".to_string(),
            slug: "ohm-storefront".to_string(),
            bank_prefix: bank_prefix.map(str::to_string),
        };
        HindsightProvider::new(&config, &identity).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn bank_names_derive_from_slug_and_agent() {
        let provider = provider(None);

        assert_eq!(
            provider.bank_name(RecallScope::Shared),
            "crost--ohm-storefront--shared"
        );
        assert_eq!(
            provider.bank_name(RecallScope::Private),
            "crost--ohm-storefront--codex-private"
        );
    }

    #[test]
    fn each_cli_reports_to_its_own_private_bank() {
        for (agent, expected) in [
            ("codex", "crost--ohm-storefront--codex-private"),
            ("grok", "crost--ohm-storefront--grok-private"),
            ("claude", "crost--ohm-storefront--claude-private"),
        ] {
            let config = CrostMemoryConfig {
                base_url: Some("https://hindsight.example/".to_string()),
                agent_id: agent.to_string(),
                ..CrostMemoryConfig::default()
            };
            let identity = ProjectIdentity {
                project_id: "p1".to_string(),
                slug: "ohm-storefront".to_string(),
                bank_prefix: None,
            };
            let provider =
                HindsightProvider::new(&config, &identity).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(provider.bank_name(RecallScope::Private), expected);
            assert_eq!(
                provider.bank_name(RecallScope::Shared),
                "crost--ohm-storefront--shared"
            );
        }
    }

    #[test]
    fn explicit_bank_prefix_wins() {
        let provider = provider(Some("crost--custom"));

        assert_eq!(
            provider.bank_name(RecallScope::Shared),
            "crost--custom--shared"
        );
    }

    #[test]
    fn urls_have_no_double_slash() {
        let provider = provider(None);

        assert_eq!(
            provider.memories_url(RecallScope::Shared),
            "https://hindsight.example/v1/default/banks/crost--ohm-storefront--shared/memories"
        );
    }

    #[test]
    fn debug_output_never_reveals_the_token() {
        let provider = provider(None);

        let rendered = format!("{provider:?}");

        assert!(rendered.contains("ApiToken"));
        assert!(!rendered.contains("HINDSIGHT_API_KEY="));
    }

    #[test]
    fn recall_payload_is_mapped_to_recall_items() {
        let payload: RecallResponse = serde_json::from_value(json!({
            "results": [{
                "id": "m-1",
                "text": "we chose sqlite",
                "type": "decision",
                "metadata": {"source_agent": "grok", "task_id": "T-42"},
                "mentioned_at": "2026-07-12T10:00:00Z",
                "scores": {"final": 0.87}
            }]
        }))
        .unwrap_or_else(|err| panic!("{err}"));

        let items = payload
            .results
            .into_iter()
            .map(WireRecallItem::into_item)
            .collect::<Vec<_>>();

        assert_eq!(
            items,
            vec![RecallItem {
                id: "m-1".to_string(),
                content: "we chose sqlite".to_string(),
                kind: Some("decision".to_string()),
                created_at: Some("2026-07-12T10:00:00Z".to_string()),
                source_agent: Some("grok".to_string()),
                task_id: Some("T-42".to_string()),
                score: 0.87,
            }]
        );
    }

    #[test]
    fn recall_payload_tolerates_missing_fields() {
        let payload: RecallResponse = serde_json::from_value(json!({"results": [{"id": "m-2"}]}))
            .unwrap_or_else(|err| panic!("{err}"));

        let item = payload
            .results
            .into_iter()
            .map(WireRecallItem::into_item)
            .next()
            .unwrap_or_default();

        assert_eq!(item.id, "m-2");
        assert_eq!(item.score, 0.0);
        assert_eq!(item.source_agent, None);
    }

    #[test]
    fn queries_are_truncated() {
        let long = "x".repeat(MAX_QUERY_CHARS + 500);

        assert_eq!(
            truncate_chars(&long, MAX_QUERY_CHARS).len(),
            MAX_QUERY_CHARS
        );
        assert_eq!(truncate_chars("héllo", 2), "hé");
    }
}
