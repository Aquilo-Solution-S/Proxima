use std::collections::{HashMap, HashSet};

use proxiima_core::verbs::schema::PayloadKind;
use proxiima_core::{EntityKind, StorageError};

use super::{MigrationInput, MigrationPlan, PgSchemaEvolution, refused};

/// Validate a pure migration plan before it can reach the executor.
pub(super) fn plan(
    program: &PgSchemaEvolution,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    if input.owner != plan.owner {
        return Err(refused("migration input and plan owners differ"));
    }

    let mut source_kinds = HashMap::new();
    let mut source_ids = HashSet::new();
    for source in &input.memories {
        insert_source(
            &mut source_ids,
            &mut source_kinds,
            source.id().into_inner(),
            SourceKind::Memory(source.kind()),
        )?;
    }
    for source in &input.goals {
        insert_source(
            &mut source_ids,
            &mut source_kinds,
            source.id().into_inner(),
            SourceKind::Goal,
        )?;
    }
    for source in &input.blobs {
        insert_source(
            &mut source_ids,
            &mut source_kinds,
            source.id(),
            SourceKind::Blob,
        )?;
    }
    if plan.mappings.len() != source_ids.len()
        || plan.mappings.keys().any(|id| !source_ids.contains(id))
    {
        return Err(refused(
            "migration mappings do not cover exactly the source records",
        ));
    }

    let targets = target_declarations(program);
    let mut referenced_targets = HashMap::<uuid::Uuid, Vec<uuid::Uuid>>::new();
    for (source, mapping) in &plan.mappings {
        let mut local = HashSet::new();
        for target in mapping.targets() {
            if !local.insert(target) {
                return Err(refused("migration mapping repeats a target identity"));
            }
            referenced_targets.entry(target).or_default().push(*source);
        }
        validate_mapping_kind(*source, mapping, &source_kinds, plan)?;
    }

    for target in &plan.memories {
        validate_memory_target(target, &targets, input, &source_kinds)?;
    }
    for target in &plan.goals {
        validate_goal_target(target, &targets, input, &source_kinds)?;
    }
    for target in &plan.blobs {
        validate_blob_target(target, &targets, input, &source_kinds)?;
    }

    let actual_target_ids = plan
        .memories
        .iter()
        .map(|target| target.id)
        .chain(plan.goals.iter().map(|target| target.id))
        .chain(plan.blobs.iter().map(|target| target.id))
        .collect::<Vec<_>>();
    if actual_target_ids.iter().collect::<HashSet<_>>().len() != actual_target_ids.len() {
        return Err(refused("migration plan repeats a target identity"));
    }
    let actual_targets = actual_target_ids.into_iter().collect::<HashSet<_>>();
    if actual_targets != referenced_targets.keys().copied().collect() {
        return Err(refused(
            "every planned target must be referenced by a source mapping",
        ));
    }

    validate_fact_merges(input, &referenced_targets)?;
    validate_blob_merges(input, &referenced_targets)?;
    validate_retained_state(input, plan, &referenced_targets)?;
    Ok(())
}

fn validate_retained_state(
    input: &MigrationInput,
    plan: &MigrationPlan,
    contributors: &HashMap<uuid::Uuid, Vec<uuid::Uuid>>,
) -> Result<(), StorageError> {
    for target in &plan.memories {
        let sources = &contributors[&target.id];
        if !sources.contains(&target.source.id().into_inner()) {
            return Err(refused(
                "memory target attribution must come from a mapped contributor",
            ));
        }
        let forgotten = target.source.row.forgotten_at.is_some();
        if input
            .memories
            .iter()
            .filter(|m| sources.contains(&m.id().into_inner()))
            .any(|m| m.row.forgotten_at.is_some() != forgotten)
        {
            return Err(refused(
                "schema migration cannot merge live and forgotten memories",
            ));
        }
    }
    for target in &plan.goals {
        let sources = &contributors[&target.id];
        if !sources.contains(&target.source.id().into_inner()) {
            return Err(refused(
                "Goal target attribution must come from a mapped contributor",
            ));
        }
        if input
            .goals
            .iter()
            .filter(|g| sources.contains(&g.id().into_inner()))
            .any(|g| g.state != target.source.state)
        {
            return Err(refused(
                "schema migration cannot merge different Goal lifecycle states",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SourceKind {
    Memory(EntityKind),
    Goal,
    Blob,
}

fn insert_source(
    ids: &mut HashSet<uuid::Uuid>,
    kinds: &mut HashMap<uuid::Uuid, SourceKind>,
    id: uuid::Uuid,
    kind: SourceKind,
) -> Result<(), StorageError> {
    if !ids.insert(id) || kinds.insert(id, kind).is_some() {
        return Err(refused(
            "source identity is duplicated across migration inputs",
        ));
    }
    Ok(())
}

fn validate_mapping_kind(
    source: uuid::Uuid,
    mapping: &super::plan::IdentityMapping,
    source_kinds: &HashMap<uuid::Uuid, SourceKind>,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    let kind = source_kinds
        .get(&source)
        .ok_or_else(|| refused("mapping names an unknown source"))?;
    let targets = mapping.targets().collect::<HashSet<_>>();
    let matching = plan
        .memories
        .iter()
        .filter(|target| targets.contains(&target.id))
        .map(|target| SourceKind::Memory(target.source.kind()))
        .chain(
            plan.goals
                .iter()
                .filter(|target| targets.contains(&target.id))
                .map(|_| SourceKind::Goal),
        )
        .chain(
            plan.blobs
                .iter()
                .filter(|target| targets.contains(&target.id))
                .map(|_| SourceKind::Blob),
        );
    for target_kind in matching {
        if !same_kind(*kind, target_kind) {
            return Err(refused("migration changes an admission kind"));
        }
    }
    Ok(())
}

fn same_kind(left: SourceKind, right: SourceKind) -> bool {
    match (left, right) {
        (SourceKind::Memory(a), SourceKind::Memory(b)) => a == b,
        (SourceKind::Goal, SourceKind::Goal) | (SourceKind::Blob, SourceKind::Blob) => true,
        _ => false,
    }
}

type TargetDecl = (
    proxiima_core::SchemaId,
    proxiima_core::SchemaVersion,
    PayloadKind,
);

fn target_declarations(program: &PgSchemaEvolution) -> Vec<TargetDecl> {
    program
        .declaration
        .targets
        .iter()
        .map(|target| (target.schema_id(), target.schema_version(), target.kind))
        .collect()
}

fn declared(targets: &[TargetDecl], schema: &str, version: u32, kind: PayloadKind) -> bool {
    targets.iter().any(|(id, target_version, target_kind)| {
        id.as_str() == schema && target_version.into_inner() == version && *target_kind == kind
    })
}

fn validate_memory_target(
    target: &super::plan::MemoryTarget,
    declarations: &[TargetDecl],
    input: &MigrationInput,
    source_kinds: &HashMap<uuid::Uuid, SourceKind>,
) -> Result<(), StorageError> {
    let source = input
        .memories
        .iter()
        .find(|source| source.id() == target.source.id())
        .ok_or_else(|| refused("memory target basis is not a migration source"))?;
    if target.source.owner() != input.owner
        || source.owner() != input.owner
        || source.kind() != target.source.kind()
    {
        return Err(refused(
            "memory target owner or source payload is inconsistent",
        ));
    }
    if let Some(kind) = source_kinds.get(&target.id)
        && (target.source.id().into_inner() != target.id
            || !same_kind(*kind, SourceKind::Memory(target.source.kind())))
    {
        return Err(refused("retained memory target does not match its source"));
    }
    if target.payload.kind == PayloadKind::Goal {
        return Err(refused("Goal payload cannot be a memory target"));
    }
    if !declared(
        declarations,
        target.payload.schema_id.as_str(),
        target.payload.schema_version.into_inner(),
        target.payload.kind,
    ) {
        return Err(refused(
            "memory target schema is not a declared evolution target",
        ));
    }
    let expected = match target.source.kind() {
        EntityKind::Fact => PayloadKind::Fact,
        EntityKind::Abstraction => PayloadKind::Abstraction,
        EntityKind::Perspective => PayloadKind::Perspective,
        EntityKind::Goal => return Err(refused("Goal cannot be a memory target")),
    };
    if target.payload.kind != expected {
        return Err(refused("retained memory identity changes kind"));
    }
    Ok(())
}

fn validate_goal_target(
    target: &super::plan::GoalTarget,
    declarations: &[TargetDecl],
    input: &MigrationInput,
    source_kinds: &HashMap<uuid::Uuid, SourceKind>,
) -> Result<(), StorageError> {
    let source = input
        .goals
        .iter()
        .find(|source| source.id() == target.source.id())
        .ok_or_else(|| refused("Goal target basis is not a migration source"))?;
    if target.source.owner() != input.owner
        || source.owner() != input.owner
        || target
            .payload
            .sidecar_payload
            .as_ref()
            .map(|payload| payload.kind)
            != Some(PayloadKind::Goal)
    {
        return Err(refused(
            "Goal target owner or typed payload is inconsistent",
        ));
    }
    if let Some(kind) = source_kinds.get(&target.id)
        && (target.source.id().into_inner() != target.id || !same_kind(*kind, SourceKind::Goal))
    {
        return Err(refused("retained Goal target does not match its source"));
    }
    if !declared(
        declarations,
        target.payload.schema_id.as_str(),
        target.payload.schema_version.into_inner(),
        PayloadKind::Goal,
    ) {
        return Err(refused(
            "Goal target schema is not a declared evolution target",
        ));
    }
    Ok(())
}

fn validate_blob_target(
    target: &super::plan::BlobTarget,
    declarations: &[TargetDecl],
    input: &MigrationInput,
    source_kinds: &HashMap<uuid::Uuid, SourceKind>,
) -> Result<(), StorageError> {
    let source = input
        .blobs
        .iter()
        .find(|source| source.id() == target.source.id())
        .ok_or_else(|| refused("blob target basis is not a migration source"))?;
    if target.source.owner() != input.owner
        || source.owner() != input.owner
        || target.source.content_hash().is_empty()
        || source.content_hash().is_empty()
    {
        return Err(refused("blob target owner or immutable bytes are invalid"));
    }
    if let Some(kind) = source_kinds.get(&target.id)
        && (target.source.id() != target.id || !same_kind(*kind, SourceKind::Blob))
    {
        return Err(refused("retained blob target does not match its source"));
    }
    if !declared(
        declarations,
        &target.schema.render(),
        target.schema.schema_version().into_inner(),
        PayloadKind::CitedObject,
    ) {
        return Err(refused(
            "blob target schema is not a declared evolution target",
        ));
    }
    Ok(())
}

fn validate_fact_merges(
    input: &MigrationInput,
    references: &HashMap<uuid::Uuid, Vec<uuid::Uuid>>,
) -> Result<(), StorageError> {
    for (target, sources) in references {
        let facts = sources
            .iter()
            .filter_map(|source| {
                input
                    .memories
                    .iter()
                    .find(|candidate| candidate.id().into_inner() == *source)
            })
            .filter(|memory| memory.kind() == EntityKind::Fact)
            .collect::<Vec<_>>();
        if facts.len() > 1 {
            let first = facts[0].row().source_id();
            if facts.iter().any(|memory| memory.row().source_id() != first) {
                return Err(refused(format!(
                    "Fact merge target {target} combines different source IDs"
                )));
            }
        }
    }
    Ok(())
}

fn validate_blob_merges(
    input: &MigrationInput,
    references: &HashMap<uuid::Uuid, Vec<uuid::Uuid>>,
) -> Result<(), StorageError> {
    for sources in references.values() {
        let blobs = sources
            .iter()
            .filter_map(|source| {
                input
                    .blobs
                    .iter()
                    .find(|candidate| candidate.id() == *source)
            })
            .collect::<Vec<_>>();
        if let Some(first) = blobs.first()
            && blobs
                .iter()
                .any(|blob| blob.content_hash() != first.content_hash())
        {
            return Err(refused("blob merge changes immutable content bytes"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SourceKind, same_kind};
    use crate::sidecars::evolution::plan::{MigrationPins, MigrationPlacement, MigrationPlan};
    use crate::sidecars::evolution::rows::test_memory;
    use crate::sidecars::evolution::{MigrationInput, PgMigrationSchemas, PgSchemaEvolution};
    use proxiima_core::memory::payloads::AgentNoteV1;
    use proxiima_core::spiice::{
        EmbeddingRecipe, Provenance, SchemaContract, SchemaEvolution, SchemaRef,
        SearchProjectionDecl, TransferRule,
    };
    use proxiima_core::{EntityKind, OwnerId};
    use uuid::Uuid;

    const TARGET: SchemaContract = SchemaContract {
        id: SchemaRef::new("core", "agent-note", 1),
        kind: proxiima_core::verbs::schema::PayloadKind::Fact,
        sidecar_table: None,
        search: SearchProjectionDecl::None {
            why: "test",
            tag_column: None,
        },
        embedding: EmbeddingRecipe::Never { why: "test" },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
    };
    const DECLARATION: SchemaEvolution =
        SchemaEvolution::new("test-evolution", &[], &[TARGET], 1, &[], "test", "test");

    fn input(
        owner: OwnerId,
        memories: Vec<crate::sidecars::evolution::MigrationMemory>,
    ) -> MigrationInput {
        MigrationInput {
            owner,
            memories,
            goals: Vec::new(),
            blobs: Vec::new(),
            mapped: std::collections::HashMap::default(),
            mapped_series: std::collections::HashMap::default(),
        }
    }

    fn program() -> PgSchemaEvolution {
        PgSchemaEvolution::new(&DECLARATION, PgMigrationSchemas::new(), |_, _| Ok(()))
    }

    fn payload() -> AgentNoteV1 {
        AgentNoteV1 {
            note_id: Uuid::nil(),
            title: "title".into(),
            body: "body".into(),
            tags: Vec::new(),
            idempotency_key: None,
        }
    }

    #[test]
    fn mapping_kind_is_closed() {
        assert!(same_kind(
            SourceKind::Memory(EntityKind::Fact),
            SourceKind::Memory(EntityKind::Fact)
        ));
        assert!(!same_kind(
            SourceKind::Memory(EntityKind::Fact),
            SourceKind::Memory(EntityKind::Abstraction)
        ));
        assert!(!same_kind(SourceKind::Goal, SourceKind::Blob));
    }

    #[test]
    fn mappings_must_cover_every_source() {
        let owner = OwnerId::new(Uuid::new_v4());
        let source = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("source"));
        let plan = MigrationPlan::new(owner, 1);
        let error = super::plan(&program(), &input(owner, vec![source]), &plan)
            .expect_err("an unmapped source is refused");
        assert!(error.to_string().contains("cover exactly"));
    }

    #[test]
    fn merging_cannot_reexpose_forgotten_contributors() {
        let owner = OwnerId::new(Uuid::new_v4());
        let first = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("source"));
        let mut second = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("source"));
        second.row.forgotten_at = Some(time::OffsetDateTime::UNIX_EPOCH);
        let mut plan = MigrationPlan::new(owner, 1);
        let target = plan
            .fact(
                &first,
                payload(),
                MigrationPlacement::RETAIN,
                MigrationPins::default(),
            )
            .unwrap();
        plan.map_memory(&first, target, &[]).unwrap();
        plan.map_memory(&second, target, &[]).unwrap();
        let error = super::plan(&program(), &input(owner, vec![first, second]), &plan).unwrap_err();
        assert!(error.to_string().contains("live and forgotten"), "{error}");
    }

    #[test]
    fn refused_duplicate_mapping_keeps_the_original_choice() {
        let owner = OwnerId::new(Uuid::new_v4());
        let source = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("source"));
        let mut plan = MigrationPlan::new(owner, 1);
        plan.map_memory(&source, source.id(), &[]).unwrap();
        assert!(
            plan.map_memory(&source, proxiima_core::MemoryId::new(Uuid::new_v4()), &[])
                .is_err()
        );
        assert_eq!(
            plan.mappings[&source.id().into_inner()].primary,
            source.id().into_inner()
        );
    }

    #[test]
    fn duplicate_actual_target_ids_are_rejected() {
        let owner = OwnerId::new(Uuid::new_v4());
        let source = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("source"));
        let mut plan = MigrationPlan::new(owner, 1);
        let target = plan
            .fact(
                &source,
                payload(),
                MigrationPlacement::RETAIN,
                MigrationPins::default(),
            )
            .unwrap();
        plan.fact(
            &source,
            payload(),
            MigrationPlacement::RETAIN,
            MigrationPins::default(),
        )
        .unwrap();
        plan.map_memory(&source, target, &[]).unwrap();
        let error = super::plan(&program(), &input(owner, vec![source]), &plan)
            .expect_err("duplicate target rows are refused");
        assert!(error.to_string().contains("target identity"), "{error}");
    }

    #[test]
    fn layer_and_owner_conservation_are_checked_at_plan_validation() {
        let owner = OwnerId::new(Uuid::new_v4());
        let source = test_memory(
            owner,
            Uuid::new_v4(),
            EntityKind::Abstraction,
            Some("source"),
        );
        let mut plan = MigrationPlan::new(owner, 1);
        let error = plan
            .fact(
                &source,
                payload(),
                MigrationPlacement::RETAIN,
                MigrationPins::default(),
            )
            .expect_err("a Fact API cannot rewrite an Abstraction");
        assert!(error.to_string().contains("layer"));

        let foreign = test_memory(
            OwnerId::new(Uuid::new_v4()),
            Uuid::new_v4(),
            EntityKind::Fact,
            Some("foreign"),
        );
        let mut plan = MigrationPlan::new(owner, 1);
        let target = plan
            .fact(
                &foreign,
                payload(),
                MigrationPlacement::RETAIN,
                MigrationPins::default(),
            )
            .unwrap();
        plan.map_memory(&foreign, target, &[]).unwrap();
        let error = super::plan(&program(), &input(owner, vec![foreign]), &plan)
            .expect_err("a foreign source owner is refused");
        assert!(error.to_string().contains("owner"));
    }

    #[test]
    fn fact_merge_requires_one_source_id_but_allows_same_source_id() {
        let owner = OwnerId::new(Uuid::new_v4());
        let first = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("same"));
        let second = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("same"));
        let mut plan = MigrationPlan::new(owner, 1);
        let target = plan
            .fact(
                &first,
                payload(),
                MigrationPlacement::RETAIN,
                MigrationPins::default(),
            )
            .unwrap();
        plan.map_memory(&first, target, &[]).unwrap();
        plan.map_memory(&second, target, &[]).unwrap();
        super::plan(
            &program(),
            &input(owner, vec![first.clone(), second.clone()]),
            &plan,
        )
        .expect("same source id merge is allowed");

        let different = test_memory(owner, Uuid::new_v4(), EntityKind::Fact, Some("different"));
        let mut plan = MigrationPlan::new(owner, 1);
        let target = plan
            .fact(
                &first,
                payload(),
                MigrationPlacement::RETAIN,
                MigrationPins::default(),
            )
            .unwrap();
        plan.map_memory(&first, target, &[]).unwrap();
        plan.map_memory(&different, target, &[]).unwrap();
        let error = super::plan(&program(), &input(owner, vec![first, different]), &plan)
            .expect_err("different source IDs cannot merge");
        assert!(error.to_string().contains("different source IDs"));
    }
}
