//! Build-time capability vocabulary for LLM + embedding routing.
//!
//! Build-time declares **what** capabilities exist and **which** an
//! operator requires. It does **not** declare specific
//! `(vendor, model_id)` pairs — those are runtime configuration,
//! not flavor authorship. New models plug in at runtime by
//! declaring their caps; runtime validation gates mismatches against
//! the operator-declared `requires`.
//!
//! The contract:
//!
//! - `LlmCaps` / `EmbedCaps` enumerate the capability axes a
//!   substrate operator can demand.
//! - `Dialect` enumerates the HTTP API shapes a runtime client
//!   speaks; `vendor` (e.g. `"ollama"`, `"openai"`) lives only at
//!   runtime config.
//! - `ModelTier` is the routing class operators declare; the
//!   tier→`(vendor, model_id)` binding is runtime.
//!
//! See docs/10 §Model tiers and §Capability vocabulary.

/// Which HTTP API shape a runtime model client speaks. Independent
/// of vendor: most non-Anthropic vendors expose the `OpenAI` dialect,
/// so a runtime entry like
/// `{vendor: "openrouter", dialect: OpenAI, model_id: "anthropic/..."}`
/// is normal.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    Anthropic,
    #[serde(rename = "openai")]
    OpenAI,
}

/// Coarse routing class for operator declarations. Substrate-fixed —
/// expansion is a substrate PR per docs/10 §Model tiers.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "proxima_core.model_tier", rename_all = "snake_case")]
pub enum ModelTier {
    Fast,
    Standard,
    Deep,
}

/// LLM capability axes. Operators declare a `requires: LlmCaps` at
/// registration; runtime config binds a `(vendor, model_id)` to a
/// tier and validates that the bound model's claimed caps satisfy
/// the union of operator `requires` for that tier.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
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
    specta::Type,
)]
pub struct EmbedCaps {
    pub dim: u32,
    pub matryoshka: bool,
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
    fn model_tier_serde_lowercase() {
        assert_eq!(serde_json::to_string(&ModelTier::Fast).unwrap(), "\"fast\"");
        assert_eq!(
            serde_json::to_string(&ModelTier::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(serde_json::to_string(&ModelTier::Deep).unwrap(), "\"deep\"");
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
