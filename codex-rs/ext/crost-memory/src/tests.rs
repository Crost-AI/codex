//! Registry-level tests exercising every contributor role end to end.
//!
//! No real network is used: the fake provider backs every scenario.

use std::marker::PhantomData;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use codex_utils_absolute_path::AbsolutePathBuf;

use codex_extension_api::ConversationHistory;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::NoopTurnItemEmitter;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolCallSource;
use codex_extension_api::ToolName;
use codex_extension_api::ToolPayload;
use codex_extension_api::TurnAbortInput;
use codex_extension_api::TurnInputContext;
use codex_extension_api::TurnInputEnvironment;
use codex_extension_api::TurnStopInput;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::user_input::UserInput;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use crate::CROST_MEMORY_TOOLS_NAMESPACE;
use crate::PROMOTE_TO_SHARED_TOOL_NAME;
use crate::config::CrostMemoryConfig;
use crate::config::ProviderKind;
use crate::fake::FakeProvider;
use crate::flush::flush_now;
use crate::identity::DEFAULT_PROJECT_FILE;
use crate::outbox::Outbox;
use crate::provider::MemoryProvider;
use crate::state::CrostMemoryExtensionConfig;
use crate::state::CrostMemoryRuntime;
use crate::state::CrostMemoryThreadState;
use crate::state::CrostMemoryTurnState;
use crate::types::PromoteKind;
use crate::types::RecallItem;
use crate::types::RecallScope;

const DESCRIPTOR: &str = "apiVersion: memory.crost/v1\n\
     projectId: 01J8ZQ-CROST-TEST\n\
     slug: ohm-storefront\n";

/// Host config stand-in: the real host maps its own `Config` through the same
/// closure shape.
#[derive(Clone, Debug)]
struct HostConfig {
    extension: CrostMemoryExtensionConfig,
}

fn workspace_with_descriptor() -> TempDir {
    let tmp = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let path = tmp.path().join(DEFAULT_PROJECT_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|err| panic!("create dirs: {err}"));
    }
    std::fs::write(&path, DESCRIPTOR).unwrap_or_else(|err| panic!("write descriptor: {err}"));
    tmp
}

fn host_config(cwd: &Path, outbox_root: &Path, enabled: bool) -> HostConfig {
    HostConfig {
        extension: CrostMemoryExtensionConfig {
            memory: CrostMemoryConfig {
                enabled,
                provider: ProviderKind::Fake,
                agent_id: "codex".to_string(),
                ..CrostMemoryConfig::default()
            },
            cwd: cwd.to_path_buf(),
            outbox_root: outbox_root.to_path_buf(),
        },
    }
}

fn registry() -> codex_extension_api::ExtensionRegistry<HostConfig> {
    let mut builder = ExtensionRegistryBuilder::<HostConfig>::new();
    crate::install(&mut builder, |config: &HostConfig| config.extension.clone());
    builder.build()
}

async fn start_thread(
    registry: &codex_extension_api::ExtensionRegistry<HostConfig>,
    config: &HostConfig,
    session_store: &ExtensionData,
    thread_store: &ExtensionData,
) {
    let session_source = SessionSource::Cli;
    for contributor in registry.thread_lifecycle_contributors() {
        contributor
            .on_thread_start(ThreadStartInput {
                config,
                session_source: &session_source,
                persistent_thread_state_available: true,
                environments: &[],
                mcp_resource_client: None,
                extension_metrics: None,
                session_store,
                thread_store,
            })
            .await;
    }
}

fn turn_input(cwd: &Path, text: &str) -> TurnInputContext<'static> {
    let abs = AbsolutePathBuf::from_absolute_path(cwd)
        .unwrap_or_else(|err| panic!("absolute cwd: {err}"));
    TurnInputContext {
        turn_id: "turn-1".to_string(),
        user_input: vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
        environments: vec![TurnInputEnvironment {
            environment_id: "local".to_string(),
            cwd: PathUri::from_abs_path(&abs),
            is_primary: true,
            _lifetime: PhantomData,
        }],
    }
}

/// Replaces the thread runtime's provider with a fake we can drive, keeping the
/// identity and outbox the extension resolved.
fn swap_in_fake(thread_store: &ExtensionData) -> (Arc<CrostMemoryRuntime>, FakeProvider) {
    let state = thread_store
        .get::<CrostMemoryThreadState>()
        .unwrap_or_else(|| panic!("thread state should be seeded"));
    let runtime = state
        .runtime()
        .unwrap_or_else(|| panic!("memory should be enabled"));
    let fake = FakeProvider::new();
    let replacement = Arc::new(CrostMemoryRuntime::new(
        runtime.config.clone(),
        runtime.identity.clone(),
        Arc::new(fake.clone()) as Arc<dyn MemoryProvider>,
        Arc::clone(&runtime.outbox),
    ));
    thread_store.insert(CrostMemoryThreadState::Enabled(Arc::clone(&replacement)));
    (replacement, fake)
}

fn agent_message(text: &str) -> TurnItem {
    serde_json::from_value(json!({
        "type": "AgentMessage",
        "id": "msg-1",
        "content": [{"type": "Text", "text": text}],
    }))
    .unwrap_or_else(|err| panic!("agent message item: {err}"))
}

fn command_execution(command: &[&str], exit_code: i32) -> TurnItem {
    serde_json::from_value(json!({
        "type": "CommandExecution",
        "id": "cmd-1",
        "command": command,
        "cwd": "file:///tmp/crost-memory-test",
        "parsed_cmd": [],
        "source": "agent",
        "status": "completed",
        "exit_code": exit_code,
    }))
    .unwrap_or_else(|err| panic!("command execution item: {err}"))
}

fn file_change(path: &str) -> TurnItem {
    serde_json::from_value(json!({
        "type": "FileChange",
        "id": "change-1",
        "changes": {path: {"type": "add", "content": "hello"}},
    }))
    .unwrap_or_else(|err| panic!("file change item: {err}"))
}

async fn observe(
    registry: &codex_extension_api::ExtensionRegistry<HostConfig>,
    thread_store: &ExtensionData,
    turn_store: &ExtensionData,
    mut item: TurnItem,
) {
    for contributor in registry.turn_item_contributors() {
        contributor
            .contribute(thread_store, turn_store, &mut item)
            .await
            .unwrap_or_else(|err| panic!("turn item contribution: {err}"));
    }
}

fn promote_tool_name() -> ToolName {
    ToolName::namespaced(CROST_MEMORY_TOOLS_NAMESPACE, PROMOTE_TO_SHARED_TOOL_NAME)
}

fn tool_call(arguments: serde_json::Value) -> ToolCall<'static> {
    ToolCall {
        turn_id: "turn-1".to_string(),
        call_id: "call-1".to_string(),
        tool_name: promote_tool_name(),
        model: "gpt-test".to_string(),
        codex_turn_metadata: None,
        truncation_policy: TruncationPolicy::Bytes(4096),
        source: ToolCallSource::Direct,
        conversation_history: ConversationHistory::default(),
        turn_item_emitter: Arc::new(NoopTurnItemEmitter),
        environments: Vec::new(),
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    }
}

#[tokio::test]
async fn recall_injects_exactly_one_fragment_per_turn() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (_runtime, fake) = swap_in_fake(&thread_store);
    fake.seed(
        RecallScope::Shared,
        vec![RecallItem {
            id: "s1".to_string(),
            content: "the storefront uses sqlite".to_string(),
            created_at: Some("2026-07-12T10:00:00Z".to_string()),
            source_agent: Some("grok".to_string()),
            score: 0.9,
            ..RecallItem::default()
        }],
    );
    let turn_store = ExtensionData::new("turn-1");

    assert_eq!(registry.turn_input_contributors().len(), 1);
    let fragments = registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Fix the checkout flow"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;

    assert_eq!(fragments.len(), 1);
    let rendered = fragments[0].render();
    assert!(rendered.starts_with(
        "<crost-memory agent=\"codex\" project=\"ohm-storefront\" trust=\"untrusted-historical\">"
    ));
    assert!(rendered.contains("[shared · grok · 2026-07-12] the storefront uses sqlite"));
    assert!(rendered.ends_with("</crost-memory>"));
}

#[tokio::test]
async fn recall_injects_nothing_when_no_memories_exist() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    swap_in_fake(&thread_store);
    let turn_store = ExtensionData::new("turn-1");

    let fragments = registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Fix the checkout flow"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;

    assert!(fragments.is_empty());
}

#[tokio::test]
async fn a_failing_provider_never_errors_the_turn() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    fake.set_auth_failure(true);
    let turn_store = ExtensionData::new("turn-1");

    let fragments = registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Fix the checkout flow"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;

    assert!(fragments.is_empty());
    assert!(
        runtime
            .last_activity()
            .recall
            .is_some_and(|recall| recall.degraded)
    );
}

#[tokio::test]
async fn disabled_config_contributes_no_fragments_and_no_tools() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ false);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let turn_store = ExtensionData::new("turn-1");

    let fragments = registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Fix the checkout flow"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;
    let tools = registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(&session_store, &thread_store))
        .collect::<Vec<_>>();

    assert!(fragments.is_empty());
    assert!(tools.is_empty());
}

#[tokio::test]
async fn a_workspace_without_a_descriptor_stays_inactive() {
    let workspace = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let turn_store = ExtensionData::new("turn-1");

    let fragments = registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Fix the checkout flow"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;
    let tools = registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(&session_store, &thread_store))
        .collect::<Vec<_>>();

    assert!(fragments.is_empty());
    assert!(tools.is_empty());

    let fragments_again = registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Fix the checkout flow again"),
            None,
            &session_store,
            &thread_store,
            &ExtensionData::new("turn-2"),
        )
        .await;
    assert!(fragments_again.is_empty());
    let state = thread_store
        .get::<crate::state::CrostMemoryThreadState>()
        .unwrap_or_else(|| panic!("thread state"));
    assert!(
        matches!(
            state.as_ref(),
            crate::state::CrostMemoryThreadState::Disabled(_)
        ),
        "descriptor-less workspaces must cache Disabled after the first miss, got {state:?}"
    );
}

#[tokio::test]
async fn identity_resolves_lazily_from_the_turn_environment() {
    // The host config points at a directory with no descriptor, but the turn's
    // primary environment is the real workspace.
    let workspace = workspace_with_descriptor();
    let elsewhere = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(elsewhere.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    assert!(matches!(
        thread_store
            .get::<CrostMemoryThreadState>()
            .as_deref()
            .unwrap_or(&CrostMemoryThreadState::Disabled(
                crate::state::DisabledReason::ConfiguredOff
            )),
        CrostMemoryThreadState::PendingIdentity(_)
    ));
    let turn_store = ExtensionData::new("turn-1");

    registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Fix the checkout flow"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;

    let runtime = thread_store
        .get::<CrostMemoryThreadState>()
        .and_then(|state| state.runtime())
        .unwrap_or_else(|| panic!("identity should have resolved from the turn environment"));
    assert_eq!(runtime.identity.slug, "ohm-storefront");
}

#[tokio::test]
async fn retention_builds_a_record_that_excludes_recalled_content() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    fake.seed(
        RecallScope::Private,
        vec![RecallItem {
            id: "p1".to_string(),
            content: "RECALLED-ONLY-MARKER".to_string(),
            score: 0.7,
            ..RecallItem::default()
        }],
    );
    let turn_store = ExtensionData::new("turn-1");

    let fragments = registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Add the outbox\nwith backoff"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;
    assert_eq!(fragments.len(), 1);
    // The model "echoes" the injected block back; it must never be retained.
    observe(
        &registry,
        &thread_store,
        &turn_store,
        agent_message(&fragments[0].render()),
    )
    .await;
    observe(
        &registry,
        &thread_store,
        &turn_store,
        agent_message("Done.\nNext step: wire diagnostics"),
    )
    .await;
    observe(
        &registry,
        &thread_store,
        &turn_store,
        command_execution(&["cargo", "test", "-p", "codex-crost-memory-extension"], 0),
    )
    .await;
    observe(
        &registry,
        &thread_store,
        &turn_store,
        file_change("/tmp/crost-memory-test/src/outbox.rs"),
    )
    .await;

    for contributor in registry.turn_lifecycle_contributors() {
        contributor
            .on_turn_stop(TurnStopInput {
                session_store: &session_store,
                thread_store: &thread_store,
                turn_store: &turn_store,
            })
            .await;
    }
    flush_now(&runtime).await;

    let retained = fake.retained();
    assert_eq!(retained.len(), 1);
    let record = &retained[0].record;
    assert_eq!(record.objective.as_deref(), Some("Add the outbox"));
    assert_eq!(
        record.files_changed,
        vec!["/tmp/crost-memory-test/src/outbox.rs".to_string()]
    );
    assert_eq!(record.tests.len(), 1);
    assert_eq!(record.tests[0].result, "passed");
    assert_eq!(record.next_step.as_deref(), Some("wire diagnostics"));
    let rendered = record.render();
    assert!(!rendered.contains("RECALLED-ONLY-MARKER"));
    assert!(!rendered.contains("<crost-memory"));
    assert_eq!(retained[0].op_id.len(), 36);
}

#[tokio::test]
async fn secrets_are_redacted_before_retention() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    let turn_store = ExtensionData::new("turn-1");

    registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Rotate AKIAIOSFODNN7EXAMPLE"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;
    observe(
        &registry,
        &thread_store,
        &turn_store,
        file_change("/tmp/crost-memory-test/src/creds.rs"),
    )
    .await;
    for contributor in registry.turn_lifecycle_contributors() {
        contributor
            .on_turn_stop(TurnStopInput {
                session_store: &session_store,
                thread_store: &thread_store,
                turn_store: &turn_store,
            })
            .await;
    }
    flush_now(&runtime).await;

    let retained = fake.retained();
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].record.objective.as_deref(),
        Some("Rotate [REDACTED:aws_access_key_id]")
    );
}

#[tokio::test]
async fn aborted_turns_retain_nothing() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    let turn_store = ExtensionData::new("turn-1");

    registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Add the outbox"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;
    observe(
        &registry,
        &thread_store,
        &turn_store,
        file_change("/tmp/crost-memory-test/src/outbox.rs"),
    )
    .await;
    for contributor in registry.turn_lifecycle_contributors() {
        contributor
            .on_turn_abort(TurnAbortInput {
                reason: TurnAbortReason::Interrupted,
                session_store: &session_store,
                thread_store: &thread_store,
                turn_store: &turn_store,
            })
            .await;
        contributor
            .on_turn_stop(TurnStopInput {
                session_store: &session_store,
                thread_store: &thread_store,
                turn_store: &turn_store,
            })
            .await;
    }
    flush_now(&runtime).await;

    assert!(fake.retained().is_empty());
    assert_eq!(runtime.outbox.depth(), 0);
    let turn_state = turn_store
        .get::<CrostMemoryTurnState>()
        .unwrap_or_else(|| panic!("turn state should exist"));
    assert!(turn_state.is_discarded());
}

#[tokio::test]
async fn trivial_turns_are_not_retained() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    let turn_store = ExtensionData::new("turn-1");
    // No recall pass, so no objective is stored and nothing else was observed.
    turn_store.get_or_init(CrostMemoryTurnState::default);

    for contributor in registry.turn_lifecycle_contributors() {
        contributor
            .on_turn_stop(TurnStopInput {
                session_store: &session_store,
                thread_store: &thread_store,
                turn_store: &turn_store,
            })
            .await;
    }
    flush_now(&runtime).await;

    assert!(fake.retained().is_empty());
    assert_eq!(
        runtime.last_activity().retention.as_deref(),
        Some("skipped: nothing meaningful to retain")
    );
}

#[tokio::test]
async fn the_promotion_tool_round_trips_into_the_shared_bank() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);

    let tools = registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(&session_store, &thread_store))
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_name(), promote_tool_name());

    let payload = json!({
        "kind": "decision",
        "title": "Store project memory in Hindsight",
        "summary": "One central Hindsight deployment backs both forks.",
        "decisions": ["Banks are derived only inside the provider"],
        "files": ["codex-rs/ext/crost-memory/src/hindsight.rs"],
        "evidence": {
            "commit": "abc1234",
            "test_cmd": "cargo test -p codex-crost-memory-extension",
            "test_result": "passed"
        },
        "task_id": "T-42"
    });
    let call = tool_call(payload.clone());
    let output = tools[0]
        .handle(call)
        .await
        .unwrap_or_else(|err| panic!("promotion should be accepted: {err}"));
    flush_now(&runtime).await;

    let response = output
        .post_tool_use_response(
            "call-1",
            &ToolPayload::Function {
                arguments: payload.to_string(),
            },
        )
        .unwrap_or_else(|| panic!("promotion should return a JSON ack"));
    assert_eq!(response.get("accepted"), Some(&json!(true)));
    assert_eq!(response.get("kind"), Some(&json!("decision")));
    assert_eq!(
        response.get("title"),
        Some(&json!("Store project memory in Hindsight"))
    );

    let promoted = fake.promoted();
    assert_eq!(promoted.len(), 1);
    assert_eq!(promoted[0].kind, PromoteKind::Decision);
    assert_eq!(
        promoted[0].op_id,
        response
            .get("op_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    );
    assert!(
        promoted[0]
            .record
            .render(PromoteKind::Decision)
            .contains("T-42")
    );
    assert!(fake.retained().is_empty());
}

#[tokio::test]
async fn promotions_of_decisions_require_evidence() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    swap_in_fake(&thread_store);
    let tools = registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(&session_store, &thread_store))
        .collect::<Vec<_>>();

    let err = tools[0]
        .handle(tool_call(json!({
            "kind": "decision",
            "title": "No evidence",
            "summary": "Nothing to back this up."
        })))
        .await
        .err()
        .unwrap_or_else(|| panic!("a decision without evidence must be rejected"));

    assert!(err.to_string().contains("evidence"));
}

#[tokio::test]
async fn handoffs_do_not_require_evidence() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    let tools = registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(&session_store, &thread_store))
        .collect::<Vec<_>>();

    tools[0]
        .handle(tool_call(json!({
            "kind": "handoff",
            "title": "Grok picks up the CLI wiring",
            "summary": "The extension crate is done; core plumbing is next.",
            "next_owner": "grok",
            "next_action": "Add the config section and registration line."
        })))
        .await
        .unwrap_or_else(|err| panic!("handoff should be accepted: {err}"));
    flush_now(&runtime).await;

    assert_eq!(fake.promoted().len(), 1);
    assert_eq!(fake.promoted()[0].kind, PromoteKind::Handoff);
}

#[tokio::test]
async fn promotion_secrets_are_redacted() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    let tools = registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(&session_store, &thread_store))
        .collect::<Vec<_>>();

    tools[0]
        .handle(tool_call(json!({
            "kind": "blocker",
            "title": "Hindsight rejects our key",
            "summary": "Server returns 401 for api_key = 'hunter2hunter2'."
        })))
        .await
        .unwrap_or_else(|err| panic!("blocker should be accepted: {err}"));
    flush_now(&runtime).await;

    let promoted = fake.promoted();
    assert_eq!(promoted.len(), 1);
    assert!(promoted[0].record.summary.contains("[REDACTED:secret]"));
    assert!(!promoted[0].record.summary.contains("hunter2"));
}

#[tokio::test]
async fn the_promotion_tool_is_gated_by_configuration() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let mut config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    config.extension.memory.shared_promotion_enabled = false;
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;

    let tools = registry
        .tool_contributors()
        .iter()
        .flat_map(|contributor| contributor.tools(&session_store, &thread_store))
        .collect::<Vec<_>>();

    assert!(tools.is_empty());
}

#[tokio::test]
async fn retention_can_be_disabled_independently() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let mut config = host_config(workspace.path(), home.path(), /*enabled*/ true);
    config.extension.memory.retain_enabled = false;
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &config, &session_store, &thread_store).await;
    let (runtime, fake) = swap_in_fake(&thread_store);
    let turn_store = ExtensionData::new("turn-1");

    registry.turn_input_contributors()[0]
        .contribute(
            turn_input(workspace.path(), "Add the outbox"),
            None,
            &session_store,
            &thread_store,
            &turn_store,
        )
        .await;
    for contributor in registry.turn_lifecycle_contributors() {
        contributor
            .on_turn_stop(TurnStopInput {
                session_store: &session_store,
                thread_store: &thread_store,
                turn_store: &turn_store,
            })
            .await;
    }
    flush_now(&runtime).await;

    assert!(fake.retained().is_empty());
}

#[tokio::test]
async fn config_changes_reseed_the_thread_state() {
    let workspace = workspace_with_descriptor();
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let registry = registry();
    let disabled = host_config(workspace.path(), home.path(), /*enabled*/ false);
    let enabled = host_config(workspace.path(), home.path(), /*enabled*/ true);
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    start_thread(&registry, &disabled, &session_store, &thread_store).await;
    assert!(
        thread_store
            .get::<CrostMemoryThreadState>()
            .and_then(|state| state.runtime())
            .is_none()
    );

    for contributor in registry.config_contributors() {
        contributor.on_config_changed(&session_store, &thread_store, &disabled, &enabled);
    }

    assert!(
        thread_store
            .get::<CrostMemoryThreadState>()
            .and_then(|state| state.runtime())
            .is_some()
    );
}

#[tokio::test]
async fn queued_operations_survive_an_offline_endpoint() {
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let identity = crate::identity::ProjectIdentity {
        project_id: "p-offline".to_string(),
        slug: "ohm".to_string(),
        bank_prefix: None,
    };
    let fake = FakeProvider::new();
    fake.fail_retain(crate::provider::MemoryError::Unavailable(
        "offline".to_string(),
    ));
    let runtime = Arc::new(CrostMemoryRuntime::new(
        CrostMemoryConfig {
            enabled: true,
            provider: ProviderKind::Fake,
            ..CrostMemoryConfig::default()
        },
        identity.clone(),
        Arc::new(fake.clone()) as Arc<dyn MemoryProvider>,
        Arc::new(Outbox::new(home.path(), &identity.project_id)),
    ));
    runtime
        .outbox
        .enqueue(crate::outbox::OutboxOp::Retain(crate::types::RetainOp {
            op_id: "cm-offline".to_string(),
            record: crate::types::TurnRecord {
                objective: Some("survive an outage".to_string()),
                ..crate::types::TurnRecord::default()
            },
        }))
        .unwrap_or_else(|err| panic!("enqueue: {err}"));

    flush_now(&runtime).await;

    assert!(fake.retained().is_empty());
    assert_eq!(runtime.outbox.depth(), 1);
    assert_eq!(
        runtime.outbox.dir(),
        PathBuf::from(home.path()).join("p-offline")
    );
}
