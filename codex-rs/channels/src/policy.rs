use crate::spec::ChannelSpec;

/// Effective `[channels]` policy after config layering has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsPolicy {
    /// Master switch. When `false`, no channel may deliver events.
    pub enabled: bool,
    /// Optional allowlist of canonical entry strings (e.g. `server:discord`).
    /// `None` allows every requested entry; an empty list allows none.
    pub allowed: Option<Vec<String>>,
}

impl Default for ChannelsPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed: None,
        }
    }
}

impl ChannelsPolicy {
    /// Whether the allowlist (if any) permits this spec.
    pub fn entry_allowed(&self, spec: &ChannelSpec) -> bool {
        match &self.allowed {
            None => true,
            Some(allowed) => {
                let canonical = spec.canonical();
                allowed.iter().any(|entry| {
                    let entry = entry.trim();
                    entry == canonical
                        || ChannelSpec::parse(entry).is_ok_and(|allowed| allowed == *spec)
                })
            }
        }
    }
}

/// Resolves the effective `[channels] enabled` value.
///
/// Precedence, highest first: managed config, the `CODEX_CHANNELS_ENABLED`
/// environment variable, then user config. Unset (or unparseable env) values
/// fall through; the default is enabled.
pub fn resolve_channels_enabled(
    managed: Option<bool>,
    env_value: Option<&str>,
    user: Option<bool>,
) -> bool {
    managed
        .or_else(|| env_value.and_then(parse_env_bool))
        .or(user)
        .unwrap_or(true)
}

fn parse_env_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
