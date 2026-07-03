use super::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, CORE_MOTIVATED_BY_RELATION,
    EdgeAuthorshipKind, EdgeDraft, EntityKind, EvidenceTarget, GoalAtomicContext, GoalAuthorship,
    GoalId, MemoryId, Owner, Postgres, RegisteredRelation, StorageError, SystemOrigin, Transaction,
    append_edge_in_tx, map_err,
};

pub(super) async fn edge_ids_for_goal_relations(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    relations: &[&str],
) -> Result<Vec<uuid::Uuid>, StorageError> {
    if relations.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = ANY($2)
          ORDER BY created_at ASC, edge_id ASC",
    )
    .bind(goal_id.into_inner())
    .bind(
        relations
            .iter()
            .map(|relation| (*relation).to_string())
            .collect::<Vec<_>>(),
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

pub(super) async fn edge_ids_for_lifecycle_memory(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1 OR target_memory_id = $1
          ORDER BY created_at ASC, edge_id ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

pub(super) async fn append_goal_to_self_edge(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    goal_id: GoalId,
    self_memory_id: MemoryId,
) -> Result<uuid::Uuid, StorageError> {
    let relation = resolve_relation(context, proxima_core::relation::CORE_INSPIRES_RELATION)?;
    let edge_id = uuid::Uuid::now_v7();
    let draft = EdgeDraft {
        edge_id,
        relation,
        source_kind: EntityKind::Goal,
        source_memory_id: None,
        source_goal_id: Some(goal_id.into_inner()),
        source_fact_entity_id: None,
        target_kind: EntityKind::Perspective,
        target_memory_id: Some(self_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: EdgeAuthorshipKind::PerspectiveGoalLink,
        authorship_owner_memory_id: Some(self_memory_id.into_inner()),
        owner,
    };
    append_edge_in_tx(tx, &draft).await?;
    Ok(edge_id)
}

pub(super) fn motivated_by_authorship_kind(authorship: &GoalAuthorship) -> EdgeAuthorshipKind {
    match authorship {
        GoalAuthorship::System(SystemOrigin::Operator { .. }) => {
            EdgeAuthorshipKind::OperatorAtoGoal
        }
        GoalAuthorship::User => EdgeAuthorshipKind::User,
        GoalAuthorship::System(SystemOrigin::Tool { .. }) => EdgeAuthorshipKind::Engine,
        GoalAuthorship::External => EdgeAuthorshipKind::ExternalAgent,
    }
}

pub(super) async fn append_motivated_by_edges(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    goal_id: GoalId,
    evidence: &[EvidenceTarget],
    authorship_kind: EdgeAuthorshipKind,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let relation = resolve_relation(context, CORE_MOTIVATED_BY_RELATION)?;
    let mut edge_ids = Vec::with_capacity(evidence.len());
    for target in evidence {
        let edge_id = uuid::Uuid::now_v7();
        let draft = EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Goal,
            source_memory_id: None,
            source_goal_id: Some(goal_id.into_inner()),
            source_fact_entity_id: None,
            target_kind: target.kind,
            target_memory_id: Some(target.memory_id.into_inner()),
            target_goal_id: None,
            target_fact_entity_id: None,
            authorship_kind,
            authorship_owner_memory_id: None,
            owner,
        };
        append_edge_in_tx(tx, &draft).await?;
        edge_ids.push(edge_id);
    }
    Ok(edge_ids)
}

pub(super) async fn append_lifecycle_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    lifecycle_memory_id: MemoryId,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let Some(self_id) = context.author_self_perspective_id else {
        return Ok(None);
    };
    let relation = resolve_relation(context, CORE_AUTHORED_RELATION)?;
    let edge_id = uuid::Uuid::now_v7();
    let draft = EdgeDraft {
        edge_id,
        relation,
        source_kind: EntityKind::Perspective,
        source_memory_id: Some(self_id.into_inner()),
        source_goal_id: None,
        source_fact_entity_id: None,
        target_kind: EntityKind::Fact,
        target_memory_id: Some(lifecycle_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: EdgeAuthorshipKind::Engine,
        authorship_owner_memory_id: None,
        owner,
    };
    append_edge_in_tx(tx, &draft).await?;
    Ok(Some(edge_id))
}

pub(super) async fn append_lifecycle_derived_from_edges(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    lifecycle_memory_id: MemoryId,
    evidence: &[EvidenceTarget],
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let relation = resolve_relation(context, CORE_DERIVED_FROM_RELATION)?;
    let mut edge_ids = Vec::new();
    for target in evidence {
        if target.kind != EntityKind::Fact {
            continue;
        }
        let edge_id = uuid::Uuid::now_v7();
        let draft = EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(lifecycle_memory_id.into_inner()),
            source_goal_id: None,
            source_fact_entity_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(target.memory_id.into_inner()),
            target_goal_id: None,
            target_fact_entity_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner,
        };
        append_edge_in_tx(tx, &draft).await?;
        edge_ids.push(edge_id);
    }
    Ok(edge_ids)
}

pub(super) fn resolve_relation<'a>(
    context: GoalAtomicContext<'a>,
    relation: &str,
) -> Result<RegisteredRelation<'a>, StorageError> {
    context.registry.resolve_relation(relation).ok_or_else(|| {
        StorageError::Internal(format!("relation {relation} not registered in goal atom"))
    })
}
