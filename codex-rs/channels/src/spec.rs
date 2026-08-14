use thiserror::Error;

/// A parsed `--channels` entry describing which MCP server(s) may push events
/// into the session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChannelSpec {
    /// `server:<name>`: a single MCP server from `[mcp_servers]`.
    Server(String),
    /// `plugin:<id>`: every MCP server contributed by the named plugin.
    Plugin(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChannelSpecError {
    #[error("channel entry is empty")]
    Empty,
    #[error("channel entry `{entry}` is missing a `server:` or `plugin:` prefix")]
    MissingKind { entry: String },
    #[error("unknown channel kind `{kind}` (expected `server` or `plugin`)")]
    UnknownKind { kind: String },
    #[error("channel {kind} name `{name}` contains invalid characters")]
    InvalidName { kind: &'static str, name: String },
}

impl ChannelSpec {
    /// Parses a single channel entry of the form `server:<name>` or
    /// `plugin:<id>`. Surrounding whitespace is ignored.
    pub fn parse(entry: &str) -> Result<Self, ChannelSpecError> {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(ChannelSpecError::Empty);
        }
        let Some((kind, name)) = entry.split_once(':') else {
            return Err(ChannelSpecError::MissingKind {
                entry: entry.to_string(),
            });
        };
        let name = name.trim();
        match kind.trim() {
            "server" => {
                if is_valid_server_name(name) {
                    Ok(Self::Server(name.to_string()))
                } else {
                    Err(ChannelSpecError::InvalidName {
                        kind: "server",
                        name: name.to_string(),
                    })
                }
            }
            "plugin" => {
                if is_valid_plugin_id(name) {
                    Ok(Self::Plugin(name.to_string()))
                } else {
                    Err(ChannelSpecError::InvalidName {
                        kind: "plugin",
                        name: name.to_string(),
                    })
                }
            }
            other => Err(ChannelSpecError::UnknownKind {
                kind: other.to_string(),
            }),
        }
    }

    /// The canonical entry string for this spec, used for allowlist matching
    /// and display.
    pub fn canonical(&self) -> String {
        match self {
            Self::Server(name) => format!("server:{name}"),
            Self::Plugin(id) => format!("plugin:{id}"),
        }
    }
}

/// Splits raw `--channels` values on commas, trimming whitespace and dropping
/// empty segments, so both repeated flags and comma-separated lists work.
pub fn split_channel_entries<S: AsRef<str>>(raw: &[S]) -> Vec<String> {
    raw.iter()
        .flat_map(|value| value.as_ref().split(','))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// Mirrors the MCP server name validation used when connecting servers.
fn is_valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
