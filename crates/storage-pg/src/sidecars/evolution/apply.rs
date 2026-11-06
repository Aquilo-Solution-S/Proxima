use std::collections::HashMap;

use proxiima_core::{OwnerId, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{MigrationInput, MigrationPlan, PgSchemaEvolution, refused};
use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;

use super::{finish, write};

pub(super) struct PreviousMappings {
    pub admissions: HashMap<Uuid, Vec<Uuid>>,
    pub series: HashMap<Uuid, Vec<Uuid>>,
}

pub(super) async fn previous_mappings(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerId,
    referenced: &[Uuid],
) -> Result<PreviousMappings, StorageError> {
    let rows: Vec<(Uuid, Option<Uuid>, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT source_id, source_handle, target_id, target_handle
           FROM proxiima_core.schema_evolution_terminal_mappings($1, $2::uuid[])",
    )
    .bind(owner.into_inner())
    .bind(referenced)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let mut admissions = HashMap::<Uuid, Vec<Uuid>>::new();
    let mut series = HashMap::<Uuid, Vec<Uuid>>::new();
    for (source, source_series, target, target_series) in rows {
        admissions.entry(source).or_default().push(target);
        if let (Some(source), Some(target)) = (source_series, target_series) {
            series.entry(source).or_default().push(target);
        }
    }
    for targets in admissions.values_mut().chain(series.values_mut()) {
        targets.sort_unstable();
        targets.dedup();
    }
    Ok(PreviousMappings { admissions, series })
}

struct Record {
    kind: &'static str,
    schema: String,
    series: Option<Uuid>,
}
fn source(input: &MigrationInput, id: Uuid) -> Result<Record, StorageError> {
    if let Some(row) = input
        .memories
        .iter()
        .find(|source| source.id().into_inner() == id)
    {
        return Ok(Record {
            kind: "memory",
            schema: row.row.schema_id.clone(),
            series: Some(row.series()),
        });
    }
    if let Some(row) = input
        .goals
        .iter()
        .find(|source| source.id().into_inner() == id)
    {
        return Ok(Record {
            kind: "goal",
            schema: row.schema_id.clone(),
            series: Some(row.series()),
        });
    }
    if let Some(row) = input.blobs.iter().find(|source| source.id() == id) {
        return Ok(Record {
            kind: "blob",
            schema: row.schema_id.clone(),
            series: None,
        });
    }
    Err(refused("migration mapping has no source record"))
}
fn target(plan: &MigrationPlan, id: Uuid) -> Result<Record, StorageError> {
    if let Some(row) = plan.memories.iter().find(|target| target.id == id) {
        return Ok(Record {
            kind: "memory",
            schema: row.payload.schema_id.as_str().to_owned(),
            series: Some(row.series),
        });
    }
    if let Some(row) = plan.goals.iter().find(|target| target.id == id) {
        return Ok(Record {
            kind: "goal",
            schema: row.payload.schema_id.as_str().to_owned(),
            series: Some(row.series),
        });
    }
    if let Some(row) = plan.blobs.iter().find(|target| target.id == id) {
        return Ok(Record {
            kind: "blob",
            schema: row.schema.render(),
            series: None,
        });
    }
    Err(refused("migration mapping has no target record"))
}

pub(super) async fn plan(
    registry: &PgSidecarRegistryFrozen,
    program: &PgSchemaEvolution,
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    for (id, mapping) in &plan.mappings {
        let old = source(input, *id)?;
        for target_id in mapping.targets() {
            let new = target(plan, target_id)?;
            sqlx::query("INSERT INTO proxiima_core.schema_evolution_authority
                (record_kind,source_id,target_id,source_schema,target_schema,source_handle,target_handle,record_owner,primary_target)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                .bind(old.kind).bind(id).bind(target_id).bind(&old.schema).bind(new.schema)
                .bind(old.series).bind(new.series).bind(input.owner.into_inner()).bind(target_id==mapping.primary)
                .execute(&mut **tx).await.map_err(map_err)?;
        }
    }
    write::targets(registry, program, tx, input, plan).await?;
    memory_replay(tx, input, plan).await?;
    goal_replay(tx, input, plan).await?;
    publications(tx, input, plan).await?;
    super::derived::rebuild(tx, input, plan, program).await?;
    let source_content: Vec<_> = input
        .memories
        .iter()
        .filter_map(|source| source.row.content_id)
        .collect();
    finish::remove_unused_content(tx, &source_content).await?;
    Ok(())
}

async fn memory_replay(
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    for source in &input.memories {
        let mapping = &plan.mappings[&source.id().into_inner()];
        sqlx::query("UPDATE proxiima_core.ingest_keys SET t=$1 WHERE t=$2 AND owner_id=$3")
            .bind(mapping.primary)
            .bind(source.id().into_inner())
            .bind(input.owner.into_inner())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    for target in &plan.memories {
        if let (Some(source), Some(key)) = (&target.source.row.source_id, &target.ingest_key) {
            claim_key(tx, input.owner, source, key, target.id).await?;
        }
    }
    for (source, old, new) in &plan.fingerprints {
        if !source.starts_with("core/key/") || !old.starts_with("f:") || !new.starts_with("f:") {
            return Err(refused(
                "a request fingerprint migration must name the request ledger's f: keys",
            ));
        }
        let id:Option<Uuid>=sqlx::query_scalar("SELECT t FROM proxiima_core.ingest_keys WHERE owner_id=$1 AND source_id=$2 AND ingest_key=$3")
            .bind(input.owner.into_inner()).bind(source).bind(old).fetch_optional(&mut **tx).await.map_err(map_err)?;
        let id = id.ok_or_else(|| refused("declared old request fingerprint is absent"))?;
        if !plan.memories.iter().any(|target| target.id == id) {
            return Err(refused(
                "request fingerprint names a record outside this migration",
            ));
        }
        claim_key(tx, input.owner, source, new, id).await?;
    }
    Ok(())
}

async fn claim_key(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerId,
    source: &str,
    key: &str,
    target: Uuid,
) -> Result<(), StorageError> {
    sqlx::query("INSERT INTO proxiima_core.ingest_keys(owner_id,source_id,ingest_key,t) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(owner.into_inner()).bind(source).bind(key).bind(target).execute(&mut **tx).await.map_err(map_err)?;
    let stored:Uuid=sqlx::query_scalar("SELECT t FROM proxiima_core.ingest_keys WHERE owner_id=$1 AND source_id=$2 AND ingest_key=$3")
        .bind(owner.into_inner()).bind(source).bind(key).fetch_one(&mut **tx).await.map_err(map_err)?;
    if stored != target {
        return Err(refused(
            "migration replay key collides with another admission",
        ));
    }
    Ok(())
}

async fn goal_replay(
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    let map = |id: Uuid| -> Result<Uuid, StorageError> {
        if let Some(mapping) = plan.mappings.get(&id) {
            return Ok(mapping.primary);
        }
        input.mapped_one(id)
    };
    for source in &input.goals {
        let mapping = &plan.mappings[&source.id().into_inner()];
        let target = plan
            .goals
            .iter()
            .find(|target| target.id == mapping.primary)
            .ok_or_else(|| refused("missing Goal replay target"))?;
        let declarations:Vec<(String,serde_json::Value)>=sqlx::query_as("SELECT request_id,declaration FROM proxiima_core.goal_replay_declaration WHERE owner_id=$1 AND goal_t=$2")
            .bind(input.owner.into_inner()).bind(source.id().into_inner()).fetch_all(&mut **tx).await.map_err(map_err)?;
        for (request, declaration) in declarations {
            let rewritten = crate::verbs::goal_write::rewrite_goal_replay(
                declaration,
                &target.payload,
                &map,
                &map,
            )?;
            sqlx::query("UPDATE proxiima_core.goal_replay_declaration SET goal_t=$1,declaration=$2 WHERE owner_id=$3 AND request_id=$4")
                .bind(target.id).bind(rewritten).bind(input.owner.into_inner()).bind(request).execute(&mut **tx).await.map_err(map_err)?;
        }
    }
    Ok(())
}

async fn publications(
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
) -> Result<(), StorageError> {
    for source in &input.memories {
        let mapping = &plan.mappings[&source.id().into_inner()];
        for target in mapping.targets() {
            sqlx::query("INSERT INTO proxiima_core.schema_evolution_publication(publication_t,memory_t)
                SELECT t,$1 FROM proxiima_core.publication_origin WHERE t=$2
                UNION SELECT publication_t,$1 FROM proxiima_core.schema_evolution_publication WHERE memory_t=$2
                ON CONFLICT DO NOTHING")
                .bind(target).bind(source.id().into_inner()).execute(&mut **tx).await.map_err(map_err)?;
        }
    }
    Ok(())
}

pub(super) async fn record_completion(
    tx: &mut Transaction<'_, Postgres>,
    program: &PgSchemaEvolution,
) -> Result<(), StorageError> {
    let targets: Vec<_> = program
        .declaration
        .targets
        .iter()
        .map(|schema| schema.id.render())
        .collect();
    let kinds: Vec<_> = program
        .declaration
        .targets
        .iter()
        .map(|s| super::kind_name(s.kind))
        .collect();
    for source in program.declaration.sources {
        sqlx::query("INSERT INTO proxiima_core.schema_evolution(source_schema_id,evolution_id,spiice_id,target_schema_ids,source_kind,target_kinds) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(source.id.render()).bind(program.declaration.id).bind(source.id.spiice).bind(&targets)
            .bind(super::kind_name(source.kind)).bind(&kinds)
            .execute(&mut **tx).await.map_err(map_err)?;
    }
    Ok(())
}

pub(super) async fn finish(
    registry: &PgSidecarRegistryFrozen,
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), StorageError> {
    finish::finish(registry, tx).await
}
