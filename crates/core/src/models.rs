//! Capability types for embedding clients.
//!
//! - [`EmbedCaps`] is live: `dim` must match the vector column;
//!   `matryoshka` / `max_input_chars` are host-injected client flags.

use std::num::NonZeroU32;

/// Embedding capability axes. `dim` is the vector size — boot-time
/// mismatch against the storage migration's vector column is fatal.
/// `matryoshka` indicates whether the model produces nested-prefix
/// embeddings (caller may truncate without re-embedding).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct EmbedCaps {
    pub dim: u32,
    pub matryoshka: bool,
    /// Longest input, in characters, the client will *send*. `None` sends
    /// every input and lets the provider judge it.
    ///
    /// Bound applied before the request: a provider may not enforce its own
    /// limit (a local runner can die instead of rejecting). Characters, not
    /// tokens (no shared tokenizer) and not bytes (would refuse non-ASCII
    /// shorter text; see [`crate::text_bounds`]). Over-cap is
    /// [`crate::llm::LlmError::EmbedPermanent`] → [`crate::llm::embed_in_chunks`];
    /// floor is [`crate::llm::MIN_EMBED_INPUT_CAP_CHARS`].
    pub max_input_chars: Option<NonZeroU32>,
}

impl EmbedCaps {
    /// Caps for a model of `dim` dimensions, sending every input.
    ///
    /// Prefer this over a struct literal: it is the construction path that
    /// survives a new capability axis being added.
    #[must_use]
    pub const fn new(dim: u32, matryoshka: bool) -> Self {
        Self {
            dim,
            matryoshka,
            max_input_chars: None,
        }
    }

    /// Refuse, without sending, any input longer than `chars` characters.
    ///
    /// See [`EmbedCaps::max_input_chars`] and
    /// [`crate::llm::MIN_EMBED_INPUT_CAP_CHARS`].
    #[must_use]
    pub const fn with_max_input_chars(mut self, chars: NonZeroU32) -> Self {
        self.max_input_chars = Some(chars);
        self
    }
}
