//! Native tools owned by the Crost memory extension.

use std::sync::Arc;

use codex_extension_api::FunctionCallError;
use codex_extension_api::ResponsesApiTool;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolSpec;
use codex_extension_api::parse_tool_input_schema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::default_namespace_description;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::CROST_MEMORY_TOOLS_NAMESPACE;
use crate::schema;
use crate::state::CrostMemoryRuntime;

mod promote;

pub(crate) use promote::PromoteToSharedTool;

/// Tools contributed when memory is enabled and promotion is allowed.
pub(crate) fn crost_memory_tools(
    runtime: Arc<CrostMemoryRuntime>,
) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
    vec![Arc::new(PromoteToSharedTool::new(runtime))]
}

pub(super) fn crost_memory_tool_name(name: &str) -> ToolName {
    ToolName::namespaced(CROST_MEMORY_TOOLS_NAMESPACE, name)
}

pub(super) fn crost_memory_function_tool<I: JsonSchema, O: JsonSchema>(
    name: &str,
    description: &str,
) -> ToolSpec {
    let tool = ResponsesApiTool {
        name: name.to_string(),
        description: description.to_string(),
        strict: false,
        defer_loading: None,
        parameters: parse_tool_input_schema(&schema::input_schema_for::<I>())
            .unwrap_or_else(|err| panic!("generated input schema for {name} should parse: {err}")),
        output_schema: Some(schema::output_schema_for::<O>()),
    };

    ToolSpec::Namespace(ResponsesApiNamespace {
        name: CROST_MEMORY_TOOLS_NAMESPACE.to_string(),
        description: default_namespace_description(CROST_MEMORY_TOOLS_NAMESPACE),
        tools: vec![ResponsesApiNamespaceTool::Function(tool)],
    })
}

pub(super) fn parse_args<T: for<'de> Deserialize<'de>>(
    call: &ToolCall<'_>,
) -> Result<T, FunctionCallError> {
    let arguments = call.function_arguments()?;
    let value = if arguments.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(arguments)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
    };
    serde_json::from_value(value).map_err(|err| FunctionCallError::RespondToModel(err.to_string()))
}
