//! The single contextual fragment this extension injects.

use codex_extension_api::ContentItemKind;
use codex_extension_api::ContextualUserFragment;

use crate::recall::CROST_MEMORY_CLOSE_TAG;
use crate::recall::CROST_MEMORY_OPEN_TAG;

/// One rendered `<crost-memory>` block presented to the model as untrusted
/// historical context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrostMemoryFragment {
    body: String,
    marked: bool,
}

impl CrostMemoryFragment {
    /// Wraps an already-rendered block.
    ///
    /// The block's own delimiters become the fragment markers so host-side
    /// context filtering can recognize injected memory, and `render()` returns
    /// the block byte-for-byte.
    pub fn from_block(block: &str) -> Self {
        if let Some(rest) = block.strip_prefix(CROST_MEMORY_OPEN_TAG)
            && let Some(inner) = rest.strip_suffix(CROST_MEMORY_CLOSE_TAG)
        {
            return Self {
                body: inner.to_string(),
                marked: true,
            };
        }
        Self {
            body: block.to_string(),
            marked: false,
        }
    }
}

impl ContextualUserFragment for CrostMemoryFragment {
    fn role(&self) -> &'static str {
        "user"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("crost_memory.recall".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        if self.marked {
            Self::type_markers()
        } else {
            ("", "")
        }
    }

    fn type_markers() -> (&'static str, &'static str) {
        (CROST_MEMORY_OPEN_TAG, CROST_MEMORY_CLOSE_TAG)
    }

    fn body(&self) -> String {
        self.body.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::render_block;
    use pretty_assertions::assert_eq;

    #[test]
    fn render_round_trips_the_block_exactly() {
        let block = render_block(
            "codex",
            "ohm",
            &["[shared · grok · 2026-07-12] a".to_string()],
            &["[private · 2026-07-19] b".to_string()],
        );

        let fragment = CrostMemoryFragment::from_block(&block);

        assert_eq!(fragment.render(), block);
        assert_eq!(fragment.role(), "user");
        assert!(CrostMemoryFragment::matches_text(&fragment.render()));
    }

    #[test]
    fn unexpected_input_still_renders_verbatim() {
        let fragment = CrostMemoryFragment::from_block("plain text");

        assert_eq!(fragment.render(), "plain text");
        assert_eq!(fragment.markers(), ("", ""));
    }

    #[test]
    fn arbitrary_text_does_not_match_the_fragment_type() {
        assert!(!CrostMemoryFragment::matches_text(
            "a normal user message about crost memory"
        ));
    }
}
