//! Static derivation of the produces-set from a substrate tool palette.
//!
//! These helpers are the canonical mapping
//! `(palette, registry) → {schema_ids, relation_ids}`. The substrate
//! emit tools (`core/emit_abstraction`, `core/emit_perspective`) gate
//! runtime authorization on the same lists, constructed via these
//! helpers in the wake fire path. Flavor MCP tools can additionally
//! advertise produced schemas through their registered tool descriptor.

use crate::Engine;

use super::{broad_emit_kind, parse_scoped_emit_tool_id};

/// Schemas this palette could emit, given the registry.
#[must_use]
pub fn writeable_schemas_for_palette(engine: &Engine, palette: &[String]) -> Vec<String> {
    let mut schema_ids: Vec<String> = Vec::new();
    for palette_id in palette {
        if let Some(kind) = broad_emit_kind(palette_id) {
            schema_ids.extend(
                engine
                    .registry()
                    .list()
                    .into_iter()
                    .filter(|schema| schema.kind == kind)
                    .map(|schema| schema.schema_id.into_inner()),
            );
            continue;
        }
        if let Ok(Some(scoped)) = parse_scoped_emit_tool_id(palette_id) {
            if engine
                .registry()
                .lookup_payload(
                    &crate::SchemaId::new(scoped.schema_id.clone()),
                    crate::SchemaVersion::new(scoped.schema_version),
                    scoped.kind,
                )
                .is_some()
            {
                schema_ids.push(scoped.schema_id);
            }
        }
    }

    for tool in engine.registry().list_mcp_tools() {
        if palette.iter().any(|id| id == tool.name) {
            schema_ids.extend(tool.produces_schema_ids.iter().map(|id| (*id).to_string()));
        }
    }
    schema_ids.sort();
    schema_ids.dedup();
    schema_ids
}

/// Relations this palette could create. v1 has no generic substrate
/// relation writer; relation-authoring tools are flavor-specific.
#[must_use]
pub fn writeable_relations_for_palette(_engine: &Engine, _palette: &[String]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::ids::{OrgId, SchemaId, SchemaVersion, UserId};
    use crate::owner::{Owner, Principal};
    use crate::relation::{RelationClass, RelationDescriptor};
    use crate::verbs::query::MemoryStore;
    use crate::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
    use crate::{Engine, McpTool};

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct TestArgs;

    #[derive(Debug, serde::Serialize)]
    struct TestOutput;

    #[derive(Debug)]
    struct TestFactTool;

    impl McpTool for TestFactTool {
        const NAME: &'static str = "test/emit_fact";
        const DESCRIPTION: &'static str = "emit a test fact";
        const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &["test/fact-v1"];
        type Args = TestArgs;
        type Output = TestOutput;

        fn call(
            _ctx: crate::McpToolCtx,
            _args: Self::Args,
        ) -> futures::future::BoxFuture<'static, Result<Self::Output, crate::McpToolError>>
        {
            Box::pin(async { Ok(TestOutput) })
        }
    }

    /// Build an Engine with one Abstraction schema, one Perspective schema,
    /// and one Relation, sufficient to exercise all four palette shapes.
    fn engine_with_test_registry() -> Engine {
        let schemas = vec![
            SchemaInfo {
                schema_id: SchemaId::new("test/abstraction-v1".to_string()),
                schema_version: SchemaVersion::new(1),
                kind: PayloadKind::Abstraction,
                filter_keys: vec![],
                sidecar_table: Some("test.abstraction_v1".to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                cbor_encoder: None,
            },
            SchemaInfo {
                schema_id: SchemaId::new("test/perspective-v1".to_string()),
                schema_version: SchemaVersion::new(1),
                kind: PayloadKind::Perspective,
                filter_keys: vec![],
                sidecar_table: Some("test.perspective_v1".to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                cbor_encoder: None,
            },
            SchemaInfo {
                schema_id: SchemaId::new("test/shared-v1".to_string()),
                schema_version: SchemaVersion::new(1),
                kind: PayloadKind::Abstraction,
                filter_keys: vec![],
                sidecar_table: Some("test.shared_abstraction_v1".to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                cbor_encoder: None,
            },
            SchemaInfo {
                schema_id: SchemaId::new("test/shared-v1".to_string()),
                schema_version: SchemaVersion::new(1),
                kind: PayloadKind::Perspective,
                filter_keys: vec![],
                sidecar_table: Some("test.shared_perspective_v1".to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                cbor_encoder: None,
            },
        ];
        let relations = vec![RelationDescriptor::substrate(
            "test/related-to",
            RelationClass::Causal,
            crate::EntityKindMask::perspective(),
            crate::EntityKindMask::fact(),
            crate::AuthorshipKindMask::engine(),
        )];
        let registry = FlavorRegistryFrozen::with_schemas_and_relations(schemas, relations);
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::from_u128(1))),
            org_id: OrgId::new(uuid::Uuid::from_u128(2)),
        };
        Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(owner.principal.clone(), owner)),
        )
    }

    fn engine_with_test_tool_registry() -> Engine {
        let mut registry = crate::FlavorRegistry::new();
        registry.add_mcp_tool::<TestFactTool>("test");
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::from_u128(1))),
            org_id: OrgId::new(uuid::Uuid::from_u128(2)),
        };
        Engine::new(
            registry.freeze(),
            MemoryStore::new(),
            Box::new(NoAuth::new(owner.principal.clone(), owner)),
        )
    }

    #[test]
    fn empty_palette_produces_nothing() {
        let engine = engine_with_test_registry();
        assert!(writeable_schemas_for_palette(&engine, &[]).is_empty());
        assert!(writeable_relations_for_palette(&engine, &[]).is_empty());
    }

    #[test]
    fn emit_abstraction_only_returns_abstraction_schemas() {
        let engine = engine_with_test_registry();
        let palette = vec!["core/emit_abstraction".to_string()];
        let schemas = writeable_schemas_for_palette(&engine, &palette);
        assert_eq!(
            schemas,
            vec![
                "test/abstraction-v1".to_string(),
                "test/shared-v1".to_string()
            ]
        );
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn scoped_emit_abstraction_returns_only_named_schema() {
        let engine = engine_with_test_registry();
        let palette = vec!["core/emit_abstraction::test/abstraction-v1::v1".to_string()];
        let schemas = writeable_schemas_for_palette(&engine, &palette);
        assert_eq!(schemas, vec!["test/abstraction-v1".to_string()]);
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn scoped_emit_abstraction_ignores_wrong_kind_schema() {
        let engine = engine_with_test_registry();
        let palette = vec!["core/emit_abstraction::test/perspective-v1::v1".to_string()];
        assert!(writeable_schemas_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn relation_writes_are_not_inferred_from_palette() {
        let engine = engine_with_test_registry();
        let palette = vec!["core/create_edge".to_string()];
        let relations = writeable_relations_for_palette(&engine, &palette);
        assert!(relations.is_empty());
        assert!(writeable_schemas_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn emit_perspective_only_returns_perspective_schemas() {
        let engine = engine_with_test_registry();
        let palette = vec!["core/emit_perspective".to_string()];
        let schemas = writeable_schemas_for_palette(&engine, &palette);
        assert_eq!(
            schemas,
            vec![
                "test/perspective-v1".to_string(),
                "test/shared-v1".to_string()
            ]
        );
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn scoped_emit_perspective_returns_shared_schema_when_abstraction_registered_first() {
        let engine = engine_with_test_registry();
        let palette = vec!["core/emit_perspective::test/shared-v1::v1".to_string()];
        let schemas = writeable_schemas_for_palette(&engine, &palette);
        assert_eq!(schemas, vec!["test/shared-v1".to_string()]);
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn unknown_palette_ids_are_ignored() {
        let engine = engine_with_test_registry();
        let palette = vec!["does/not/exist".to_string()];
        assert!(writeable_schemas_for_palette(&engine, &palette).is_empty());
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn flavor_mcp_tool_descriptor_can_advertise_fact_outputs() {
        let engine = engine_with_test_tool_registry();
        let palette = vec!["test/emit_fact".to_string()];

        let schemas = writeable_schemas_for_palette(&engine, &palette);

        assert_eq!(schemas, vec!["test/fact-v1".to_string()]);
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }
}
