//! The Crost memory extension and its contributor roles.
//!
//! The crate stays generic over the host config type `C`: the host supplies a
//! closure that maps its own config into [`CrostMemoryExtensionConfig`], so this
//! crate never depends on codex-core.

use std::path::Path;
use std::sync::Arc;

use codex_extension_api::ConfigContributor;
use codex_extension_api::ContextualUserFragment;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;
use codex_extension_api::TurnAbortInput;
use codex_extension_api::TurnErrorInput;
use codex_extension_api::TurnInputContext;
use codex_extension_api::TurnInputContributor;
use codex_extension_api::TurnItemContributor;
use codex_extension_api::TurnLifecycleContributor;
use codex_extension_api::TurnStopInput;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::TurnItem;
use codex_protocol::user_input::UserInput;

use crate::capture::CapturedCommand;
use crate::capture::first_line;
use crate::capture::text_hash;
use crate::flush::spawn_flush;
use crate::fragment::CrostMemoryFragment;
use crate::identity::resolve_project_identity_at;
use crate::outbox::Outbox;
use crate::outbox::OutboxOp;
use crate::provider::build_provider;
use crate::recall::run_recall;
use crate::redact::redact_turn_record;
use crate::state::CrostMemoryExtensionConfig;
use crate::state::CrostMemoryRuntime;
use crate::state::CrostMemoryThreadState;
use crate::state::CrostMemoryTurnState;
use crate::state::DisabledReason;
use crate::tools::crost_memory_tools;
use crate::types::RetainOp;
use crate::types::new_op_id;

/// Longest objective stored on a turn record.
const MAX_OBJECTIVE_CHARS: usize = 300;

/// Contributes pre-turn recall, turn observation, automatic private retention,
/// and the shared-promotion tool.
pub struct CrostMemoryExtension<C> {
    config_from_host: Arc<dyn Fn(&C) -> CrostMemoryExtensionConfig + Send + Sync>,
}

impl<C> CrostMemoryExtension<C> {
    fn resolve_thread_state(&self, host_config: &C) -> CrostMemoryThreadState {
        let config = (self.config_from_host)(host_config);
        build_thread_state(config)
    }
}

/// Builds thread state from an already-resolved extension config.
pub fn build_thread_state(config: CrostMemoryExtensionConfig) -> CrostMemoryThreadState {
    let memory = config.memory.clone().with_env_overrides();
    if !memory.enabled {
        return CrostMemoryThreadState::Disabled(DisabledReason::ConfiguredOff);
    }
    match activate(&memory, &config.cwd, &config.outbox_root) {
        Ok(state) => state,
        Err(DisabledReason::NoProjectIdentity(_)) => {
            // Identity may still resolve from the turn's primary environment.
            CrostMemoryThreadState::PendingIdentity(Box::new(CrostMemoryExtensionConfig {
                memory,
                ..config
            }))
        }
        Err(reason) => CrostMemoryThreadState::Disabled(reason),
    }
}

fn activate(
    memory: &crate::config::CrostMemoryConfig,
    cwd: &Path,
    outbox_root: &Path,
) -> Result<CrostMemoryThreadState, DisabledReason> {
    let identity = resolve_project_identity_at(cwd, &memory.project_file)
        .map_err(|err| DisabledReason::NoProjectIdentity(err.to_string()))?;
    let provider = build_provider(memory, &identity)
        .map_err(|err| DisabledReason::ProviderUnavailable(err.to_string()))?;
    let outbox = Arc::new(Outbox::new(outbox_root, &identity.project_id));
    Ok(CrostMemoryThreadState::Enabled(Arc::new(
        CrostMemoryRuntime::new(memory.clone(), identity, provider, outbox),
    )))
}

/// Reads thread state, upgrading a pending identity from the turn's primary
/// environment cwd when possible.
fn runtime_for_turn(
    thread_store: &ExtensionData,
    input: &TurnInputContext,
) -> Option<Arc<CrostMemoryRuntime>> {
    let state = thread_store.get::<CrostMemoryThreadState>()?;
    if let Some(runtime) = state.runtime() {
        return Some(runtime);
    }
    let CrostMemoryThreadState::PendingIdentity(config) = state.as_ref() else {
        return None;
    };
    let cwd = input
        .environments
        .iter()
        .find(|environment| environment.is_primary)
        .or_else(|| input.environments.first())
        .map(|environment| environment.cwd.clone())?;
    match activate(&config.memory, cwd.to_path_buf().as_path(), &config.outbox_root) {
        Ok(next) => {
            let runtime = next.runtime();
            thread_store.insert(next);
            runtime
        }
        Err(DisabledReason::NoProjectIdentity(detail)) => {
            tracing::debug!(detail, "crost memory stays inactive: no project identity");
            thread_store.insert(CrostMemoryThreadState::Disabled(
                DisabledReason::NoProjectIdentity(detail),
            ));
            None
        }
        Err(reason) => {
            tracing::warn!(reason = %reason, "crost memory disabled for this thread");
            thread_store.insert(CrostMemoryThreadState::Disabled(reason));
            None
        }
    }
}

impl<C> ThreadLifecycleContributor<C> for CrostMemoryExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_thread_start<'a>(&'a self, input: ThreadStartInput<'a, C>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            input
                .thread_store
                .insert(self.resolve_thread_state(input.config));
        })
    }
}

impl<C> ConfigContributor<C> for CrostMemoryExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_config_changed(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
        _previous_config: &C,
        new_config: &C,
    ) {
        thread_store.insert(self.resolve_thread_state(new_config));
    }
}

impl<C> TurnInputContributor for CrostMemoryExtension<C>
where
    C: Send + Sync + 'static,
{
    fn contribute<'a>(
        &'a self,
        input: TurnInputContext<'a>,
        _extension_metrics: Option<std::sync::Arc<dyn codex_extension_api::ExtensionMetrics>>,
        _session_store: &'a ExtensionData,
        thread_store: &'a ExtensionData,
        turn_store: &'a ExtensionData,
    ) -> ExtensionFuture<'a, Vec<Box<dyn ContextualUserFragment + Send>>> {
        Box::pin(async move {
            let query = user_input_text(&input.user_input);
            let turn_state = turn_store.get_or_init(CrostMemoryTurnState::default);
            turn_state.with_capture(|capture| {
                capture.turn_id = Some(input.turn_id.clone());
                capture.objective = first_line(&query, MAX_OBJECTIVE_CHARS);
            });

            let Some(runtime) = runtime_for_turn(thread_store, &input) else {
                return Vec::new();
            };
            if query.trim().is_empty() {
                return Vec::new();
            }

            let outcome = run_recall(
                &runtime.provider,
                &runtime.config,
                &runtime.identity,
                &query,
            )
            .await;
            runtime.record_recall(&outcome);
            turn_state.with_capture(|capture| {
                capture.recall_private_n = outcome.private_n;
                capture.recall_shared_n = outcome.shared_n;
                capture.injected_block_hash = outcome.block.as_deref().map(text_hash);
            });
            tracing::debug!(
                private_n = outcome.private_n,
                shared_n = outcome.shared_n,
                injected_tokens = outcome.injected_tokens,
                latency_ms = outcome.latency_ms,
                degraded = outcome.degraded,
                "crost memory recall completed"
            );

            let Some(block) = outcome.block else {
                return Vec::new();
            };
            vec![Box::new(CrostMemoryFragment::from_block(&block))
                as Box<dyn ContextualUserFragment + Send>]
        })
    }
}

impl<C> TurnItemContributor for CrostMemoryExtension<C>
where
    C: Send + Sync + 'static,
{
    fn contribute<'a>(
        &'a self,
        thread_store: &'a ExtensionData,
        turn_store: &'a ExtensionData,
        item: &'a mut TurnItem,
    ) -> ExtensionFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let enabled = thread_store
                .get::<CrostMemoryThreadState>()
                .is_some_and(|state| state.runtime().is_some());
            if !enabled {
                return Ok(());
            }
            let Some(turn_state) = turn_store.get::<CrostMemoryTurnState>() else {
                return Ok(());
            };
            if turn_state.is_discarded() {
                return Ok(());
            }

            // Items are observed, never mutated.
            match &*item {
                TurnItem::AgentMessage(message) => {
                    let text = message
                        .content
                        .iter()
                        .map(|content| match content {
                            AgentMessageContent::Text { text } => text.as_str(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    turn_state.with_capture(|capture| capture.push_agent_message(text));
                }
                TurnItem::CommandExecution(command) => {
                    let captured = CapturedCommand {
                        command: command.command.join(" "),
                        status: format!("{:?}", command.status).to_ascii_lowercase(),
                        exit_code: command.exit_code,
                    };
                    turn_state.with_capture(|capture| capture.push_command(captured));
                }
                TurnItem::FileChange(change) => {
                    let mut paths = change
                        .changes
                        .keys()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    paths.sort();
                    turn_state.with_capture(|capture| {
                        for path in paths {
                            capture.push_file_changed(path);
                        }
                    });
                }
                _ => {}
            }
            Ok(())
        })
    }
}

impl<C> TurnLifecycleContributor for CrostMemoryExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_turn_stop<'a>(&'a self, input: TurnStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = input
                .thread_store
                .get::<CrostMemoryThreadState>()
                .and_then(|state| state.runtime())
            else {
                return;
            };
            if !runtime.config.retain_enabled {
                return;
            }
            let Some(turn_state) = input.turn_store.get::<CrostMemoryTurnState>() else {
                return;
            };
            if turn_state.is_discarded() {
                return;
            }

            let record = redact_turn_record(&turn_state.snapshot().to_turn_record());
            if !record.is_meaningful() {
                runtime.record_retention("skipped: nothing meaningful to retain");
                return;
            }
            let op = RetainOp {
                op_id: new_op_id(),
                record,
            };
            match runtime.outbox.enqueue(OutboxOp::Retain(op)) {
                Ok(_) => {
                    runtime.record_retention("queued 1 turn summary");
                    spawn_flush(Arc::clone(&runtime));
                }
                Err(err) => {
                    runtime.record_retention(format!("failed to queue: {err}"));
                    tracing::warn!(error = %err, "crost memory could not queue a turn summary");
                }
            }
        })
    }

    fn on_turn_abort<'a>(&'a self, input: TurnAbortInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            discard_turn(input.turn_store);
        })
    }

    fn on_turn_error<'a>(&'a self, input: TurnErrorInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            discard_turn(input.turn_store);
        })
    }
}

impl<C> ToolContributor for CrostMemoryExtension<C>
where
    C: Send + Sync + 'static,
{
    fn tools(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
        let Some(runtime) = thread_store
            .get::<CrostMemoryThreadState>()
            .and_then(|state| state.runtime())
        else {
            return Vec::new();
        };
        if !runtime.config.shared_promotion_enabled {
            return Vec::new();
        }
        crost_memory_tools(runtime)
    }
}

fn discard_turn(turn_store: &ExtensionData) {
    if let Some(turn_state) = turn_store.get::<CrostMemoryTurnState>() {
        turn_state.discard();
    }
}

fn user_input_text(user_input: &[UserInput]) -> String {
    user_input
        .iter()
        .filter_map(|input| match input {
            UserInput::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Installs every Crost memory contributor from one shared extension value.
pub fn install<C>(
    registry: &mut ExtensionRegistryBuilder<C>,
    config_from_host: impl Fn(&C) -> CrostMemoryExtensionConfig + Send + Sync + 'static,
) where
    C: Send + Sync + 'static,
{
    let extension = Arc::new(CrostMemoryExtension {
        config_from_host: Arc::new(config_from_host),
    });
    registry.thread_lifecycle_contributor(extension.clone());
    registry.config_contributor(extension.clone());
    registry.turn_input_contributor(extension.clone());
    registry.turn_item_contributor(extension.clone());
    registry.turn_lifecycle_contributor(extension.clone());
    registry.tool_contributor(extension);
}
