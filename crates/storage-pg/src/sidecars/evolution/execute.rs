use std::collections::HashSet;

use proxiima_core::spiice::SchemaEvolution;
use proxiima_core::{OwnerId, StorageError};
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use super::{MigrationInput, MigrationPlan, read_sources, refused};
use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarRegistryFrozen};

use super::{apply, validate};

pub(super) fn check_codecs(
    declaration: &SchemaEvolution,
    schemas: &super::PgMigrationSchemas,
) -> Result<(), StorageError> {
    for schema in declaration.sources.iter().chain(declaration.targets) {
        if let Some(table) = schema.sidecar_table {
            let key = PgSidecarKey::new(schema.kind, schema.schema_id(), schema.schema_version());
            let entry = schemas.registry.entries.get(&key).ok_or_else(|| {
                refused(format!(
                    "evolution {} lacks the typed sidecar for {}",
                    declaration.id,
                    schema.id.render()
                ))
            })?;
            if entry.sidecar_table != table
                || entry.natural_key_declaration != schema.natural_key_columns
            {
                return Err(refused(format!(
                    "evolution {} sidecar disagrees with {}",
                    declaration.id,
                    schema.id.render()
                )));
            }
            let surface = schema
                .surfaces
                .iter()
                .find(|surface| surface.table == table)
                .ok_or_else(|| {
                    refused(format!(
                        "migration schema {} must declare its sidecar surface {table}",
                        schema.id.render()
                    ))
                })?;
            match surface.key {
                proxiima_core::spiice::KeyShape::MemoryT { column }
                    if entry.memory_key_column == Some(column) => {}
                proxiima_core::spiice::KeyShape::GoalT { .. }
                    if schema.kind == proxiima_core::verbs::schema::PayloadKind::Goal => {}
                _ => {
                    return Err(refused(
                        "migration sidecar key does not match its typed registration",
                    ));
                }
            }
        }
    }
    for source in declaration.sources {
        if source.kind == proxiima_core::verbs::schema::PayloadKind::Goal
            && source.sidecar_table.is_some()
        {
            let key = PgSidecarKey::new(source.kind, source.schema_id(), source.schema_version());
            if !schemas.goal_loaders.contains_key(&key) {
                return Err(refused(format!(
                    "Goal migration source {} has no typed loader",
                    source.id.render()
                )));
            }
        }
    }
    Ok(())
}

type Selector = (proxiima_core::verbs::schema::PayloadKind, String);

#[derive(sqlx::FromRow)]
struct StoredCompletion {
    source_kind: String,
    source_schema_id: String,
    evolution_id: String,
    spiice_id: String,
    target_kinds: Vec<String>,
    target_schema_ids: Vec<String>,
}

struct Completion {
    source: Selector,
    evolution_id: String,
    spiice_id: String,
    targets: HashSet<Selector>,
}

fn parse_kind(value: &str) -> Result<proxiima_core::verbs::schema::PayloadKind, StorageError> {
    use proxiima_core::verbs::schema::PayloadKind;
    match value {
        "fact" => Ok(PayloadKind::Fact),
        "abstraction" => Ok(PayloadKind::Abstraction),
        "perspective" => Ok(PayloadKind::Perspective),
        "goal" => Ok(PayloadKind::Goal),
        "cited_object" => Ok(PayloadKind::CitedObject),
        _ => Err(refused(format!("unknown persisted evolution kind {value}"))),
    }
}

async fn completions(connection: &mut PgConnection) -> Result<Vec<Completion>, StorageError> {
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('proxiima_core.schema_evolution') IS NOT NULL")
            .fetch_one(&mut *connection)
            .await
            .map_err(map_err)?;
    if !present {
        return Ok(Vec::new());
    }
    let rows: Vec<StoredCompletion> = sqlx::query_as("SELECT source_kind,source_schema_id,evolution_id,spiice_id,target_kinds,target_schema_ids FROM proxiima_core.schema_evolution")
        .fetch_all(&mut *connection).await.map_err(map_err)?;
    rows.into_iter()
        .map(|row| {
            if row.target_kinds.len() != row.target_schema_ids.len() || row.target_kinds.is_empty()
            {
                return Err(refused("invalid persisted evolution targets"));
            }
            let targets = row
                .target_kinds
                .into_iter()
                .zip(row.target_schema_ids)
                .map(|(kind, id)| Ok((parse_kind(&kind)?, id)))
                .collect::<Result<HashSet<_>, StorageError>>()?;
            Ok(Completion {
                source: (parse_kind(&row.source_kind)?, row.source_schema_id),
                evolution_id: row.evolution_id,
                spiice_id: row.spiice_id,
                targets,
            })
        })
        .collect()
}

fn pending_steps(
    registry: &PgSidecarRegistryFrozen,
    completed: &[Completion],
) -> Result<Vec<SchemaEvolution>, StorageError> {
    if completed
        .iter()
        .any(|row| registry.active_schema_ids.contains(&row.source))
    {
        return Err(refused(
            "this composition serves an already migrated source schema",
        ));
    }
    let mut pending = Vec::new();
    for declaration in registry.evolutions.iter() {
        let expected: HashSet<_> = declaration
            .targets
            .iter()
            .map(|s| (s.kind, s.id.render()))
            .collect();
        let mut recorded = 0;
        for source in declaration.sources {
            if let Some(row) = completed
                .iter()
                .find(|row| row.source == (source.kind, source.id.render()))
            {
                if row.evolution_id != declaration.id
                    || row.spiice_id != source.id.spiice
                    || row.targets != expected
                {
                    return Err(refused(format!(
                        "completed schema evolution {} changed",
                        declaration.id
                    )));
                }
                recorded += 1;
            }
        }
        if recorded == 0 {
            pending.push(*declaration);
        } else if recorded != declaration.sources.len() {
            return Err(refused("partially recorded schema evolution"));
        }
    }
    Ok(pending)
}

pub(super) async fn pending(
    registry: &PgSidecarRegistryFrozen,
    connection: &mut PgConnection,
) -> Result<Vec<SchemaEvolution>, StorageError> {
    pending_steps(registry, &completions(connection).await?)
}

pub(super) async fn ensure(
    registry: &PgSidecarRegistryFrozen,
    connection: &mut PgConnection,
) -> Result<(), StorageError> {
    let completed = completions(connection).await?;
    let pending = pending_steps(registry, &completed)?;
    if let Some(step) = pending.first() {
        return Err(refused(format!(
            "schema evolution {} is not complete; run migrations before serving",
            step.id
        )));
    }
    let consumed: HashSet<_> = completed.iter().map(|row| &row.source).collect();
    for target in completed.iter().flat_map(|row| &row.targets) {
        if !consumed.contains(target) && !registry.active_schema_ids.contains(target) {
            return Err(refused(format!(
                "completed schema evolution targets unavailable {} schema {}",
                target.0, target.1
            )));
        }
    }
    Ok(())
}

/// Fence source writes in the same order as Goal persistence: a head first,
/// then its revision. Table locks precede the schema advisory locks because
/// an INSERT acquires its table lock before entering the schema trigger.
pub(super) async fn lock_sources(
    tx: &mut Transaction<'_, Postgres>,
    source_ids: &[String],
) -> Result<(), StorageError> {
    sqlx::query("LOCK TABLE proxiima_core.memory, proxiima_core.goal_head, proxiima_core.goal, proxiima_core.blob IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut **tx).await.map_err(map_err)?;
    let mut locked = source_ids.to_vec();
    locked.sort_unstable();
    for schema in locked {
        sqlx::query("SELECT proxiima_core.lock_schema_evolution_exclusive($1)")
            .bind(schema)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

pub(super) async fn run(
    registry: &PgSidecarRegistryFrozen,
    tx: &mut Transaction<'_, Postgres>,
    declaration: &SchemaEvolution,
) -> Result<(), StorageError> {
    let program = registry
        .evolution_programs
        .get(declaration.id)
        .ok_or_else(|| refused(format!("missing executable evolution {}", declaration.id)))?;
    if program.declaration != declaration {
        return Err(refused(
            "migration runner supplied a different evolution declaration",
        ));
    }
    let source_ids: Vec<_> = declaration
        .sources
        .iter()
        .map(|schema| schema.id.render())
        .collect();
    let source_kinds: Vec<_> = declaration
        .sources
        .iter()
        .map(|schema| super::kind_name(schema.kind))
        .collect();
    let completed = completions(&mut *tx).await?;
    if !pending_steps(registry, &completed)?
        .iter()
        .any(|step| step.id == declaration.id)
    {
        return Ok(());
    }
    lock_sources(tx, &source_ids).await?;
    let owners: Vec<Uuid> = sqlx::query_scalar(
        "WITH source_pairs(schema_id, kind) AS (
             SELECT * FROM unnest($1::text[], $2::text[])
         )
         SELECT m.owner_id
           FROM proxiima_core.memory m
           JOIN source_pairs s ON s.schema_id = m.schema_id AND s.kind = m.kind::text
          /* FORGET-POLICY: includes-forgotten (all persisted representations migrate) */
         UNION
         SELECT g.owner_id
           FROM proxiima_core.goal g
           JOIN source_pairs s ON s.schema_id = g.schema_id AND s.kind = 'goal'
         UNION
         SELECT b.owner_id
           FROM proxiima_core.blob b
           JOIN source_pairs s ON s.schema_id = b.schema_id AND s.kind = 'cited_object'
         ORDER BY owner_id",
    )
    .bind(&source_ids)
    .bind(&source_kinds)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    if owners.is_empty() {
        return apply::record_completion(tx, program).await;
    }
    let store_number: i32 =
        sqlx::query_scalar("SELECT store_number FROM proxiima_core.store_identity WHERE singleton")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_err)?;
    let store_number = u16::try_from(store_number)
        .map_err(|_| refused("invalid store number during schema evolution"))?;
    for owner in owners {
        let owner = OwnerId::new(owner);
        let (memories, goals, blobs) = read_sources(tx, program, owner).await?;
        let mut referenced = Vec::new();
        for memory in &memories {
            referenced.extend(&memory.row.origins);
            referenced.extend(&memory.row.refs);
            referenced.extend(&memory.row.goal_refs);
            referenced.extend(&memory.row.guided_by);
            referenced.extend(memory.row.forked_from);
        }
        for goal in &goals {
            referenced.extend(&goal.dependency_t);
            referenced.extend(&goal.evidence_t);
            referenced.extend(&goal.motivated_by);
            referenced.extend(goal.close_fact_t);
            referenced.extend(goal.assignment_t);
            referenced.extend(goal.write_act_t);
        }
        let mappings = apply::previous_mappings(tx, owner, &referenced).await?;
        let input = MigrationInput {
            owner,
            memories,
            goals,
            blobs,
            mapped: mappings.admissions,
            mapped_series: mappings.series,
        };
        let mut plan = MigrationPlan::new(owner, store_number);
        (program.transform)(&input, &mut plan)?;
        validate::plan(program, &input, &plan)?;
        apply::plan(registry, program, tx, &input, &plan).await?;
    }
    apply::record_completion(tx, program).await
}

pub(super) async fn finish(
    registry: &PgSidecarRegistryFrozen,
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), StorageError> {
    apply::finish(registry, tx).await?;
    ensure(registry, &mut *tx).await
}
