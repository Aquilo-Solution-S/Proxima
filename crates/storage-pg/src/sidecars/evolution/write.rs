use std::collections::HashMap;

use proxiima_core::{MemoryId, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::target_rows;
use super::{MigrationInput, MigrationPlan, PgSchemaEvolution, refused};
use crate::error::map_err;
use crate::pg_enums::{PgMemoryKind, PgSketchKind};
use crate::pg_ident::PgIdent;
use crate::sidecars::{PgSidecarEntry, PgSidecarRegistryFrozen};

/// Materialize one owner's target records after the authority rows for the
/// plan have been installed in the current transaction.
pub(super) async fn targets(
    registry: &PgSidecarRegistryFrozen,
    program: &PgSchemaEvolution,
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    materialize_memory_heads(tx, input, plan).await?;
    materialize_goal_heads(tx, input, plan).await?;

    memory_targets(registry, program, tx, input, plan).await?;
    goal_targets(registry, program, tx, input, plan).await?;

    let mut blobs: Vec<_> = plan.blobs.iter().collect();
    blobs.sort_by_key(|target| target.id);
    for target in blobs {
        target_rows::blob(tx, target).await?;
    }
    Ok(())
}

async fn memory_targets(
    registry: &PgSidecarRegistryFrozen,
    program: &PgSchemaEvolution,
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    let mut memories: Vec<_> = plan.memories.iter().collect();
    memories.sort_by_key(|target| target.id);
    for target in memories {
        let entry = memory_entry(registry, program, &target.payload)?;
        let tables = vec![entry.sidecar_table.clone()];
        target_rows::memory(tx, target, &tables).await?;
        if target.id == target.source.id().into_inner() {
            remove_replaced_memory_sidecars(tx, registry, program, target).await?;
        }
        registry
            .insert_migration_memory_sidecar_entry(
                tx,
                MemoryId::new(target.id),
                &target.payload,
                entry,
            )
            .await?;
        remove_stale_projection(
            tx,
            target.id,
            target.payload.schema_id.as_str(),
            entry.projection_table.as_deref(),
        )
        .await?;
        // Forget keeps the typed sidecar for restore, but its derived search
        // projection is intentionally absent.  Do not recreate that state
        // while replaying the forgotten source row into a new schema.
        if target.source.row.forgotten_at.is_none() && entry.projection_insert.is_some() {
            registry
                .rebuild_projection_for_table(
                    tx,
                    MemoryId::new(target.id),
                    &entry.sidecar_table,
                    target.payload.schema_id.as_str(),
                    Some(target.source.row.lexical_language.as_str()),
                )
                .await?;
        }
        let sketch = crate::verbs::sketch::sketch_line(
            PgSketchKind::from(target.source.kind()),
            None,
            std::slice::from_ref(&target.payload),
        );
        crate::verbs::sketch::upsert_sketch(
            tx,
            input.owner.into_inner(),
            target.id,
            PgSketchKind::from(target.source.kind()),
            &sketch,
        )
        .await?;
    }

    Ok(())
}

async fn goal_targets(
    registry: &PgSidecarRegistryFrozen,
    program: &PgSchemaEvolution,
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    let mut goals: Vec<_> = plan.goals.iter().collect();
    goals.sort_by_key(|target| target.id);
    for target in goals {
        if target.id == target.source.id().into_inner() {
            remove_replaced_goal_sidecars(tx, program, target).await?;
        }
        target_rows::goal(tx, target).await?;
        if let Some(payload) = target.payload.sidecar_payload.as_ref() {
            let entry = memory_entry(registry, program, payload)?;
            registry
                .insert_migration_goal_sidecar_entry(
                    tx,
                    proxiima_core::GoalId::new(target.id),
                    payload,
                    entry,
                )
                .await?;
            let sketch = crate::verbs::sketch::sketch_line(
                PgSketchKind::Goal,
                Some(target.payload.text.as_str()),
                std::slice::from_ref(payload),
            );
            crate::verbs::sketch::upsert_sketch(
                tx,
                input.owner.into_inner(),
                target.id,
                PgSketchKind::Goal,
                &sketch,
            )
            .await?;
        } else {
            crate::verbs::sketch::upsert_sketch(
                tx,
                input.owner.into_inner(),
                target.id,
                PgSketchKind::Goal,
                &crate::verbs::sketch::sketch_line(
                    PgSketchKind::Goal,
                    Some(target.payload.text.as_str()),
                    &[],
                ),
            )
            .await?;
        }
    }

    Ok(())
}

async fn materialize_memory_heads(
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    let mut groups: HashMap<Uuid, (PgMemoryKind, String, i64, i64)> = HashMap::new();
    let mut targets: Vec<_> = plan.memories.iter().collect();
    targets.sort_by_key(|target| target.id);
    for target in targets {
        let kind = PgMemoryKind::of(target.source.kind())
            .ok_or_else(|| refused("Goal cannot materialize as a memory head"))?;
        let schema = target.payload.schema_id.as_str().to_owned();
        let source_head: Option<(i64, i64)> = sqlx::query_as(
            "SELECT authority_generation, publication_sequence
               FROM proxiima_core.memory_head
              WHERE handle=$1 AND owner_id=$2",
        )
        .bind(target.source.series())
        .bind(input.owner.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        let target_head: Option<(i64, i64, String, PgMemoryKind, Uuid)> = sqlx::query_as(
            "SELECT authority_generation, publication_sequence, schema_id, kind, owner_id
               FROM proxiima_core.memory_head WHERE handle=$1",
        )
        .bind(target.series)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        if let Some((_, _, existing_schema, existing_kind, existing_owner)) = &target_head {
            require_participating_series(tx, "memory", target.series).await?;
            if *existing_kind != kind || *existing_owner != input.owner.into_inner() {
                return Err(refused(
                    "migration target conflicts with an unrelated memory series",
                ));
            }
            if existing_schema != &schema && target.source.series() != target.series {
                return Err(refused(
                    "migration target schema conflicts with an existing memory series",
                ));
            }
        }
        let (source_generation, source_publication) = source_head.unwrap_or((0, 0));
        let (existing_generation, existing_publication) =
            target_head.map_or((0, 0), |head| (head.0, head.1));
        let entry = groups
            .entry(target.series)
            .or_insert((kind, schema.clone(), 0, 0));
        if entry.0 != kind || entry.1 != schema {
            return Err(refused("one target memory series has mixed kind or schema"));
        }
        entry.2 = entry.2.max(source_generation).max(existing_generation);
        entry.3 = entry.3.max(source_publication).max(existing_publication);
    }
    for (handle, (kind, schema, generation, publication)) in groups {
        sqlx::query(
            "INSERT INTO proxiima_core.memory_head
                (handle, kind, schema_id, owner_id, t, authority_generation, publication_sequence)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (handle) DO UPDATE SET
                kind=EXCLUDED.kind, schema_id=EXCLUDED.schema_id, owner_id=EXCLUDED.owner_id,
                t=GREATEST(proxiima_core.memory_head.t, EXCLUDED.t),
                authority_generation=GREATEST(proxiima_core.memory_head.authority_generation, EXCLUDED.authority_generation),
                publication_sequence=GREATEST(proxiima_core.memory_head.publication_sequence, EXCLUDED.publication_sequence)",
        )
        .bind(handle)
        .bind(kind)
        .bind(schema)
        .bind(input.owner.into_inner())
        .bind(head_t(plan, handle)?)
        .bind(generation)
        .bind(publication)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

async fn materialize_goal_heads(
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    let mut groups: HashMap<Uuid, (String, i64)> = HashMap::new();
    let mut targets: Vec<_> = plan.goals.iter().collect();
    targets.sort_by_key(|target| target.id);
    for target in targets {
        let schema = target.payload.schema_id.as_str().to_owned();
        let source_generation: Option<i64> = sqlx::query_scalar(
            "SELECT authority_generation FROM proxiima_core.goal_head WHERE handle=$1 AND owner_id=$2",
        )
        .bind(target.source.series())
        .bind(input.owner.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        let target_head: Option<(i64, String, Uuid)> = sqlx::query_as(
            "SELECT authority_generation, schema_id, owner_id FROM proxiima_core.goal_head WHERE handle=$1",
        )
        .bind(target.series)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        if let Some((_, existing_schema, existing_owner)) = &target_head {
            require_participating_series(tx, "goal", target.series).await?;
            if *existing_owner != input.owner.into_inner() {
                return Err(refused(
                    "migration target conflicts with an unrelated Goal series",
                ));
            }
            if existing_schema != &schema && target.source.series() != target.series {
                return Err(refused(
                    "migration target schema conflicts with an existing Goal series",
                ));
            }
        }
        let entry = groups.entry(target.series).or_insert((schema.clone(), 0));
        if entry.0 != schema {
            return Err(refused("one target Goal series has mixed schemas"));
        }
        entry.1 = entry
            .1
            .max(source_generation.unwrap_or(0))
            .max(target_head.map_or(0, |head| head.0));
    }
    for (handle, (schema, generation)) in groups {
        sqlx::query(
            "INSERT INTO proxiima_core.goal_head (handle, schema_id, owner_id, t, authority_generation)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (handle) DO UPDATE SET
                schema_id=EXCLUDED.schema_id, owner_id=EXCLUDED.owner_id,
                t=GREATEST(proxiima_core.goal_head.t, EXCLUDED.t),
                authority_generation=GREATEST(proxiima_core.goal_head.authority_generation, EXCLUDED.authority_generation)",
        )
        .bind(handle)
        .bind(schema)
        .bind(input.owner.into_inner())
        .bind(goal_head_t(plan, handle)?)
        .bind(generation)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

async fn require_participating_series(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    series: Uuid,
) -> Result<(), StorageError> {
    let participating: bool = sqlx::query_scalar(
        "SELECT EXISTS (
        SELECT 1 FROM proxiima_core.schema_evolution_authority WHERE xact=pg_current_xact_id()
          AND record_kind=$1 AND source_handle=$2)",
    )
    .bind(kind)
    .bind(series)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if !participating {
        return Err(refused(
            "migration cannot join a series whose existing admissions are outside its source mapping",
        ));
    }
    Ok(())
}

fn head_t(plan: &MigrationPlan, handle: Uuid) -> Result<Uuid, StorageError> {
    plan.memories
        .iter()
        .filter(|target| target.series == handle)
        .map(|target| target.id)
        .max()
        .ok_or_else(|| refused("memory target series has no target row"))
}

fn goal_head_t(plan: &MigrationPlan, handle: Uuid) -> Result<Uuid, StorageError> {
    plan.goals
        .iter()
        .filter(|target| target.series == handle)
        .map(|target| target.id)
        .max()
        .ok_or_else(|| refused("Goal target series has no target row"))
}

fn memory_entry<'a>(
    registry: &'a PgSidecarRegistryFrozen,
    program: &'a PgSchemaEvolution,
    payload: &proxiima_core::SidecarPayload,
) -> Result<&'a PgSidecarEntry, StorageError> {
    let key = crate::sidecars::PgSidecarKey::new(
        payload.kind,
        payload.schema_id.clone(),
        payload.schema_version,
    );
    registry
        .entries
        .get(&key)
        .or_else(|| program.schemas.registry.entries.get(&key))
        .ok_or_else(|| refused("migration target has no typed sidecar entry"))
}

async fn remove_replaced_goal_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    program: &PgSchemaEvolution,
    target: &super::plan::GoalTarget,
) -> Result<(), StorageError> {
    let source = program
        .declaration
        .sources
        .iter()
        .find(|schema| schema.schema_id().as_str() == target.source.schema_id())
        .ok_or_else(|| refused("retained Goal source has no declared contract"))?;
    for surface in source.surfaces {
        let (column, value) = match surface.key {
            proxiima_core::spiice::KeyShape::GoalT { column } => (column, target.id),
            proxiima_core::spiice::KeyShape::GoalHandle { column, .. } => (column, target.series),
            _ => continue,
        };
        let table = PgIdent::table(surface.table)?;
        let column = PgIdent::column(column)?;
        // SQL-POLICY: source surfaces and key columns are frozen contracts.
        let sql = format!(
            "DELETE FROM {} WHERE {}=$1",
            table.as_str(),
            column.as_str()
        );
        // SQL-POLICY: PgIdent
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(value)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn remove_replaced_memory_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    registry: &PgSidecarRegistryFrozen,
    program: &PgSchemaEvolution,
    target: &super::plan::MemoryTarget,
) -> Result<(), StorageError> {
    for table in &target.source.row.sidecar_tables {
        let table = PgIdent::table(table)?;
        let column_name = registry
            .memory_key_column(table.as_str())
            .or_else(|| {
                program
                    .schemas
                    .registry
                    .entries
                    .values()
                    .find(|entry| entry.sidecar_table == table.as_str())
                    .and_then(|entry| entry.memory_key_column)
            })
            .ok_or_else(|| refused("source sidecar has no typed memory key column"))?;
        let column = PgIdent::column(column_name)?;
        let sql = format!(
            "DELETE FROM {} WHERE {}=$1",
            table.as_str(),
            column.as_str()
        );
        // SQL-POLICY: PgIdent
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(target.id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn remove_stale_projection(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: Uuid,
    schema_id: &str,
    projection_table: Option<&str>,
) -> Result<(), StorageError> {
    let Some(projection_table) = projection_table else {
        return Ok(());
    };
    let table = PgIdent::table(projection_table)?;
    // SQL-POLICY: projection table is a frozen contract identifier.
    let sql = format!(
        "DELETE FROM {} WHERE memory_id=$1 AND schema_id<>$2",
        table.as_str()
    );
    // SQL-POLICY: PgIdent
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(memory_id)
        .bind(schema_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}
