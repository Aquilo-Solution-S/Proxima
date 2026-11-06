use proxiima_core::StorageError;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::refused;
use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;

/// Finish only after all participating spiices have produced their target
/// records. An incoming pin owned by an omitted spiice is a refusal, never an
/// implicit rewrite of that spiice's payload.
pub(super) async fn finish(
    registry: &PgSidecarRegistryFrozen,
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), StorageError> {
    let memory_ids:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT m.t FROM proxiima_core.memory m /* FORGET-POLICY: includes-forgotten (migration replaces every representation) */
        JOIN proxiima_core.schema_evolution_authority a ON a.source_id=m.t AND a.source_schema=m.schema_id
         AND a.record_kind='memory' AND a.xact=pg_current_xact_id()")
        .fetch_all(&mut **tx).await.map_err(map_err)?;
    let goal_ids:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT g.t FROM proxiima_core.goal g
        JOIN proxiima_core.schema_evolution_authority a ON a.source_id=g.t AND a.source_schema=g.schema_id
         AND a.record_kind='goal' AND a.xact=pg_current_xact_id()")
        .fetch_all(&mut **tx).await.map_err(map_err)?;
    let blob_ids:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT b.blob_id FROM proxiima_core.blob b
        JOIN proxiima_core.schema_evolution_authority a ON a.source_id=b.blob_id AND a.source_schema=b.schema_id
         AND a.record_kind='blob' AND a.xact=pg_current_xact_id()")
        .fetch_all(&mut **tx).await.map_err(map_err)?;
    incoming_pins(tx, &memory_ids, &goal_ids, &blob_ids).await?;
    super::collaboration::preserve(tx).await?;
    remap_uploads(tx).await?;
    remove_source_sidecars(registry, tx, &memory_ids).await?;
    remove_goal_sidecars(registry, tx, &goal_ids).await?;
    sqlx::query("DELETE FROM proxiima_core.sketch WHERE t=ANY($1::uuid[]) OR t=ANY($2::uuid[])")
        .bind(&memory_ids)
        .bind(&goal_ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    let source_content: Vec<Option<Uuid>> = sqlx::query_scalar("DELETE FROM proxiima_core.memory WHERE t=ANY($1::uuid[]) /* FORGET-POLICY: includes-forgotten (explicit mapped representation removal) */ RETURNING content_id")
        .bind(&memory_ids).fetch_all(&mut **tx).await.map_err(map_err)?;
    sqlx::query("DELETE FROM proxiima_core.goal WHERE t=ANY($1::uuid[])")
        .bind(&goal_ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxiima_core.blob WHERE blob_id=ANY($1::uuid[])")
        .bind(&blob_ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    remove_unused_content(
        tx,
        &source_content.into_iter().flatten().collect::<Vec<_>>(),
    )
    .await?;
    sqlx::query("DELETE FROM proxiima_core.memory_head h WHERE EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority a
        WHERE a.xact=pg_current_xact_id() AND a.record_kind='memory' AND a.source_handle=h.handle)
        AND NOT EXISTS (SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: includes-forgotten (historical series stays while any version survives) */ WHERE m.handle=h.handle)")
        .execute(&mut **tx).await.map_err(map_err)?;
    sqlx::query("DELETE FROM proxiima_core.goal_head h WHERE EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority a
        WHERE a.xact=pg_current_xact_id() AND a.record_kind='goal' AND a.source_handle=h.handle)
        AND NOT EXISTS (SELECT 1 FROM proxiima_core.goal g WHERE g.handle=h.handle)")
        .execute(&mut **tx).await.map_err(map_err)?;
    validate_targets(tx).await?;
    super::graph::validate(tx).await?;
    // All deferred sidecar constraints must see the still-live narrow permits.
    // The authority-drain constraint itself is checked after removing permits.
    let constraints = data_constraints(tx, registry).await?;
    enforce_data_constraints(tx, &constraints).await?;
    restore_data_constraints(tx, &constraints).await?;
    sqlx::query(
        "DELETE FROM proxiima_core.schema_evolution_authority WHERE xact=pg_current_xact_id()",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query("SET CONSTRAINTS proxiima_core.schema_evolution_authority_drained IMMEDIATE")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    sqlx::query("SET CONSTRAINTS proxiima_core.schema_evolution_authority_drained DEFERRED")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

/// Reclaim only content consumed by this plan or by its retired source rows.
/// Retained admissions may already point to new content; replacement admissions
/// keep the old content reachable until the source rows are removed at finish.
pub(super) async fn remove_unused_content(
    tx: &mut Transaction<'_, Postgres>,
    content_ids: &[Uuid],
) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM proxiima_core.content c WHERE c.content_id=ANY($1::uuid[])
        AND NOT EXISTS (SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: includes-forgotten (historical content remains reachable) */ WHERE m.content_id=c.content_id)")
        .bind(content_ids).execute(&mut **tx).await.map_err(map_err)?;
    Ok(())
}

pub(super) async fn checkpoint_data_constraints(
    registry: &PgSidecarRegistryFrozen,
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), StorageError> {
    let constraints = data_constraints(tx, registry).await?;
    enforce_data_constraints(tx, &constraints).await?;
    restore_data_constraints(tx, &constraints).await
}

async fn data_constraints(
    tx: &mut Transaction<'_, Postgres>,
    registry: &PgSidecarRegistryFrozen,
) -> Result<Vec<(String, bool)>, StorageError> {
    let mut schemas = registry.spiice_schema_names().to_vec();
    schemas.push("proxiima_core".to_owned());
    sqlx::query_as(
        "SELECT DISTINCT format('%I.%I', n.nspname, c.conname), c.condeferred
           FROM pg_constraint c
           JOIN pg_namespace n ON n.oid=c.connamespace
          WHERE c.condeferrable AND n.nspname=ANY($1::text[])
            AND c.conrelid<>'proxiima_core.schema_evolution_authority'::regclass",
    )
    .bind(schemas)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn enforce_data_constraints(
    tx: &mut Transaction<'_, Postgres>,
    constraints: &[(String, bool)],
) -> Result<(), StorageError> {
    for (qualified, _) in constraints {
        // SQL-POLICY: generated — PostgreSQL format('%I') quotes catalog identifiers.
        let statement = format!("SET CONSTRAINTS {qualified} IMMEDIATE");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
            .execute(&mut **tx)
            .await
            .map_err(|err| refused(format!("migration constraint {qualified}: {err}")))?;
    }
    Ok(())
}

async fn restore_data_constraints(
    tx: &mut Transaction<'_, Postgres>,
    constraints: &[(String, bool)],
) -> Result<(), StorageError> {
    for (qualified, initially_deferred) in constraints {
        let mode = if *initially_deferred {
            "DEFERRED"
        } else {
            "IMMEDIATE"
        };
        // SQL-POLICY: generated — catalog identifiers are quoted by PostgreSQL;
        // mode is selected from the two fixed constraint-timing keywords.
        let statement = format!("SET CONSTRAINTS {qualified} {mode}");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
            .execute(&mut **tx)
            .await
            .map_err(|err| refused(format!("migration constraint {qualified}: {err}")))?;
    }
    Ok(())
}

async fn incoming_pins(
    tx: &mut Transaction<'_, Postgres>,
    memories: &[Uuid],
    goals: &[Uuid],
    blobs: &[Uuid],
) -> Result<(), StorageError> {
    let dangling:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: includes-forgotten (all surviving pins need their target) */
        WHERE m.t<>ALL($1::uuid[]) AND (m.origins&&$1::uuid[] OR m.refs&&$1::uuid[] OR m.forked_from=ANY($1::uuid[])
           OR m.goal_refs&&$2::uuid[] OR m.guided_by&&$2::uuid[] OR m.blob_id=ANY($3::uuid[])))
        OR EXISTS (SELECT 1 FROM proxiima_core.goal g WHERE g.t<>ALL($2::uuid[])
           AND (g.dependency_t&&$2::uuid[] OR g.evidence_t&&$1::uuid[] OR g.motivated_by&&$1::uuid[]
                OR g.close_fact_t=ANY($1::uuid[]) OR g.assignment_t=ANY($1::uuid[]) OR g.write_act_t=ANY($1::uuid[])))")
        .bind(memories).bind(goals).bind(blobs).fetch_one(&mut **tx).await.map_err(map_err)?;
    if dangling {
        return Err(refused(
            "schema migration leaves a source-owned reference unresolved; the owning spiice must supply its reference migration",
        ));
    }
    Ok(())
}

async fn remap_uploads(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxiima_core.blob_uploads upload SET blob_id=a.target_id
        FROM proxiima_core.schema_evolution_authority a
        JOIN proxiima_core.blob target ON target.blob_id=a.target_id
        WHERE a.xact=pg_current_xact_id() AND a.record_kind='blob' AND a.primary_target
          AND upload.blob_id=a.source_id AND upload.owner_id=target.owner_id
          AND upload.content_hash=target.content_hash",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn remove_source_sidecars(
    registry: &PgSidecarRegistryFrozen,
    tx: &mut Transaction<'_, Postgres>,
    memories: &[Uuid],
) -> Result<(), StorageError> {
    let tables:Vec<String>=sqlx::query_scalar("SELECT DISTINCT unnest(sidecar_tables) FROM proxiima_core.memory /* FORGET-POLICY: includes-forgotten (mapped source footprint is removed) */ WHERE t=ANY($1::uuid[])")
        .bind(memories).fetch_all(&mut **tx).await.map_err(map_err)?;
    for table in tables {
        let surface = registry
            .evolutions
            .iter()
            .flat_map(|step| step.sources)
            .flat_map(|schema| schema.surfaces)
            .find(|surface| surface.table == table)
            .ok_or_else(|| {
                refused(format!(
                    "source sidecar {table} lacks an explicit migration surface"
                ))
            })?;
        let proxiima_core::spiice::KeyShape::MemoryT { column } = surface.key else {
            return Err(refused("source sidecar lacks a declared Memory key"));
        };
        let table = crate::pg_ident::PgIdent::table(&table)?;
        let column = crate::pg_ident::PgIdent::column(column)?;
        // SQL-POLICY: PgIdent
        let statement = format!(
            "DELETE FROM {} WHERE {}=ANY($1::uuid[])",
            table.as_str(),
            column.as_str()
        );
        // SQL-POLICY: PgIdent
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
            .bind(memories)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn validate_targets(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    let missing: bool = sqlx::query_scalar("SELECT EXISTS (
        SELECT 1 FROM proxiima_core.schema_evolution_authority a WHERE a.xact=pg_current_xact_id()
          AND NOT EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority next
              WHERE next.xact=a.xact AND next.record_kind=a.record_kind AND next.source_id=a.target_id AND next.source_schema=a.target_schema)
          AND NOT (CASE a.record_kind
            WHEN 'memory' THEN EXISTS (SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: includes-forgotten (terminal migration output) */ WHERE m.t=a.target_id AND m.schema_id=a.target_schema AND m.owner_id=a.record_owner AND m.handle=a.target_handle)
            WHEN 'goal' THEN EXISTS (SELECT 1 FROM proxiima_core.goal g WHERE g.t=a.target_id AND g.schema_id=a.target_schema AND g.owner_id=a.record_owner AND g.handle=a.target_handle)
            WHEN 'blob' THEN EXISTS (SELECT 1 FROM proxiima_core.blob b WHERE b.blob_id=a.target_id AND b.schema_id=a.target_schema AND b.owner_id=a.record_owner)
            ELSE false END))")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if missing {
        return Err(refused(
            "schema migration is missing a declared terminal target",
        ));
    }
    let invalid:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxiima_core.memory m
        /* FORGET-POLICY: includes-forgotten (the persisted target graph includes history) */
        JOIN proxiima_core.schema_evolution_authority a ON a.target_id=m.t AND a.record_kind='memory' AND a.xact=pg_current_xact_id()
        LEFT JOIN proxiima_core.memory_head h ON h.handle=m.handle
        WHERE h.handle IS NULL OR m.owner_id<>h.owner_id OR m.kind<>h.kind OR m.schema_id<>h.schema_id
           OR (m.kind<>'fact' AND NOT EXISTS (SELECT 1 FROM proxiima_core.content c WHERE c.content_id=m.content_id AND c.owner_id=m.owner_id AND c.schema_id=m.schema_id)))
        OR EXISTS (SELECT 1 FROM proxiima_core.goal g JOIN proxiima_core.schema_evolution_authority a
          ON a.target_id=g.t AND a.record_kind='goal' AND a.xact=pg_current_xact_id()
          LEFT JOIN proxiima_core.goal_head h ON h.handle=g.handle WHERE h.handle IS NULL OR h.owner_id<>g.owner_id OR h.schema_id<>g.schema_id)")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if invalid {
        return Err(refused(
            "migration target violates head, owner, or content alignment",
        ));
    }
    let old_rows:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM proxiima_core.memory m JOIN proxiima_core.schema_evolution e ON e.source_schema_id=m.schema_id AND e.source_kind=m.kind::text
          /* FORGET-POLICY: includes-forgotten (no old representation can activate) */)
        OR EXISTS(SELECT 1 FROM proxiima_core.goal g JOIN proxiima_core.schema_evolution e ON e.source_schema_id=g.schema_id AND e.source_kind='goal')
        OR EXISTS(SELECT 1 FROM proxiima_core.blob b JOIN proxiima_core.schema_evolution e ON e.source_schema_id=b.schema_id AND e.source_kind='cited_object')")
        .fetch_one(&mut **tx).await.map_err(map_err)?;
    if old_rows {
        return Err(refused(
            "schema migration did not drain all source representations",
        ));
    }
    Ok(())
}

async fn remove_goal_sidecars(
    registry: &PgSidecarRegistryFrozen,
    tx: &mut Transaction<'_, Postgres>,
    goals: &[Uuid],
) -> Result<(), StorageError> {
    let sources: Vec<(String,Vec<Uuid>)> = sqlx::query_as("SELECT schema_id,array_agg(t) FROM proxiima_core.goal WHERE t=ANY($1::uuid[]) GROUP BY schema_id")
        .bind(goals).fetch_all(&mut **tx).await.map_err(map_err)?;
    for (schema_id, ids) in sources {
        let schema = registry
            .evolutions
            .iter()
            .flat_map(|step| step.sources)
            .find(|s| {
                s.kind == proxiima_core::verbs::schema::PayloadKind::Goal
                    && s.id.render() == schema_id
            })
            .ok_or_else(|| refused("Goal source has no declared migration schema"))?;
        for surface in schema.surfaces {
            let proxiima_core::spiice::KeyShape::GoalT { column } = surface.key else {
                continue;
            };
            let table = crate::pg_ident::PgIdent::table(surface.table)?;
            let column = crate::pg_ident::PgIdent::column(column)?;
            let sql = format!(
                "DELETE FROM {} WHERE {}=ANY($1::uuid[])",
                table.as_str(),
                column.as_str()
            );
            // SQL-POLICY: PgIdent
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(&ids)
                .execute(&mut **tx)
                .await
                .map_err(map_err)?;
        }
    }
    Ok(())
}
