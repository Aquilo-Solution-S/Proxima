use std::sync::Arc;

use crate::personality::PersonalityTool;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizationError {
    #[error("tool outside palette: {tool_id}")]
    OutsidePalette { tool_id: String },
    #[error("schema outside writeable_schemas: {schema_id}")]
    OutsideWriteableSchemas { schema_id: String },
}

pub fn authorize_tool_call(
    tool_id: &str,
    palette: &[Arc<dyn PersonalityTool>],
) -> Result<(), AuthorizationError> {
    if palette.iter().any(|tool| tool.tool_id() == tool_id) {
        return Ok(());
    }
    Err(AuthorizationError::OutsidePalette {
        tool_id: tool_id.to_string(),
    })
}

pub fn authorize_emit(
    schema_id: &str,
    writeable_schemas: &[String],
) -> Result<(), AuthorizationError> {
    if writeable_schemas.iter().any(|id| id == schema_id) {
        return Ok(());
    }
    Err(AuthorizationError::OutsideWriteableSchemas {
        schema_id: schema_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::personality::{PersonalityToolContext, PersonalityToolResult};
    use async_trait::async_trait;

    #[derive(Debug)]
    struct DemoTool;

    #[async_trait]
    impl PersonalityTool for DemoTool {
        fn tool_id(&self) -> &'static str {
            "test/demo"
        }

        fn description(&self) -> &'static str {
            "demo"
        }

        fn args_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn invoke(
            &self,
            _ctx: &PersonalityToolContext<'_>,
            _args: serde_json::Value,
        ) -> Result<PersonalityToolResult, crate::ProtocolError> {
            Ok(PersonalityToolResult::ok(serde_json::json!({})))
        }
    }

    #[test]
    fn authorizes_tool_inside_palette() {
        let palette: Vec<Arc<dyn PersonalityTool>> = vec![Arc::new(DemoTool)];
        assert_eq!(authorize_tool_call("test/demo", &palette), Ok(()));
    }

    #[test]
    fn rejects_tool_outside_palette() {
        let palette: Vec<Arc<dyn PersonalityTool>> = vec![Arc::new(DemoTool)];
        assert_eq!(
            authorize_tool_call("test/missing", &palette),
            Err(AuthorizationError::OutsidePalette {
                tool_id: "test/missing".to_string()
            })
        );
    }

    #[test]
    fn rejects_schema_outside_writeable_set() {
        let allowed = vec!["test/allowed".to_string()];
        assert_eq!(
            authorize_emit("test/other", &allowed),
            Err(AuthorizationError::OutsideWriteableSchemas {
                schema_id: "test/other".to_string()
            })
        );
    }
}
