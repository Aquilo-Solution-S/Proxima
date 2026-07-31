//! Build-time capability vocabulary for LLM + embedding clients.
//!
//! Build-time declares **what** capabilities exist and **which** an
//! operator requires. It does **not** declare specific
//! `(vendor, model_id)` pairs — those are host configuration, not
//! flavor authorship. New models plug in by declaring their caps;
//! validation gates mismatches against the operator-declared
//! `requires`.
//!
//! The contract:
//!
//! - `LlmCaps` / `EmbedCaps` enumerate the capability axes a
//!   substrate operator can demand.
//! - `Dialect` enumerates the HTTP API shapes a runtime client
//!   speaks; `vendor` (e.g. `"ollama"`, `"openai"`) lives only at
//!   runtime config.
//!
//! See docs/10 §Capability vocabulary.

use std::num::NonZeroU32;

/// Which HTTP API shape a runtime model client speaks. Independent
/// of vendor: most non-Anthropic vendors expose the `OpenAI` dialect,
/// so a runtime entry like
/// `{vendor: "openrouter", dialect: OpenAI, model_id: "anthropic/..."}`
/// is normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    Anthropic,
    #[serde(rename = "openai")]
    OpenAI,
}

/// LLM capability axes. Operators declare a `requires: LlmCaps` at
/// registration; hosts validate that the selected model's claimed caps
/// satisfy the union of operator `requires`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "orthogonal LLM capability flags"
)]
pub struct LlmCaps {
    pub tool_use: bool,
    pub json_mode: bool,
    pub long_context: bool,
    pub vision: bool,
}

impl LlmCaps {
    /// All-false. Useful with functional update —
    /// `LlmCaps { json_mode: true, ..LlmCaps::none() }`.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            tool_use: false,
            json_mode: false,
            long_context: false,
            vision: false,
        }
    }

    /// Does `self` satisfy `required`? — every cap that `required`
    /// demands must be present in `self`. Used at runtime when a
    /// model is bound to a tier; the model's claimed caps must
    /// satisfy the union of `requires` over operators using that
    /// tier.
    #[must_use]
    pub const fn satisfies(&self, required: &Self) -> bool {
        (!required.tool_use || self.tool_use)
            && (!required.json_mode || self.json_mode)
            && (!required.long_context || self.long_context)
            && (!required.vision || self.vision)
    }
}

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
    /// This exists because a provider's limit is not always enforced by the
    /// provider. A local Ollama sizes a model runner's context when the
    /// runner loads; an input past that killed the runner with SIGTRAP
    /// rather than being rejected, and the death surfaced as a transport
    /// error — indistinguishable, at the response, from a runner that was
    /// already down. Retried unchanged, one input took the embedder down
    /// 329 times in ten hours. A bound applied *before* the request removes
    /// the class: a limit is never discovered by killing a process.
    ///
    /// Characters, not tokens: no tokenizer is provider-independent, so
    /// this is a deliberately conservative proxy that any deployment can
    /// set from its own model's context. Characters, not bytes, for the
    /// same reason as [`crate::text_bounds`] — a byte cap would refuse a
    /// shorter text merely for not being written in ASCII.
    ///
    /// Over-cap input is refused as [`crate::llm::LlmError::EmbedPermanent`],
    /// which is what routes it into [`crate::llm::embed_in_chunks`]. That
    /// couples the two constants — see
    /// [`crate::llm::MIN_EMBED_INPUT_CAP_CHARS`], which is the floor a cap
    /// must clear for the rescue to be able to succeed at all.
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
    /// See [`EmbedCaps::max_input_chars`] for why a client-side bound is
    /// worth having, and [`crate::llm::MIN_EMBED_INPUT_CAP_CHARS`] for how
    /// low it can usefully go.
    #[must_use]
    pub const fn with_max_input_chars(mut self, chars: NonZeroU32) -> Self {
        self.max_input_chars = Some(chars);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_serde_lowercase() {
        let s = serde_json::to_string(&Dialect::OpenAI).unwrap();
        assert_eq!(s, "\"openai\"");
        let back: Dialect = serde_json::from_str(&s).unwrap();
        assert_eq!(back, Dialect::OpenAI);

        let s = serde_json::to_string(&Dialect::Anthropic).unwrap();
        assert_eq!(s, "\"anthropic\"");
    }

    #[test]
    fn caps_satisfies_when_all_required_present() {
        let have = LlmCaps {
            tool_use: true,
            json_mode: true,
            ..LlmCaps::none()
        };
        let want = LlmCaps {
            json_mode: true,
            ..LlmCaps::none()
        };
        assert!(have.satisfies(&want));
    }

    #[test]
    fn caps_rejects_when_required_missing() {
        let have = LlmCaps {
            tool_use: true,
            ..LlmCaps::none()
        };
        let want = LlmCaps {
            json_mode: true,
            ..LlmCaps::none()
        };
        assert!(!have.satisfies(&want));
    }

    #[test]
    fn caps_none_requires_nothing() {
        let have = LlmCaps::none();
        let want = LlmCaps::none();
        assert!(have.satisfies(&want));
    }
}
