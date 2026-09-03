//! Bounded persistent queue that keeps turn completion off the network path.
//!
//! One JSON file per operation. Retries re-send the SAME op id, so the server
//! dedupes and a retried operation can never create a duplicate memory.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde::Serialize;

use crate::provider::MemoryError;
use crate::provider::MemoryProvider;
use crate::types::PromoteOp;
use crate::types::RetainOp;

/// Maximum number of queued operations before the oldest are dropped.
pub const MAX_QUEUED_OPS: usize = 200;

/// Longest backoff between flush attempts for one operation.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// One queued operation, tagged so `flush` can dispatch it.
///
/// External tagging is deliberate: `PromoteOp` already carries its own `kind`
/// field, so an internally tagged representation would collide with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxOp {
    Retain(RetainOp),
    Promote(PromoteOp),
}

impl OutboxOp {
    /// Stable op id reused by every retry.
    pub fn op_id(&self) -> &str {
        match self {
            Self::Retain(op) => &op.op_id,
            Self::Promote(op) => &op.op_id,
        }
    }

    /// Stable label used in tracing fields.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Retain(_) => "retain",
            Self::Promote(_) => "promote",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OutboxEntry {
    op: OutboxOp,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    next_attempt_at_ms: u64,
}

/// Result of one flush pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FlushOutcome {
    /// Operations the provider accepted.
    pub sent: usize,
    /// Operations dropped because the provider rejected them as invalid.
    pub dropped: usize,
    /// Operations rescheduled after a retryable failure.
    pub retried: usize,
    /// Whether the pass stopped because credentials were rejected.
    pub auth_failed: bool,
    /// Operations still queued after the pass.
    pub remaining: usize,
}

/// Persistent bounded outbox rooted at a caller-supplied directory.
#[derive(Clone, Debug)]
pub struct Outbox {
    dir: PathBuf,
    max_queued_ops: usize,
}

impl Outbox {
    /// Creates an outbox at `root/<project_id>`.
    pub fn new(root: &Path, project_id: &str) -> Self {
        Self {
            dir: root.join(sanitize_component(project_id)),
            max_queued_ops: MAX_QUEUED_OPS,
        }
    }

    /// Overrides the queue bound (tests).
    #[must_use]
    pub fn with_max_queued_ops(mut self, max_queued_ops: usize) -> Self {
        self.max_queued_ops = max_queued_ops.max(1);
        self
    }

    /// Directory backing this outbox.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends one operation, enforcing the queue bound.
    pub fn enqueue(&self, op: OutboxOp) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let entry = OutboxEntry {
            attempts: 0,
            next_attempt_at_ms: now_ms(),
            op,
        };
        let millis = now_ms();
        let op_id = sanitize_component(entry.op.op_id());
        let path = self.dir.join(format!("{millis:016}-{op_id}.json"));
        let tmp = path.with_extension("json.tmp");
        let encoded = serde_json::to_vec(&entry).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, encoded)?;
        std::fs::rename(&tmp, &path)?;
        self.enforce_bound();
        Ok(path)
    }

    /// Number of queued operations.
    pub fn depth(&self) -> usize {
        self.entry_paths().len()
    }

    /// Age of the oldest queued operation.
    pub fn oldest_age(&self) -> Option<Duration> {
        let oldest = self.entry_paths().into_iter().next()?;
        let millis = leading_millis(&oldest)?;
        let now = now_ms();
        Some(Duration::from_millis(now.saturating_sub(millis)))
    }

    /// Attempts every due operation, oldest first.
    ///
    /// A retryable failure reschedules the operation with exponential backoff
    /// and stops the pass (circuit break) so a dead endpoint is contacted once.
    pub async fn flush(&self, provider: &dyn MemoryProvider) -> FlushOutcome {
        let mut outcome = FlushOutcome::default();
        for path in self.entry_paths() {
            let Some(mut entry) = self.read_entry(&path) else {
                remove_quietly(&path);
                continue;
            };
            if entry.next_attempt_at_ms > now_ms() {
                continue;
            }

            let result = match &entry.op {
                OutboxOp::Retain(op) => provider.retain_private(op).await,
                OutboxOp::Promote(op) => provider.promote_shared(op).await.map(|_| ()),
            };
            match result {
                Ok(()) => {
                    outcome.sent += 1;
                    remove_quietly(&path);
                }
                Err(MemoryError::Unavailable(detail)) => {
                    entry.attempts = entry.attempts.saturating_add(1);
                    let delay_ms =
                        u64::try_from(backoff(entry.attempts).as_millis()).unwrap_or(u64::MAX);
                    entry.next_attempt_at_ms = now_ms().saturating_add(delay_ms);
                    outcome.retried += 1;
                    self.write_entry(&path, &entry);
                    tracing::debug!(
                        op_kind = entry.op.label(),
                        attempts = entry.attempts,
                        detail = %detail,
                        "crost memory op rescheduled"
                    );
                    break;
                }
                Err(MemoryError::Auth(detail)) => {
                    outcome.auth_failed = true;
                    tracing::warn!(
                        op_kind = entry.op.label(),
                        detail = %detail,
                        "crost memory credentials rejected; queued operations are paused"
                    );
                    break;
                }
                Err(MemoryError::Invalid(detail)) => {
                    outcome.dropped += 1;
                    tracing::warn!(
                        op_kind = entry.op.label(),
                        detail = %detail,
                        "crost memory op dropped as invalid"
                    );
                    remove_quietly(&path);
                }
            }
        }
        outcome.remaining = self.depth();
        outcome
    }

    fn entry_paths(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn read_entry(&self, path: &Path) -> Option<OutboxEntry> {
        let contents = std::fs::read(path).ok()?;
        serde_json::from_slice(&contents).ok()
    }

    fn write_entry(&self, path: &Path, entry: &OutboxEntry) {
        let Ok(encoded) = serde_json::to_vec(entry) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, encoded).is_ok() && std::fs::rename(&tmp, path).is_err() {
            remove_quietly(&tmp);
        }
    }

    fn enforce_bound(&self) {
        let paths = self.entry_paths();
        if paths.len() <= self.max_queued_ops {
            return;
        }
        let overflow = paths.len() - self.max_queued_ops;
        for path in paths.into_iter().take(overflow) {
            tracing::warn!(
                dropped_ops = 1,
                "crost memory outbox is full; dropping the oldest queued op"
            );
            remove_quietly(&path);
        }
    }
}

/// Exponential backoff for the given attempt count, capped at [`MAX_BACKOFF`].
pub fn backoff(attempts: u32) -> Duration {
    let seconds = 1u64.checked_shl(attempts.min(31)).unwrap_or(u64::MAX);
    Duration::from_secs(seconds).min(MAX_BACKOFF)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn leading_millis(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let (millis, _) = name.split_once('-')?;
    millis.parse().ok()
}

fn remove_quietly(path: &Path) {
    if let Err(err) = std::fs::remove_file(path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(error = %err, "crost memory could not remove an outbox file");
    }
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "unknown".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeProvider;
    use crate::types::PromoteKind;
    use crate::types::PromoteRecord;
    use crate::types::TurnRecord;
    use pretty_assertions::assert_eq;

    fn retain_op(op_id: &str) -> OutboxOp {
        OutboxOp::Retain(RetainOp {
            op_id: op_id.to_string(),
            record: TurnRecord {
                objective: Some(format!("objective for {op_id}")),
                ..TurnRecord::default()
            },
        })
    }

    fn promote_op(op_id: &str) -> OutboxOp {
        OutboxOp::Promote(PromoteOp {
            op_id: op_id.to_string(),
            kind: PromoteKind::Decision,
            record: PromoteRecord {
                title: "decision".to_string(),
                summary: "we chose sqlite".to_string(),
                ..PromoteRecord::default()
            },
            supersedes: None,
        })
    }

    fn outbox(root: &Path) -> Outbox {
        Outbox::new(root, "project-1")
    }

    #[tokio::test]
    async fn enqueue_then_flush_delivers_both_op_kinds() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path());
        let provider = FakeProvider::new();
        outbox
            .enqueue(retain_op("cm-a"))
            .unwrap_or_else(|err| panic!("{err}"));
        outbox
            .enqueue(promote_op("cm-b"))
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(outbox.depth(), 2);

        let outcome = outbox.flush(&provider).await;

        assert_eq!(outcome.sent, 2);
        assert_eq!(outcome.remaining, 0);
        assert_eq!(outbox.depth(), 0);
        assert_eq!(provider.retained().len(), 1);
        assert_eq!(provider.promoted().len(), 1);
    }

    #[tokio::test]
    async fn retry_reuses_the_same_op_id_and_never_duplicates() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path());
        let provider = FakeProvider::new();
        provider.fail_retain(MemoryError::Unavailable("offline".to_string()));
        outbox
            .enqueue(retain_op("cm-stable"))
            .unwrap_or_else(|err| panic!("{err}"));

        let first = outbox.flush(&provider).await;
        assert_eq!(first.retried, 1);
        assert_eq!(outbox.depth(), 1);

        // Force the entry to be due again, then let the provider recover.
        force_due(&outbox);
        provider.clear_write_failure();
        let second = outbox.flush(&provider).await;
        force_due(&outbox);
        let third = outbox.flush(&provider).await;

        assert_eq!(second.sent, 1);
        assert_eq!(third.sent, 0);
        assert_eq!(provider.retained().len(), 1);
        assert_eq!(provider.retained()[0].op_id, "cm-stable");
    }

    #[tokio::test]
    async fn unavailable_failures_circuit_break_the_pass() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path());
        let provider = FakeProvider::new();
        provider.fail_retain(MemoryError::Unavailable("offline".to_string()));
        for index in 0..3 {
            outbox
                .enqueue(retain_op(&format!("cm-{index}")))
                .unwrap_or_else(|err| panic!("{err}"));
        }

        let outcome = outbox.flush(&provider).await;

        assert_eq!(outcome.retried, 1);
        assert_eq!(outcome.sent, 0);
        assert_eq!(outcome.remaining, 3);
    }

    #[tokio::test]
    async fn auth_failures_leave_the_file_and_report_once() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path());
        let provider = FakeProvider::new();
        provider.set_auth_failure(true);
        outbox
            .enqueue(retain_op("cm-auth"))
            .unwrap_or_else(|err| panic!("{err}"));

        let outcome = outbox.flush(&provider).await;

        assert!(outcome.auth_failed);
        assert_eq!(outcome.sent, 0);
        assert_eq!(outbox.depth(), 1);
    }

    #[tokio::test]
    async fn invalid_ops_are_dropped_and_the_pass_continues() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path());
        let provider = FakeProvider::new();
        provider.fail_retain(MemoryError::Invalid("bad request".to_string()));
        outbox
            .enqueue(retain_op("cm-x"))
            .unwrap_or_else(|err| panic!("{err}"));
        outbox
            .enqueue(retain_op("cm-y"))
            .unwrap_or_else(|err| panic!("{err}"));

        let outcome = outbox.flush(&provider).await;

        assert_eq!(outcome.dropped, 2);
        assert_eq!(outcome.remaining, 0);
    }

    #[test]
    fn the_bound_drops_the_oldest_entries() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path()).with_max_queued_ops(3);

        for index in 0..6 {
            outbox
                .enqueue(retain_op(&format!("cm-{index:02}")))
                .unwrap_or_else(|err| panic!("{err}"));
        }

        assert_eq!(outbox.depth(), 3);
        let remaining = outbox
            .entry_paths()
            .into_iter()
            .filter_map(|path| outbox.read_entry(&path))
            .map(|entry| entry.op.op_id().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            remaining,
            vec![
                "cm-03".to_string(),
                "cm-04".to_string(),
                "cm-05".to_string()
            ]
        );
    }

    #[test]
    fn backoff_grows_exponentially_and_is_capped() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(4));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(20), MAX_BACKOFF);
    }

    #[tokio::test]
    async fn entries_that_are_not_due_are_skipped() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path());
        let provider = FakeProvider::new();
        provider.fail_retain(MemoryError::Unavailable("offline".to_string()));
        outbox
            .enqueue(retain_op("cm-later"))
            .unwrap_or_else(|err| panic!("{err}"));
        outbox.flush(&provider).await;
        provider.clear_write_failure();

        let outcome = outbox.flush(&provider).await;

        assert_eq!(outcome.sent, 0);
        assert_eq!(outcome.remaining, 1);
    }

    #[test]
    fn depth_and_oldest_age_report_queue_state() {
        let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let outbox = outbox(tmp.path());

        assert_eq!(outbox.depth(), 0);
        assert_eq!(outbox.oldest_age(), None);

        outbox
            .enqueue(retain_op("cm-age"))
            .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(outbox.depth(), 1);
        assert!(outbox.oldest_age().is_some_and(|age| age < MAX_BACKOFF));
    }

    fn force_due(outbox: &Outbox) {
        for path in outbox.entry_paths() {
            let Some(mut entry) = outbox.read_entry(&path) else {
                continue;
            };
            entry.next_attempt_at_ms = 0;
            outbox.write_entry(&path, &entry);
        }
    }
}
