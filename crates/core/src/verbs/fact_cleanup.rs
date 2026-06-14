//! Fact-retention cleanup verb result.

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CleanupDueFactsOutcome {
    pub facts_erased: u64,
    pub derivatives_tombstoned: u64,
}
