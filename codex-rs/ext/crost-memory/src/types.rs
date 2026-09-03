//! Provider-agnostic value types shared by every Crost memory module.

use rand::Rng;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Bank a recall or write targets.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallScope {
    /// The calling agent's own bank.
    Private,
    /// The bank shared by every agent working on the project.
    Shared,
}

impl RecallScope {
    /// Stable lowercase label used in rendered blocks and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Shared => "shared",
        }
    }
}

/// One memory returned by a recall.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallItem {
    pub id: String,
    pub content: String,
    pub kind: Option<String>,
    pub created_at: Option<String>,
    pub source_agent: Option<String>,
    pub task_id: Option<String>,
    pub score: f64,
}

/// One command that was run as evidence for a turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TestEvidence {
    pub cmd: String,
    pub result: String,
}

/// Compact structured summary of one completed turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub objective: Option<String>,
    pub decisions: Vec<String>,
    pub files_changed: Vec<String>,
    pub tests: Vec<TestEvidence>,
    pub blockers: Vec<String>,
    pub next_step: Option<String>,
    pub task_id: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
}

impl TurnRecord {
    /// Whether this record carries anything worth retaining.
    pub fn is_meaningful(&self) -> bool {
        self.objective
            .as_ref()
            .is_some_and(|objective| !objective.trim().is_empty())
            || !self.decisions.is_empty()
            || !self.files_changed.is_empty()
            || !self.tests.is_empty()
            || !self.blockers.is_empty()
    }

    /// Compact deterministic markdown used as the stored record body.
    pub fn render(&self) -> String {
        let mut lines = vec!["# Turn summary".to_string()];
        if let Some(objective) = non_empty(self.objective.as_deref()) {
            lines.push(format!("Objective: {objective}"));
        }
        if let Some(task_id) = non_empty(self.task_id.as_deref()) {
            lines.push(format!("Task: {task_id}"));
        }
        if let Some(branch) = non_empty(self.branch.as_deref()) {
            lines.push(format!("Branch: {branch}"));
        }
        if let Some(commit) = non_empty(self.commit.as_deref()) {
            lines.push(format!("Commit: {commit}"));
        }
        push_list(&mut lines, "Decisions", &self.decisions);
        push_list(&mut lines, "Files changed", &self.files_changed);
        if !self.tests.is_empty() {
            lines.push("Tests:".to_string());
            for test in &self.tests {
                let cmd = &test.cmd;
                let result = &test.result;
                lines.push(format!("- {cmd} => {result}"));
            }
        }
        push_list(&mut lines, "Blockers", &self.blockers);
        if let Some(next_step) = non_empty(self.next_step.as_deref()) {
            lines.push(format!("Next step: {next_step}"));
        }
        lines.join("\n")
    }
}

/// Kind of shared record a promotion creates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PromoteKind {
    Decision,
    Result,
    Blocker,
    Handoff,
}

impl PromoteKind {
    /// Stable lowercase label used in tags and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Result => "result",
            Self::Blocker => "blocker",
            Self::Handoff => "handoff",
        }
    }

    /// Whether records of this kind are expected to carry evidence.
    pub fn requires_evidence(self) -> bool {
        matches!(self, Self::Decision | Self::Result)
    }
}

/// Verifiable support for a promoted record.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub commit: Option<String>,
    pub test_cmd: Option<String>,
    pub test_result: Option<String>,
    pub pr: Option<String>,
}

impl Evidence {
    /// Whether any evidence field is populated.
    pub fn is_empty(&self) -> bool {
        non_empty(self.commit.as_deref()).is_none()
            && non_empty(self.test_cmd.as_deref()).is_none()
            && non_empty(self.test_result.as_deref()).is_none()
            && non_empty(self.pr.as_deref()).is_none()
    }
}

/// Body of a record promoted into the shared bank.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PromoteRecord {
    pub title: String,
    pub summary: String,
    pub status: Option<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub evidence: Evidence,
    pub next_owner: Option<String>,
    pub next_action: Option<String>,
    pub task_id: Option<String>,
}

impl PromoteRecord {
    /// Compact deterministic markdown used as the stored record body.
    pub fn render(&self, kind: PromoteKind) -> String {
        let title = &self.title;
        let kind_label = kind.as_str();
        let mut lines = vec![format!("# {title}"), format!("Kind: {kind_label}")];
        if let Some(status) = non_empty(self.status.as_deref()) {
            lines.push(format!("Status: {status}"));
        }
        if let Some(task_id) = non_empty(self.task_id.as_deref()) {
            lines.push(format!("Task: {task_id}"));
        }
        let summary = &self.summary;
        lines.push(format!("Summary: {summary}"));
        push_list(&mut lines, "Decisions", &self.decisions);
        push_list(&mut lines, "Files", &self.files);
        if !self.evidence.is_empty() {
            lines.push("Evidence:".to_string());
            for (label, value) in [
                ("commit", self.evidence.commit.as_deref()),
                ("test_cmd", self.evidence.test_cmd.as_deref()),
                ("test_result", self.evidence.test_result.as_deref()),
                ("pr", self.evidence.pr.as_deref()),
            ] {
                if let Some(value) = non_empty(value) {
                    lines.push(format!("- {label}: {value}"));
                }
            }
        }
        if let Some(next_owner) = non_empty(self.next_owner.as_deref()) {
            lines.push(format!("Next owner: {next_owner}"));
        }
        if let Some(next_action) = non_empty(self.next_action.as_deref()) {
            lines.push(format!("Next action: {next_action}"));
        }
        lines.join("\n")
    }
}

/// One idempotent private-bank retention.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetainOp {
    pub op_id: String,
    pub record: TurnRecord,
}

/// One idempotent shared-bank promotion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromoteOp {
    pub op_id: String,
    pub kind: PromoteKind,
    pub record: PromoteRecord,
    pub supersedes: Option<String>,
}

/// Health snapshot for the configured provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub healthy: bool,
    pub provider: &'static str,
    pub endpoint: Option<String>,
    pub latency_ms: Option<u64>,
    pub detail: Option<String>,
}

/// Generates one stable operation id.
///
/// Callers must compute this ONCE per logical operation and store it with the
/// operation; retries reuse the same value so the server can dedupe.
pub fn new_op_id() -> String {
    // Hindsight validates `operation_id` as an RFC 4122 UUID.
    let bits: u128 = rand::rng().random();
    let hex = format!("{bits:032x}");
    format!(
        "{}-{}-4{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        {
            let nibble = u8::from_str_radix(&hex[16..17], 16).unwrap_or(0);
            format!("{:x}{}", (nibble & 0x3) | 0x8, &hex[17..20])
        },
        &hex[20..32]
    )
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn push_list(lines: &mut Vec<String>, label: &str, values: &[String]) {
    let values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return;
    }
    lines.push(format!("{label}:"));
    for value in values {
        lines.push(format!("- {value}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn empty_turn_record_is_not_meaningful() {
        assert!(!TurnRecord::default().is_meaningful());
    }

    #[test]
    fn blank_objective_alone_is_not_meaningful() {
        let record = TurnRecord {
            objective: Some("   ".to_string()),
            ..TurnRecord::default()
        };

        assert!(!record.is_meaningful());
    }

    #[test]
    fn any_populated_field_makes_a_record_meaningful() {
        let with_files = TurnRecord {
            files_changed: vec!["src/lib.rs".to_string()],
            ..TurnRecord::default()
        };
        let with_tests = TurnRecord {
            tests: vec![TestEvidence {
                cmd: "cargo test".to_string(),
                result: "passed".to_string(),
            }],
            ..TurnRecord::default()
        };

        assert!(with_files.is_meaningful());
        assert!(with_tests.is_meaningful());
    }

    #[test]
    fn turn_record_renders_deterministic_markdown() {
        let record = TurnRecord {
            objective: Some("Add outbox".to_string()),
            decisions: vec!["Bound the queue at 200".to_string()],
            files_changed: vec!["src/outbox.rs".to_string()],
            tests: vec![TestEvidence {
                cmd: "cargo test".to_string(),
                result: "passed".to_string(),
            }],
            blockers: Vec::new(),
            next_step: Some("Wire diagnostics".to_string()),
            task_id: Some("T-42".to_string()),
            branch: None,
            commit: None,
        };

        assert_eq!(
            record.render(),
            "# Turn summary\n\
             Objective: Add outbox\n\
             Task: T-42\n\
             Decisions:\n\
             - Bound the queue at 200\n\
             Files changed:\n\
             - src/outbox.rs\n\
             Tests:\n\
             - cargo test => passed\n\
             Next step: Wire diagnostics"
        );
        assert_eq!(record.render(), record.clone().render());
    }

    #[test]
    fn op_ids_are_prefixed_and_unique() {
        let first = new_op_id();
        let second = new_op_id();

        assert_eq!(first.len(), 36);
        assert_eq!(&first[14..15], "4");
        let hex: String = first.chars().filter(|c| *c != '-').collect();
        assert_eq!(hex.len(), 32);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn promote_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&PromoteKind::Handoff).unwrap_or_default(),
            "\"handoff\""
        );
        assert!(PromoteKind::Decision.requires_evidence());
        assert!(!PromoteKind::Handoff.requires_evidence());
    }
}
