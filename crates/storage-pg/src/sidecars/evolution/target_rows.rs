use proxiima_core::{EntityKind, EntityRef, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::plan::{BlobTarget, GoalTarget, MemoryTarget};
use crate::error::map_err;
use crate::pg_enums::PgMemoryKind;

fn memory_refs(target: &MemoryTarget) -> Result<(Vec<Uuid>, Vec<Uuid>), StorageError> {
    let mut refs = target
        .pins
        .references
        .iter()
        .map(|id| id.into_inner())
        .collect::<Vec<_>>();
    let mut goal_refs = target
        .pins
        .goal_references
        .iter()
        .map(|id| id.into_inner())
        .collect::<Vec<_>>();
    for reference in target.payload.references() {
        reference
            .target
            .validate_shape()
            .map_err(StorageError::ConstraintViolation)?;
        let (ids, id) = match reference.target.entity {
            EntityRef::Memory(id) => (&mut refs, id.into_inner()),
            EntityRef::Goal(id) => (&mut goal_refs, id.into_inner()),
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok((refs, goal_refs))
}

async fn content_id(
    tx: &mut Transaction<'_, Postgres>,
    target: &MemoryTarget,
) -> Result<Option<Uuid>, StorageError> {
    if matches!(target.source.kind(), EntityKind::Fact) {
        return Ok(None);
    }
    let hash = crate::verbs::content::hash_sidecar_payloads(std::slice::from_ref(&target.payload))?;
    let kind = PgMemoryKind::of(target.source.kind()).ok_or_else(|| {
        StorageError::ConstraintViolation("schema migration target is not a memory".into())
    })?;
    Ok(Some(
        crate::verbs::content::ensure_content(
            tx,
            target.source.owner().into_inner(),
            target.payload.schema_id.as_str(),
            &crate::verbs::content::kind_scoped_hash(kind.label(), &hash),
        )
        .await?,
    ))
}

pub(super) async fn memory(
    tx: &mut Transaction<'_, Postgres>,
    target: &MemoryTarget,
    sidecar_tables: &[String],
) -> Result<(), StorageError> {
    let (refs, goal_refs) = memory_refs(target)?;
    let content_id = content_id(tx, target).await?;
    let source = &target.source.row;
    let kind = PgMemoryKind::of(source.kind).ok_or_else(|| {
        StorageError::ConstraintViolation("schema migration target is not a memory".into())
    })?;
    let origins: Vec<_> = target
        .pins
        .origins
        .iter()
        .map(|id| id.into_inner())
        .collect();
    let guided_by: Vec<_> = target
        .pins
        .guided_by
        .iter()
        .map(|id| id.into_inner())
        .collect();
    if target.id == source.t.into_inner() {
        sqlx::query(
            "UPDATE proxiima_core.memory
                SET handle=$1, schema_id=$2, source_id=$3, ingest_key=$4, blob_id=$5, content_id=$6,
                    origins=$7, refs=$8, goal_refs=$9, guided_by=$10, sidecar_tables=$11,
                    forgotten_at=$12, forked_from=$15
              WHERE t=$13 AND owner_id=$14",
        )
        .bind(target.series)
        .bind(target.payload.schema_id.as_str())
        .bind(source.source_id.as_deref())
        .bind(target.ingest_key.as_deref())
        .bind(target.pins.blob_id)
        .bind(content_id)
        .bind(&origins)
        .bind(&refs)
        .bind(&goal_refs)
        .bind(&guided_by)
        .bind(sidecar_tables)
        .bind(source.forgotten_at)
        .bind(target.id)
        .bind(source.owner_id.into_inner())
        .bind(
            target
                .pins
                .forked_from
                .map(proxiima_core::MemoryId::into_inner),
        )
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    } else {
        sqlx::query(
            "INSERT INTO proxiima_core.memory
                (handle,t,kind,owner_id,schema_id,source_id,ingest_key,blob_id,content_id,
                 origins,refs,goal_refs,guided_by,sidecar_tables,occurred_at,\"by\",via,
                 forked_from,lexical_language,forgotten_at)
             VALUES ($1,$2,$3::proxiima_core.memory_kind,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,
                     $14,$15,$16,$17,$18,$19::text::regconfig,$20)",
        )
        .bind(target.series)
        .bind(target.id)
        .bind(kind)
        .bind(source.owner_id.into_inner())
        .bind(target.payload.schema_id.as_str())
        .bind(source.source_id.as_deref())
        .bind(target.ingest_key.as_deref())
        .bind(target.pins.blob_id)
        .bind(content_id)
        .bind(&origins)
        .bind(&refs)
        .bind(&goal_refs)
        .bind(&guided_by)
        .bind(sidecar_tables)
        .bind(source.occurred_at)
        .bind(source.by)
        .bind(source.via)
        .bind(
            target
                .pins
                .forked_from
                .map(proxiima_core::MemoryId::into_inner),
        )
        .bind(&source.lexical_language)
        .bind(source.forgotten_at)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

pub(super) async fn goal(
    tx: &mut Transaction<'_, Postgres>,
    target: &GoalTarget,
) -> Result<(), StorageError> {
    let source = &target.source;
    let dependencies: Vec<_> = target
        .pins
        .dependencies
        .iter()
        .map(|id| id.into_inner())
        .collect();
    let evidence: Vec<_> = target
        .pins
        .evidence
        .iter()
        .map(|id| id.into_inner())
        .collect();
    let motivated_by: Vec<_> = target
        .pins
        .motivated_by
        .iter()
        .map(|id| id.into_inner())
        .collect();
    if target.id == source.t.into_inner() {
        sqlx::query(
            "UPDATE proxiima_core.goal
                SET schema_id=$1,title=$2,text=$3,close_fact_t=$4,assignment_t=$5,dependency_t=$6,
                    evidence_t=$7,motivated_by=$8,write_act_t=$9,parent_handle=$10,handle=$13
              WHERE t=$11 AND owner_id=$12",
        )
        .bind(target.payload.schema_id.as_str())
        .bind(&target.payload.title)
        .bind(&target.payload.text)
        .bind(
            target
                .pins
                .close_fact
                .map(proxiima_core::MemoryId::into_inner),
        )
        .bind(
            target
                .pins
                .assignment
                .map(proxiima_core::MemoryId::into_inner),
        )
        .bind(&dependencies)
        .bind(&evidence)
        .bind(&motivated_by)
        .bind(
            target
                .pins
                .write_act
                .map(proxiima_core::MemoryId::into_inner),
        )
        .bind(target.pins.parent_handle)
        .bind(target.id)
        .bind(source.owner_id.into_inner())
        .bind(target.series)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    } else {
        sqlx::query(
            "INSERT INTO proxiima_core.goal
                (handle,t,owner_id,schema_id,title,state,request_id,close_fact_t,assignment_t,dependency_t,
                 evidence_t,write_act_t,text,by,via,parent_handle,motivated_by)
             VALUES ($1,$2,$3,$4,$5,$6::proxiima_core.goal_state,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(target.series)
        .bind(target.id)
        .bind(source.owner_id.into_inner())
        .bind(target.payload.schema_id.as_str())
        .bind(&target.payload.title)
        .bind(source.state.list_label())
        .bind(format!("schema-evolution:{}", target.id))
        .bind(target.pins.close_fact.map(proxiima_core::MemoryId::into_inner))
        .bind(target.pins.assignment.map(proxiima_core::MemoryId::into_inner))
        .bind(&dependencies)
        .bind(&evidence)
        .bind(target.pins.write_act.map(proxiima_core::MemoryId::into_inner))
        .bind(&target.payload.text)
        .bind(source.by)
        .bind(source.via)
        .bind(target.pins.parent_handle)
        .bind(target.pins.motivated_by.iter().map(|id| id.into_inner()).collect::<Vec<_>>())
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

pub(super) async fn blob(
    tx: &mut Transaction<'_, Postgres>,
    target: &BlobTarget,
) -> Result<(), StorageError> {
    let source = &target.source;
    if target.id == source.id() {
        sqlx::query("UPDATE proxiima_core.blob SET schema_id=$1 WHERE blob_id=$2 AND owner_id=$3")
            .bind(target.schema.render())
            .bind(target.id)
            .bind(source.owner().into_inner())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    } else {
        sqlx::query(
            "INSERT INTO proxiima_core.blob(blob_id,owner_id,schema_id,content_hash) VALUES($1,$2,$3,$4)",
        )
        .bind(target.id)
        .bind(source.owner().into_inner())
        .bind(target.schema.render())
        .bind(source.content_hash())
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}
