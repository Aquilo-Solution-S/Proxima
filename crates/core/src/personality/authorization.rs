use std::sync::Arc;

use crate::personality::PersonalityTool;
use crate::{CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizationError {
    #[error("tool outside palette: {tool_id}")]
    OutsidePalette { tool_id: String },
    #[error("schema outside writeable_schemas: {schema_id}")]
    OutsideWriteableSchemas { schema_id: String },
    #[error("relation outside writeable_relations: {relation_id}")]
    OutsideWriteableRelations { relation_id: String },
    #[error("relation is substrate-only: {relation_id}")]
    SubstrateOnlyRelation { relation_id: String },
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
    writeable_schemas: &[&'static str],
) -> Result<(), AuthorizationError> {
    if writeable_schemas.contains(&schema_id) {
        return Ok(());
    }
    Err(AuthorizationError::OutsideWriteableSchemas {
        schema_id: schema_id.to_string(),
    })
}

pub fn authorize_create_edge(
    relation_id: &str,
    writeable_relations: &[&'static str],
) -> Result<(), AuthorizationError> {
    if matches!(
        relation_id,
        CORE_DERIVED_FROM_RELATION | CORE_SUPERSEDES_RELATION
    ) {
        return Err(AuthorizationError::SubstrateOnlyRelation {
            relation_id: relation_id.to_string(),
        });
    }
    if writeable_relations.contains(&relation_id) {
        return Ok(());
    }
    Err(AuthorizationError::OutsideWriteableRelations {
        relation_id: relation_id.to_string(),
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
        assert_eq!(
            authorize_emit("test/other", &["test/allowed"]),
            Err(AuthorizationError::OutsideWriteableSchemas {
                schema_id: "test/other".to_string()
            })
        );
    }

    #[test]
    fn rejects_relation_outside_writeable_set() {
        assert_eq!(
            authorize_create_edge("test/other", &["test/allowed"]),
            Err(AuthorizationError::OutsideWriteableRelations {
                relation_id: "test/other".to_string()
            })
        );
    }

    #[test]
    fn rejects_substrate_only_relations() {
        assert_eq!(
            authorize_create_edge(CORE_DERIVED_FROM_RELATION, &[CORE_DERIVED_FROM_RELATION]),
            Err(AuthorizationError::SubstrateOnlyRelation {
                relation_id: CORE_DERIVED_FROM_RELATION.to_string()
            })
        );
    }
}
