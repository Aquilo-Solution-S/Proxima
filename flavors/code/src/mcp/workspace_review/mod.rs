// Workspace review module
// Split from workspace_review.rs for improved compile times

pub mod helpers;
pub mod ingest;
pub mod loaders;
pub mod tools;
pub mod types;

// Re-export all public types and tools from the original file

pub use helpers::*;
pub use ingest::{append_review_derived_edge, append_review_reviews_edge};
pub use loaders::*;
pub use tools::*;
pub use types::*;

// Re-export constants
pub use types::{
    MAX_WORKSPACE_VETO_ROUNDS, WORKSPACE_REVIEW_OBJECT_SCHEMA, WORKSPACE_REVIEW_SOURCE_ID,
    WORKSPACE_REVIEW_WHOLE_SCHEMA,
};

pub const CODE_REVIEWS_RELATION: &str = "proxima-code/reviews";
