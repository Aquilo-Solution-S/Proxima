//! Static derivation of the produces-set from a substrate tool palette.
//!
//! These helpers are the canonical mapping
//! `(palette, registry) → {schema_ids, relation_ids}`. The substrate
//! emit tools (`core/emit_abstraction`, `core/emit_perspective`,
//! `core/create_edge`) gate runtime authorization on the same lists,
//! constructed via these helpers in the wake fire path.

use crate::Engine;
use crate::verbs::schema::PayloadKind;

/// Schemas this palette could emit, given the registry. Returns one
/// schema_id per registered Abstraction schema if `core/emit_abstraction`
/// is present, plus one per registered Perspective schema if
/// `core/emit_perspective` is present. Empty if neither emit tool is
/// in the palette.
#[must_use]
pub fn writeable_schemas_for_palette(engine: &Engine, palette: &[String]) -> Vec<String> {
    let allow_abstraction = palette.iter().any(|id| id == "core/emit_abstraction");
    let allow_perspective = palette.iter().any(|id| id == "core/emit_perspective");
    engine
        .registry()
        .list()
        .into_iter()
        .filter(|schema| {
            (allow_abstraction && schema.kind == PayloadKind::Abstraction)
                || (allow_perspective && schema.kind == PayloadKind::Perspective)
        })
        .map(|schema| schema.schema_id.into_inner())
        .collect()
}

/// Relations this palette could create. Returns every registered
/// relation if `core/create_edge` is in the palette; empty otherwise.
#[must_use]
pub fn writeable_relations_for_palette(engine: &Engine, palette: &[String]) -> Vec<String> {
    if !palette.iter().any(|id| id == "core/create_edge") {
        return Vec::new();
    }
    engine
        .registry()
        .list_relations()
        .iter()
        .map(|relation| relation.relation.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::ids::{OrgId, SchemaId, SchemaVersion, UserId};
    use crate::owner::{Owner, Principal};
    use crate::relation::{RelationClass, RelationDescriptor};
    use crate::verbs::query::MemoryStore;
    use crate::verbs::schema::{FlavorRegistryFrozen, SchemaInfo};
    use crate::Engine;

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
        ];
        let relations = vec![RelationDescriptor::substrate(
            "test/related-to",
            RelationClass::Causal,
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
        assert!(
            !schemas.is_empty(),
            "expected at least one Abstraction schema in test registry"
        );
        assert!(
            schemas.iter().all(|id| id == "test/abstraction-v1"),
            "only Abstraction schemas should be returned"
        );
        // No relations: create_edge not in palette
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn create_edge_only_returns_all_relations() {
        let engine = engine_with_test_registry();
        let palette = vec!["core/create_edge".to_string()];
        let relations = writeable_relations_for_palette(&engine, &palette);
        assert!(
            !relations.is_empty(),
            "expected at least one relation in test registry"
        );
        assert!(writeable_schemas_for_palette(&engine, &palette).is_empty());
    }

    #[test]
    fn unknown_palette_ids_are_ignored() {
        let engine = engine_with_test_registry();
        let palette = vec!["does/not/exist".to_string()];
        assert!(writeable_schemas_for_palette(&engine, &palette).is_empty());
        assert!(writeable_relations_for_palette(&engine, &palette).is_empty());
    }
}
