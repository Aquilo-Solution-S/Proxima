//! Engine composite — wires SchemaRegistry, MemoryStore, and
//! an AuthResolver behind the typed verb surfaces of
//! docs/14-protocol-surface.md.

mod builder;
mod goals;
mod ingest;
mod operators;
mod query;

use std::collections::HashMap;
use std::sync::Arc;

use crate::ModelTier;
use crate::auth::AuthResolver;
use crate::error::ProtocolError;
use crate::operators::{EmbeddingClient, LlmClient, OperatorError, OperatorRegistry};
use crate::storage::{StorageError, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::SchemaRegistry;

pub struct Engine {
    registry: SchemaRegistry,
    // TODO(M3.B): remove MemoryStore
    memories: MemoryStore,
    auth: Box<dyn AuthResolver>,
    storage: StorageHandle,
    operators: OperatorRegistry,
    llms: HashMap<ModelTier, Arc<dyn LlmClient>>,
    embed: Option<Arc<dyn EmbeddingClient>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("memories", &self.memories)
            .field("auth", &"<dyn AuthResolver>")
            .field("storage", &"<dyn Storage>")
            .finish()
    }
}

pub(super) fn map_storage_err_for_goal_write(
    request_id: &str,
) -> impl FnOnce(StorageError) -> ProtocolError + '_ {
    move |e| match e {
        StorageError::ConstraintViolation(msg) if msg.starts_with("idempotency_conflict:") => {
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::NotFound => ProtocolError::not_found("prior goal not found"),
        other => ProtocolError::internal(other.to_string()),
    }
}

pub(super) fn map_operator_err(e: OperatorError) -> ProtocolError {
    ProtocolError::internal(format!("operator: {e}"))
}

#[cfg(test)]
mod tier_union_tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::ids::{OrgId, UserId};
    use crate::operators::{
        F2AContext, F2AOperator, NewAbstraction, OperatorError, OperatorRegistry,
    };
    use crate::verbs::query::MemoryStore;
    use crate::verbs::schema::SchemaRegistry;
    use crate::{LlmCaps, ModelTier, Owner, Principal, SchemaId};
    use async_trait::async_trait;
    use uuid::Uuid;

    #[derive(Debug)]
    struct OpAt {
        tier: ModelTier,
        requires: LlmCaps,
    }
    #[async_trait]
    impl F2AOperator for OpAt {
        fn operator_id(&self) -> &'static str {
            "test/op"
        }
        fn output_schema_id(&self) -> &'static str {
            "test/out"
        }
        fn output_schema_version(&self) -> u32 {
            1
        }
        fn prompt_version(&self) -> &'static str {
            "v1"
        }
        fn consumes(&self, _: &SchemaId) -> bool {
            true
        }
        async fn run(&self, _: F2AContext<'_>) -> Result<Vec<NewAbstraction>, OperatorError> {
            Ok(Vec::new())
        }
        fn tier(&self) -> ModelTier {
            self.tier
        }
        fn requires(&self) -> LlmCaps {
            self.requires
        }
    }

    fn engine_with_ops(ops: Vec<OpAt>) -> Engine {
        let mut reg = OperatorRegistry::new();
        for op in ops {
            reg.register_f2a(op);
        }
        let principal = Principal::User(UserId::new(Uuid::now_v7()));
        let owner = Owner {
            principal: principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        Engine::new(
            SchemaRegistry::new(),
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner)),
        )
        .with_operators(reg)
    }

    #[test]
    fn union_empty_when_no_ops_at_tier() {
        let eng = engine_with_ops(vec![OpAt {
            tier: ModelTier::Fast,
            requires: LlmCaps {
                tool_use: true,
                ..LlmCaps::none()
            },
        }]);
        assert_eq!(
            eng.tier_requires_union(ModelTier::Standard),
            LlmCaps::none()
        );
    }

    #[test]
    fn union_combines_caps_across_ops_at_same_tier() {
        let eng = engine_with_ops(vec![
            OpAt {
                tier: ModelTier::Standard,
                requires: LlmCaps {
                    tool_use: true,
                    ..LlmCaps::none()
                },
            },
            OpAt {
                tier: ModelTier::Standard,
                requires: LlmCaps {
                    json_mode: true,
                    ..LlmCaps::none()
                },
            },
            OpAt {
                tier: ModelTier::Deep,
                requires: LlmCaps {
                    vision: true,
                    ..LlmCaps::none()
                },
            },
        ]);
        let standard = eng.tier_requires_union(ModelTier::Standard);
        assert!(standard.tool_use);
        assert!(standard.json_mode);
        assert!(!standard.vision); // vision was on Deep, not Standard
    }
}
