//! `crost_memory.promote_to_shared` — the ONLY path to the shared bank.

use std::sync::Arc;

use codex_extension_api::FunctionCallError;
use codex_extension_api::JsonToolOutput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolExecutorFuture;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

use crate::PROMOTE_TO_SHARED_TOOL_NAME;
use crate::flush::spawn_flush;
use crate::outbox::OutboxOp;
use crate::redact::redact_promote_record;
use crate::state::CrostMemoryRuntime;
use crate::types::Evidence;
use crate::types::PromoteKind;
use crate::types::PromoteOp;
use crate::types::PromoteRecord;
use crate::types::new_op_id;

use super::crost_memory_function_tool;
use super::crost_memory_tool_name;
use super::parse_args;

const MAX_TITLE_CHARS: usize = 160;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PromoteToSharedArgs {
    /// Kind of shared record. `decision` and `result` should carry evidence.
    kind: PromoteKind,
    /// Short human-readable title, one line.
    #[schemars(length(min = 1, max = 160))]
    title: String,
    /// Self-contained summary another agent can act on without this transcript.
    #[schemars(length(min = 1))]
    summary: String,
    /// Optional state label, for example `done`, `in-progress`, `reverted`.
    #[serde(default)]
    status: Option<String>,
    /// Decisions this record establishes.
    #[serde(default)]
    decisions: Vec<String>,
    /// Repository-relative paths this record is about.
    #[serde(default)]
    files: Vec<String>,
    /// Verifiable support: commit, test command plus result, or PR.
    #[serde(default)]
    evidence: Evidence,
    /// Agent or person who should pick this up next.
    #[serde(default)]
    next_owner: Option<String>,
    /// Concrete next action for the next owner.
    #[serde(default)]
    next_action: Option<String>,
    /// External task identifier when one exists.
    #[serde(default)]
    task_id: Option<String>,
    /// Id of the shared record this one corrects. History is preserved: a
    /// correction is a NEW record that links back to the old one.
    #[serde(default)]
    supersedes: Option<String>,
}

/// Acknowledgement returned to the model.
#[derive(Debug, Serialize, JsonSchema)]
struct PromoteToSharedResponse {
    /// Always true when the record was accepted for delivery.
    accepted: bool,
    /// Stable operation id; retries reuse it so no duplicate is created.
    op_id: String,
    /// Kind that was recorded.
    kind: PromoteKind,
    /// Title that was recorded, after redaction.
    title: String,
}

/// Promotes one record into the project's shared bank.
pub(crate) struct PromoteToSharedTool {
    runtime: Arc<CrostMemoryRuntime>,
}

impl PromoteToSharedTool {
    pub(crate) fn new(runtime: Arc<CrostMemoryRuntime>) -> Self {
        Self { runtime }
    }

    async fn handle_call(&self, call: ToolCall<'_>) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: PromoteToSharedArgs = parse_args(&call)?;
        let title = args.title.trim().to_string();
        if title.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "`title` must not be empty".to_string(),
            ));
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(FunctionCallError::RespondToModel(format!(
                "`title` must be at most {MAX_TITLE_CHARS} characters"
            )));
        }
        let summary = args.summary.trim().to_string();
        if summary.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "`summary` must not be empty".to_string(),
            ));
        }
        if args.kind.requires_evidence() && args.evidence.is_empty() {
            let kind = args.kind.as_str();
            return Err(FunctionCallError::RespondToModel(format!(
                "a `{kind}` record needs evidence: set `evidence.commit`, \
                 `evidence.test_cmd` plus `evidence.test_result`, or `evidence.pr`"
            )));
        }

        let record = redact_promote_record(&PromoteRecord {
            title,
            summary,
            status: args.status,
            decisions: args.decisions,
            files: args.files,
            evidence: args.evidence,
            next_owner: args.next_owner,
            next_action: args.next_action,
            task_id: args.task_id,
        });
        let op = PromoteOp {
            op_id: new_op_id(),
            kind: args.kind,
            record,
            supersedes: args.supersedes,
        };
        let response = PromoteToSharedResponse {
            accepted: true,
            op_id: op.op_id.clone(),
            kind: op.kind,
            title: op.record.title.clone(),
        };

        self.runtime
            .outbox
            .enqueue(OutboxOp::Promote(op))
            .map_err(|err| {
                FunctionCallError::Fatal(format!("could not queue the shared record: {err}"))
            })?;
        spawn_flush(Arc::clone(&self.runtime));

        Ok(Box::new(JsonToolOutput::new(json!(response))))
    }
}

impl<'call> ToolExecutor<ToolCall<'call>> for PromoteToSharedTool {
    fn tool_name(&self) -> ToolName {
        crost_memory_tool_name(PROMOTE_TO_SHARED_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        crost_memory_function_tool::<PromoteToSharedArgs, PromoteToSharedResponse>(
            PROMOTE_TO_SHARED_TOOL_NAME,
            "Publish one durable record to the project's shared project memory so other agents \
             can act on it. Use it for decisions, verified results, blockers, and handoffs only; \
             routine turn summaries are retained automatically and privately.",
        )
    }

    fn handle<'a>(&'a self, call: ToolCall<'call>) -> ToolExecutorFuture<'a>
    where
        ToolCall<'call>: 'a,
    {
        Box::pin(self.handle_call(call))
    }
}
