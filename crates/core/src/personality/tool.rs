//! Personality tool trait and result types.
//!
//! This module contains the tooling infrastructure for personalities:
//! - `PersonalityTool` - Trait for personality tools
//! - `PersonalityToolResult` - Result of a tool invocation

use async_trait::async_trait;

use crate::error::ProtocolError;

use super::context::PersonalityToolContext;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PersonalityToolResult {
    pub content: serde_json::Value,
    pub is_error: bool,
}

impl PersonalityToolResult {
    #[must_use]
    pub fn ok(content: serde_json::Value) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    #[must_use]
    pub fn error(content: serde_json::Value) -> Self {
        Self {
            content,
            is_error: true,
        }
    }
}

#[async_trait]
pub trait PersonalityTool: Send + Sync + std::fmt::Debug {
    fn tool_id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn args_schema(&self) -> serde_json::Value;

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError>;
}
