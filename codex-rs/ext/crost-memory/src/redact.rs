//! Secret redaction applied to every string field of every outbound payload.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::NoExpand;
use regex::Regex;

use crate::types::Evidence;
use crate::types::PromoteRecord;
use crate::types::TestEvidence;
use crate::types::TurnRecord;

struct Pattern {
    label: &'static str,
    regex: Regex,
}

/// Ordered redaction patterns. Longer, more structural secrets run first so a
/// generic pattern cannot swallow half of them.
static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    [
        (
            "private_key",
            r"-----BEGIN[A-Z ]*PRIVATE KEY-----[\s\S]*?-----END[A-Z ]*PRIVATE KEY-----",
        ),
        (
            "jwt",
            r"eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
        ),
        ("aws_access_key_id", r"AKIA[0-9A-Z]{16}"),
        ("github_token", r"ghp_[A-Za-z0-9]{36,}"),
        ("github_token", r"github_pat_[A-Za-z0-9_]{22,}"),
        ("slack_token", r"xox[baprs]-[A-Za-z0-9-]{10,}"),
        ("api_key", r"sk-[A-Za-z0-9_-]{20,}"),
        ("bearer_token", r"(?i)bearer\s+[a-z0-9._~+/-]{16,}=*"),
        (
            "secret",
            r#"(?i)\b(api[_-]?key|secret|token|password|passwd)\b\s*[=:]\s*['"]?\S{6,}"#,
        ),
    ]
    .into_iter()
    .filter_map(|(label, pattern)| match Regex::new(pattern) {
        Ok(regex) => Some(Pattern { label, regex }),
        Err(err) => {
            tracing::error!(label, error = %err, "crost memory redaction pattern failed to compile");
            None
        }
    })
    .collect()
});

/// Replaces every recognized secret with `[REDACTED:<label>]`.
pub fn redact(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    for pattern in PATTERNS.iter() {
        if !pattern.regex.is_match(out.as_ref()) {
            continue;
        }
        let label = pattern.label;
        let replacement = format!("[REDACTED:{label}]");
        let replaced = pattern
            .regex
            .replace_all(out.as_ref(), NoExpand(replacement.as_str()))
            .into_owned();
        out = Cow::Owned(replaced);
    }
    out
}

fn redact_owned(text: &str) -> String {
    redact(text).into_owned()
}

fn redact_option(value: &Option<String>) -> Option<String> {
    value.as_deref().map(redact_owned)
}

fn redact_list(values: &[String]) -> Vec<String> {
    values.iter().map(|value| redact_owned(value)).collect()
}

/// Returns a copy of the record with every string field redacted.
pub fn redact_turn_record(record: &TurnRecord) -> TurnRecord {
    TurnRecord {
        objective: redact_option(&record.objective),
        decisions: redact_list(&record.decisions),
        files_changed: redact_list(&record.files_changed),
        tests: record
            .tests
            .iter()
            .map(|test| TestEvidence {
                cmd: redact_owned(&test.cmd),
                result: redact_owned(&test.result),
            })
            .collect(),
        blockers: redact_list(&record.blockers),
        next_step: redact_option(&record.next_step),
        task_id: redact_option(&record.task_id),
        branch: redact_option(&record.branch),
        commit: redact_option(&record.commit),
    }
}

/// Returns a copy of the record with every string field redacted.
pub fn redact_promote_record(record: &PromoteRecord) -> PromoteRecord {
    PromoteRecord {
        title: redact_owned(&record.title),
        summary: redact_owned(&record.summary),
        status: redact_option(&record.status),
        decisions: redact_list(&record.decisions),
        files: redact_list(&record.files),
        evidence: Evidence {
            commit: redact_option(&record.evidence.commit),
            test_cmd: redact_option(&record.evidence.test_cmd),
            test_result: redact_option(&record.evidence.test_result),
            pr: redact_option(&record.evidence.pr),
        },
        next_owner: redact_option(&record.next_owner),
        next_action: redact_option(&record.next_action),
        task_id: redact_option(&record.task_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn redacts_aws_access_key_ids() {
        let redacted = redact("creds AKIAIOSFODNN7EXAMPLE rotated");

        assert_eq!(redacted, "creds [REDACTED:aws_access_key_id] rotated");
    }

    #[test]
    fn redacts_github_tokens() {
        let classic = format!("token ghp_{}", "a".repeat(36));
        let fine_grained = format!("token github_pat_{}", "b".repeat(30));

        assert!(redact(&classic).contains("[REDACTED:github_token]"));
        assert!(!redact(&classic).contains("aaaa"));
        assert!(redact(&fine_grained).contains("[REDACTED:github_token]"));
    }

    #[test]
    fn redacts_generic_key_assignments() {
        assert_eq!(redact("api_key = 'hunter2hunter2'"), "[REDACTED:secret]");
        assert_eq!(redact("password: correcthorse"), "[REDACTED:secret]");
        assert!(redact("SECRET=abcdef123456").contains("[REDACTED:secret]"));
    }

    #[test]
    fn redacts_slack_tokens() {
        let token = format!("xoxb-{}", "1234567890".repeat(2));

        assert_eq!(redact(&token), "[REDACTED:slack_token]");
    }

    #[test]
    fn redacts_openai_style_keys() {
        let key = format!("sk-{}", "A1b2C3d4E5f6G7h8I9j0");

        assert_eq!(redact(&key), "[REDACTED:api_key]");
    }

    #[test]
    fn redacts_bearer_headers() {
        let header = "Authorization: Bearer abcdefghijklmnop0123";

        assert!(redact(header).contains("[REDACTED:bearer_token]"));
        assert!(!redact(header).contains("abcdefghijklmnop"));
    }

    #[test]
    fn redacts_jwts() {
        let jwt = format!(
            "eyJ{}.eyJ{}.{}",
            "A".repeat(12),
            "B".repeat(12),
            "C".repeat(12)
        );

        assert_eq!(redact(&jwt), "[REDACTED:jwt]");
    }

    #[test]
    fn redacts_pem_private_key_blocks() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIB\nAQID\n-----END RSA PRIVATE KEY-----";

        assert_eq!(redact(pem), "[REDACTED:private_key]");
    }

    #[test]
    fn leaves_ordinary_prose_and_code_untouched() {
        let prose = "Refactored the recall orchestrator so one scope timing out no longer \
                     discards the other. Updated src/recall.rs and added tests.";
        let code = "let items = provider.recall(scope, query, budget, max_items).await?;";

        assert_eq!(redact(prose), prose);
        assert_eq!(redact(code), code);
        assert!(matches!(redact(prose), Cow::Borrowed(_)));
    }

    #[test]
    fn redacts_every_field_of_a_turn_record() {
        let record = TurnRecord {
            objective: Some("rotate AKIAIOSFODNN7EXAMPLE".to_string()),
            decisions: vec!["use password: correcthorse".to_string()],
            files_changed: vec!["src/lib.rs".to_string()],
            tests: vec![TestEvidence {
                cmd: "run with token=abcdefgh".to_string(),
                result: "passed".to_string(),
            }],
            blockers: vec!["waiting on AKIAIOSFODNN7EXAMPLE".to_string()],
            next_step: Some("ship".to_string()),
            task_id: Some("T-42".to_string()),
            branch: Some("main".to_string()),
            commit: Some("abc1234".to_string()),
        };

        let redacted = redact_turn_record(&record);

        assert_eq!(
            redacted.objective.as_deref(),
            Some("rotate [REDACTED:aws_access_key_id]")
        );
        assert_eq!(
            redacted.decisions,
            vec!["use [REDACTED:secret]".to_string()]
        );
        assert_eq!(redacted.files_changed, vec!["src/lib.rs".to_string()]);
        assert_eq!(redacted.tests[0].cmd, "run with [REDACTED:secret]");
        assert!(redacted.blockers[0].contains("[REDACTED:aws_access_key_id]"));
        assert_eq!(redacted.next_step.as_deref(), Some("ship"));
    }

    #[test]
    fn redacts_every_field_of_a_promote_record() {
        let record = PromoteRecord {
            title: "AKIAIOSFODNN7EXAMPLE".to_string(),
            summary: "password: correcthorse".to_string(),
            status: Some("done".to_string()),
            decisions: vec!["AKIAIOSFODNN7EXAMPLE".to_string()],
            files: vec!["src/lib.rs".to_string()],
            evidence: Evidence {
                commit: Some("AKIAIOSFODNN7EXAMPLE".to_string()),
                test_cmd: Some("cargo test".to_string()),
                test_result: Some("passed".to_string()),
                pr: None,
            },
            next_owner: Some("codex".to_string()),
            next_action: Some("AKIAIOSFODNN7EXAMPLE".to_string()),
            task_id: Some("T-42".to_string()),
        };

        let redacted = redact_promote_record(&record);

        assert_eq!(redacted.title, "[REDACTED:aws_access_key_id]");
        assert_eq!(redacted.summary, "[REDACTED:secret]");
        assert_eq!(
            redacted.evidence.commit.as_deref(),
            Some("[REDACTED:aws_access_key_id]")
        );
        assert_eq!(
            redacted.next_action.as_deref(),
            Some("[REDACTED:aws_access_key_id]")
        );
        assert_eq!(redacted.files, vec!["src/lib.rs".to_string()]);
    }
}
