//! Crost project memory client for Codex.
//!
//! Implements the Crost Project Memory contract (v1): pre-turn recall of
//! private and shared banks, one delimited untrusted-historical injection per
//! genuine prompt, automatic private retention through a bounded persistent
//! outbox, and explicit shared promotion through a native tool.
//!
//! Everything above [`provider::MemoryProvider`] is provider-agnostic. Only
//! [`hindsight`] knows the server API or bank naming.

pub mod capture;
pub mod config;
pub mod diag;
mod extension;
pub mod fake;
pub mod flush;
pub mod fragment;
pub mod hindsight;
pub mod identity;
pub mod outbox;
pub mod provider;
pub mod recall;
pub mod redact;
mod schema;
pub mod state;
mod tools;
pub mod types;

pub use config::CrostMemoryConfig;
pub use extension::CrostMemoryExtension;
pub use extension::build_thread_state;
pub use extension::install;
pub use state::CrostMemoryExtensionConfig;

/// Namespace of every tool this extension owns.
pub const CROST_MEMORY_TOOLS_NAMESPACE: &str = "crost_memory";

/// Name of the shared-promotion tool inside the namespace.
pub const PROMOTE_TO_SHARED_TOOL_NAME: &str = "promote_to_shared";

#[cfg(test)]
mod tests;
