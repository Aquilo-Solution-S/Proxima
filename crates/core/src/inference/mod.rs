//! Settings-side InferenceTarget verbs + WakeEntry validation pipeline.
//!
//! The dispatcher rewrite (next plan) will add adapter-trait abstraction
//! and the actual goose-run subprocess. v1 only ships the Settings
//! storage surface and the write-time validation that gates
//! `set_wake_entries`.

pub mod types;

pub mod bind_inference_tier;
pub mod list_inference_targets;
pub mod list_inference_tier_bindings;
pub mod register_inference_target;
pub mod remove_inference_target;
pub mod set_wake_entries;

pub use types::{
    BindInferenceTierRequest, BindInferenceTierResponse, InferenceTargetConfig, InferenceTargetRow,
    InferenceTierBindingRow, ListInferenceTargetsRequest, ListInferenceTierBindingsRequest,
    MistralChatConfig, OpenAIChatConfig, OpenAIResponsesConfig, RegisterInferenceTargetRequest,
    RegisterInferenceTargetResponse, RemoveInferenceTargetRequest, RemoveInferenceTargetResponse,
};
