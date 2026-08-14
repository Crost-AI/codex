//! Turn observation and the typed record built from it.
//!
//! Everything here is derived from typed turn state that the host already
//! produced. No extra model call is made, hidden reasoning and raw transcripts
//! are never captured, and recalled memory is excluded by construction.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

use crate::recall::CROST_MEMORY_OPEN_TAG;
use crate::types::TestEvidence;
use crate::types::TurnRecord;

/// Command that ran during a turn, reduced to what a summary needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedCommand {
    /// Command line, already joined.
    pub command: String,
    /// Terminal status label reported by the host.
    pub status: String,
    /// Process exit code when the host reported one.
    pub exit_code: Option<i32>,
}

impl CapturedCommand {
    fn result(&self) -> String {
        match self.exit_code {
            Some(0) => "passed".to_string(),
            Some(code) => format!("failed (exit {code})"),
            None => self.status.clone(),
        }
    }

    fn is_verification(&self) -> bool {
        const NEEDLES: [&str; 10] = [
            "test", "pytest", "jest", "vitest", "clippy", "lint", "check", "tsc", "mypy", "build",
        ];
        let command = self.command.to_ascii_lowercase();
        NEEDLES.iter().any(|needle| command.contains(needle))
    }
}

/// Everything observed for one turn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrostTurnCapture {
    /// First line of the user's submitted prompt, stored at recall time.
    pub objective: Option<String>,
    /// Host turn id.
    pub turn_id: Option<String>,
    /// Task id when the host exposes one.
    pub task_id: Option<String>,
    /// Branch when the host already knows it. Never derived by running git.
    pub branch: Option<String>,
    /// Commit when the host already knows it. Never derived by running git.
    pub commit: Option<String>,
    /// Agent message texts, in order.
    pub agent_messages: Vec<String>,
    /// Command executions observed during the turn.
    pub commands: Vec<CapturedCommand>,
    /// Paths reported by file-change items.
    pub files_changed: Vec<String>,
    /// Hash of the injected memory block, so it can never be captured back.
    pub injected_block_hash: Option<u64>,
    /// Stats from the pre-turn recall, for diagnostics.
    pub recall_private_n: usize,
    pub recall_shared_n: usize,
}

impl CrostTurnCapture {
    /// Records an agent message unless it is recalled memory.
    pub fn push_agent_message(&mut self, text: String) {
        if self.is_recalled_memory(&text) {
            return;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.agent_messages.push(text);
    }

    /// Records one command execution.
    pub fn push_command(&mut self, command: CapturedCommand) {
        if command.command.trim().is_empty() {
            return;
        }
        if let Some(existing) = self
            .commands
            .iter_mut()
            .find(|existing| existing.command == command.command)
        {
            *existing = command;
            return;
        }
        self.commands.push(command);
    }

    /// Records one changed path.
    pub fn push_file_changed(&mut self, path: String) {
        if path.trim().is_empty() || self.files_changed.contains(&path) {
            return;
        }
        self.files_changed.push(path);
    }

    /// Whether `text` is the memory block injected into this turn.
    pub fn is_recalled_memory(&self, text: &str) -> bool {
        let trimmed = text.trim_start();
        if trimmed.starts_with(CROST_MEMORY_OPEN_TAG) {
            return true;
        }
        self.injected_block_hash
            .is_some_and(|hash| hash == text_hash(text))
    }

    /// Builds the structured record retained for this turn.
    pub fn to_turn_record(&self) -> TurnRecord {
        let tests = self
            .commands
            .iter()
            .filter(|command| command.is_verification())
            .map(|command| TestEvidence {
                cmd: command.command.clone(),
                result: command.result(),
            })
            .collect::<Vec<_>>();
        let final_message = self.agent_messages.last().map(String::as_str);
        let blockers = final_message
            .map(|message| extract_labelled(message, &["blocker:", "blocked:", "blocked by:"]))
            .unwrap_or_default();
        let next_step = final_message
            .and_then(|message| {
                extract_labelled(message, &["next step:", "next steps:", "next:"])
                    .into_iter()
                    .next()
            })
            .filter(|next_step| !next_step.is_empty());
        let decisions = final_message
            .map(|message| extract_labelled(message, &["decision:", "decided:"]))
            .unwrap_or_default();

        TurnRecord {
            objective: self.objective.clone(),
            decisions,
            files_changed: self.files_changed.clone(),
            tests,
            blockers,
            next_step,
            task_id: self.task_id.clone(),
            branch: self.branch.clone(),
            commit: self.commit.clone(),
        }
    }
}

/// Stable hash used to recognize the injected block.
pub fn text_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Extracts the first line of `text`, trimmed and length-bounded.
pub fn first_line(text: &str, max_chars: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut out = String::new();
    for (index, ch) in line.chars().enumerate() {
        if index >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    Some(out)
}

fn extract_labelled(message: &str, labels: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in message.lines() {
        let line = raw_line.trim().trim_start_matches(['-', '*', '#', ' ']);
        let lowered = line.to_ascii_lowercase();
        for label in labels {
            let Some(rest) = lowered.strip_prefix(label) else {
                continue;
            };
            // `to_ascii_lowercase` preserves byte length, so this offset is a
            // valid boundary in the original line.
            let start = line.len().saturating_sub(rest.len());
            let value = line.get(start..).unwrap_or_default().trim();
            if !value.is_empty() && !out.contains(&value.to_string()) {
                out.push(value.to_string());
            }
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::render_block;
    use pretty_assertions::assert_eq;

    fn command(command: &str, exit_code: Option<i32>) -> CapturedCommand {
        CapturedCommand {
            command: command.to_string(),
            status: "completed".to_string(),
            exit_code,
        }
    }

    #[test]
    fn record_uses_files_and_verification_commands() {
        let mut capture = CrostTurnCapture {
            objective: Some("add the outbox".to_string()),
            ..CrostTurnCapture::default()
        };
        capture.push_file_changed("src/outbox.rs".to_string());
        capture.push_file_changed("src/outbox.rs".to_string());
        capture.push_command(command(
            "cargo test -p codex-crost-memory-extension",
            Some(0),
        ));
        capture.push_command(command("ls -la", Some(0)));
        capture.push_command(command("cargo clippy", Some(101)));

        let record = capture.to_turn_record();

        assert_eq!(record.files_changed, vec!["src/outbox.rs".to_string()]);
        assert_eq!(
            record.tests,
            vec![
                TestEvidence {
                    cmd: "cargo test -p codex-crost-memory-extension".to_string(),
                    result: "passed".to_string(),
                },
                TestEvidence {
                    cmd: "cargo clippy".to_string(),
                    result: "failed (exit 101)".to_string(),
                },
            ]
        );
        assert!(record.is_meaningful());
    }

    #[test]
    fn next_step_blockers_and_decisions_come_from_the_final_message() {
        let mut capture = CrostTurnCapture::default();
        capture.push_agent_message("earlier chatter".to_string());
        capture.push_agent_message(
            "Done.\n- Decision: keep the outbox bounded\n- Blocker: hindsight is offline\n\
             Next step: wire diagnostics"
                .to_string(),
        );

        let record = capture.to_turn_record();

        assert_eq!(
            record.decisions,
            vec!["keep the outbox bounded".to_string()]
        );
        assert_eq!(record.blockers, vec!["hindsight is offline".to_string()]);
        assert_eq!(record.next_step.as_deref(), Some("wire diagnostics"));
    }

    #[test]
    fn branch_and_commit_are_only_used_when_already_captured() {
        let capture = CrostTurnCapture::default();

        let record = capture.to_turn_record();

        assert_eq!(record.branch, None);
        assert_eq!(record.commit, None);
        assert!(!record.is_meaningful());
    }

    #[test]
    fn recalled_memory_is_never_captured() {
        let block = render_block(
            "codex",
            "ohm",
            &["[shared · a] secret lore".to_string()],
            &[],
        );
        let mut capture = CrostTurnCapture {
            injected_block_hash: Some(text_hash(&block)),
            ..CrostTurnCapture::default()
        };

        capture.push_agent_message(block.clone());
        capture.push_agent_message(format!("  {block}"));

        assert!(capture.agent_messages.is_empty());
        assert!(!capture.to_turn_record().is_meaningful());
    }

    #[test]
    fn first_line_is_trimmed_and_bounded() {
        assert_eq!(
            first_line("\n\n  Implement the outbox  \nand more\n", 100).as_deref(),
            Some("Implement the outbox")
        );
        assert_eq!(first_line("abcdef", 3).as_deref(), Some("abc…"));
        assert_eq!(first_line("   \n  ", 10), None);
    }
}
