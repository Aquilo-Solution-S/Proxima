//! Fact-retention cleanup verb result.

#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CleanupDueFactsOutcome {
    pub facts_erased: u64,
    pub derivatives_tombstoned: u64,
    pub cited_objects_erased: u64,
    pub orphaned_s3_blobs: Vec<OrphanedS3Blob>,
}

#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct OrphanedS3Blob {
    pub bucket: String,
    pub object_key: String,
}
