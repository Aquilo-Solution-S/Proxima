//! Settings-side InferenceTarget verbs + WakeEntry validation pipeline.
//!
//! `InferenceTargetConfig` enumerates the providers the harness can
//! drive (`MistralChat`, `OpenAIChat`, `OpenAIResponses`). This module
//! owns the Settings storage surface and the write-time validation
//! that gates `set_wake_entries`.

pub mod types;

pub mod bind_inference_tier;
pub mod list_inference_targets;
pub mod list_inference_tier_bindings;
pub mod register_inference_target;
pub mod remove_inference_target;
pub mod set_wake_entries;

pub use types::{
    BindInferenceTierRequest, BindInferenceTierResponse, ChatGPTCodexConfig, InferenceTargetConfig,
    InferenceTargetKind, InferenceTargetRow, InferenceTierBindingRow, ListInferenceTargetsRequest,
    ListInferenceTierBindingsRequest, MistralChatConfig, OpenAIChatConfig, OpenAIResponsesConfig,
    RegisterInferenceTargetRequest, RegisterInferenceTargetResponse, RemoveInferenceTargetRequest,
    RemoveInferenceTargetResponse,
};
