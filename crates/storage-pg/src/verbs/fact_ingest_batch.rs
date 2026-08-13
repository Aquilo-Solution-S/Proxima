//! Batched `FactIngest`: N ingest units, one transaction, one statement per
//! table instead of one per unit.
//!
//! The per-unit path in [`super::fact_ingest`] issues roughly fifteen
//! single-row statements per landed Fact, so a chat-ingest workload is capped
//! by round-trip rate rather than by anything the database is doing. This
//! module lands the same rows through `INSERT … SELECT FROM unnest(…)`, with
//! the per-unit `SAVEPOINT` around `fact_receipts` replaced by
//! `ON CONFLICT (receipt_id) DO NOTHING … RETURNING`: the receipts a batch
//! actually claimed come back from the insert, and a unit that did not claim
//! its receipt is resolved as the replay it is.
//!
//! Which path runs is [`crate::PgTuning::batched_writes`]. With the flag off
//! this module runs each unit through the untouched per-unit verb, one
//! transaction per unit — the statement stream is the one that shipped.
//!
//! Two units of one batch may carry the same receipt. That is a replay in
//! exactly the sense the serial path means: the first carrier materializes
//! the Fact and every later carrier reports `idempotent_replay = true`
//! against it, which is what a serial run of the same units produces.
//!
//! Not every unit can be set-based: a stateful Fact's head derivation reads
//! back the sidecar row it just wrote, so it runs the per-unit verb inside
//! the chunk's transaction. A chunk is therefore cut into segments at those
//! units — set-based run, stateful unit, next set-based run — and the
//! segments run in unit order. Order is the whole point: `change_event.seq`
//! is a uuid-v7 minted when the row is written, and the feed is read in seq
//! order per owner, so running the stateful units first would hand a reader
//! a batch's events in an order no serial run of the same units produces.
//!
//! A chunk stays all-or-nothing across its segments: one unit's failure
//! rolls back the whole chunk, where the serial loop commits the units
//! before it. That is inherent to batching, and it is why `write_batch_size`
//! is the unit an operator reasons about.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use proxima_core::verbs::fact_ingest::FactIngestOutcome;
use proxima_core::{
    AuthorizedFactWrite, EntityKind, FactReceiptId, MemoryId, Owner, OwnerRefKind, SidecarPayload,
    StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::PgTuning;
use crate::access::owner_columns::{owner_binds, reject_world_write_owner};
use crate::error::{internal, map_err, with_bounded_retry};
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::compliance_erase::{
    FactSuppressionProbe, check_suppression_for_fact_tx, check_suppression_for_facts_tx,
};
use crate::verbs::fact_ingest::{
    FactLinks, assert_fact_index_rows, ingest_fact_with_typed_sidecar_atomic,
    ingest_fact_with_typed_sidecar_in_tx, upsert_cited_object_hint_in_tx,
};

/// One Fact ingest unit: exactly what the per-unit
/// `FactIngestPort::ingest_fact_with_typed_sidecar` takes, so a caller
/// accumulating units hands the batch what it would have handed the port.
#[derive(Debug, Clone, Copy)]
pub struct FactIngestBatchUnit<'a> {
    pub authorized: &'a AuthorizedFactWrite,
    pub sidecar_payloads: &'a [SidecarPayload],
    pub embedding_model_id: Option<&'a str>,
}

/// Live Facts for a set of receipts — the set-based twin of the per-unit
/// receipt probe.
const SELECT_LIVE_FACTS_BY_RECEIPTS_SQL: &str = "SELECT receipt_id, memory_id
           FROM proxima_core.memories
          WHERE receipt_id = ANY($1::bytea[])
            AND tombstoned_at IS NULL";

// Target-less ON CONFLICT for the same reason the per-unit insert has it:
// keyed batches race concurrent ingests into identical rows, and `(id)` as
// the sole arbiter turns a loser's collision on
// `source_batches_unique_per_source` into a spurious unique violation.
const INSERT_SOURCE_BATCHES_SQL: &str = "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_kind,
             owner_id)
         SELECT * FROM unnest($1::uuid[], $2::text[],
                              $3::proxima_core.owner_ref_kind[], $4::uuid[])
         ON CONFLICT DO NOTHING";

// `ORDER BY id` so a batch takes the batch-row locks in one order and two
// batches overlapping on two source batches cannot deadlock on them.
const LOCK_SOURCE_BATCHES_SQL: &str = "SELECT id, closed_at IS NOT NULL
           FROM proxima_core.source_batches
          WHERE id = ANY($1::uuid[])
          ORDER BY id
            FOR UPDATE";

// `(receipt_id)` rather than a target-less conflict clause: the per-unit
// path recognises a receipt race by constraint name (`fact_receipts_pkey`)
// and lets every other unique violation through as the error it is, so the
// batch must not swallow more than that one constraint either. What comes
// back is the receipts THIS statement claimed; a unit whose receipt is
// absent from the result lost the race and replays the winner's Fact.
const INSERT_FACT_RECEIPTS_SQL: &str = "INSERT INTO proxima_core.fact_receipts
            (receipt_id, source, source_batch_id,
             owner_kind, owner_id,
             schema_id, schema_version, observed_at, occurred_at)
         SELECT * FROM unnest($1::bytea[], $2::text[], $3::uuid[],
                              $4::proxima_core.owner_ref_kind[], $5::uuid[],
                              $6::text[], $7::int[],
                              $8::timestamptz[], $9::timestamptz[])
         ON CONFLICT (receipt_id) DO NOTHING
         RETURNING receipt_id";

// NULL language means the column DEFAULT — the COALESCE spells that out
// rather than branching the statement text on the option.
const INSERT_MEMORIES_SQL: &str = "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version,
             receipt_id, citation_mapping_id, text, authoring_perspective_id,
             lexical_language)
         SELECT u.memory_id, u.owner_kind, u.owner_id, u.schema_id,
                u.schema_version, u.receipt_id, u.citation_mapping_id, u.text,
                u.authoring_perspective_id,
                COALESCE(u.lexical_language::regconfig,
                         proxima_core.lexical_config())
           FROM unnest($1::uuid[], $2::proxima_core.owner_ref_kind[],
                       $3::uuid[], $4::text[], $5::int[], $6::bytea[],
                       $7::uuid[], $8::text[], $9::uuid[], $10::text[])
                AS u(memory_id, owner_kind, owner_id, schema_id,
                     schema_version, receipt_id, citation_mapping_id, text,
                     authoring_perspective_id, lexical_language)";

const INSERT_CITATION_MAPPINGS_SQL: &str = "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id,
             cited_object_id, owner_kind,
             owner_id)
         SELECT * FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::uuid[],
                              $5::proxima_core.owner_ref_kind[], $6::uuid[])";

const INSERT_CHANGE_EVENTS_SQL: &str = "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind,
             entity_memory_id, entity_schema_id,
             entity_schema_version)
         SELECT t.seq, t.owner_kind, t.owner_id, 'EntityAppend', 'Fact',
                t.memory_id, t.schema_id, t.schema_version
           FROM unnest($1::uuid[], $2::proxima_core.owner_ref_kind[],
                       $3::uuid[], $4::uuid[], $5::text[], $6::int[])
                AS t(seq, owner_kind, owner_id, memory_id, schema_id,
                     schema_version)";

const SELECT_FIRST_CHANGE_SEQS_SQL: &str = "SELECT DISTINCT ON (entity_memory_id)
                entity_memory_id, seq
           FROM proxima_core.change_event
          WHERE entity_memory_id = ANY($1::uuid[])
          ORDER BY entity_memory_id, seq ASC";

const SELECT_CITED_OBJECTS_SQL: &str = "SELECT m.memory_id, cm.cited_object_id
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm USING (citation_mapping_id)
          WHERE m.memory_id = ANY($1::uuid[])";

/// What a batch resolved a unit to before any row was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitDisposition {
    /// Materialize this unit through the set-based statements.
    Fresh,
    /// An earlier unit of this batch carries the same receipt, so this unit
    /// replays whatever that unit resolves to.
    ReplayOfUnit(usize),
    /// Nothing set-based about it: a stateful Fact's head derivation reads
    /// back the sidecar row it just wrote and updates the memory it belongs
    /// to, so the unit runs the per-unit path inside the batch transaction.
    PerUnit,
}

/// The classification inputs of one unit — everything the disposition
/// depends on, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnitShape {
    receipt_id: Option<FactReceiptId>,
    stateful: bool,
}

/// Resolve every unit against the batch it arrived in, before the batch
/// touches the database.
///
/// Duplicates are resolved strictly by position, which is what makes the
/// batch agree with a serial run of the same units: the first carrier of a
/// receipt materializes the Fact and every later carrier replays it,
/// whichever path each of them takes.
fn plan_dispositions(shapes: &[UnitShape]) -> Vec<UnitDisposition> {
    let mut claimed: HashMap<FactReceiptId, usize> = HashMap::new();
    let mut dispositions = Vec::with_capacity(shapes.len());
    for (index, shape) in shapes.iter().enumerate() {
        let carries = |shape: &UnitShape| {
            if shape.stateful {
                UnitDisposition::PerUnit
            } else {
                UnitDisposition::Fresh
            }
        };
        let disposition = match shape.receipt_id {
            Some(receipt_id) => match claimed.entry(receipt_id) {
                Entry::Occupied(first) => UnitDisposition::ReplayOfUnit(*first.get()),
                Entry::Vacant(slot) => {
                    slot.insert(index);
                    carries(shape)
                }
            },
            None => carries(shape),
        };
        dispositions.push(disposition);
    }
    dispositions
}

fn unit_shape(unit: &FactIngestBatchUnit<'_>) -> Result<UnitShape, StorageError> {
    let owner = unit.authorized.owner_write_permit().owner();
    reject_world_write_owner(owner)?;
    Ok(UnitShape {
        receipt_id: unit.authorized.draft().receipt_id_for_owner(*owner),
        stateful: !unit.authorized.fact_natural_key_columns().is_empty(),
    })
}

fn unit_owner(unit: &FactIngestBatchUnit<'_>) -> Owner {
    *unit.authorized.owner_write_permit().owner()
}

fn unit_links<'a>(unit: &FactIngestBatchUnit<'a>) -> FactLinks<'a> {
    FactLinks::new(
        unit.authorized.links().origins(),
        unit.authorized.links().references(),
    )
}

/// Land a set of authorized Facts.
///
/// With [`PgTuning::batched_writes`] off this is a loop over the per-unit
/// verb, one transaction each — the write path exactly as it ships. With it
/// on, units are cut into [`PgTuning::write_batch_size`] chunks and each
/// chunk lands in one transaction through the set-based statements.
///
/// Outcomes are returned in unit order in both cases.
///
/// # Errors
///
/// Returns storage errors from transaction setup, Fact materialization,
/// sidecar insertion, or commit. A batched chunk is all-or-nothing: one
/// unit's error rolls the whole chunk back.
pub async fn ingest_facts_batch(
    pool: &PgPool,
    tuning: &PgTuning,
    sidecars: &PgSidecarRegistryFrozen,
    units: &[FactIngestBatchUnit<'_>],
) -> Result<Vec<FactIngestOutcome>, StorageError> {
    let mut outcomes = Vec::with_capacity(units.len());
    if !tuning.batched_writes {
        for unit in units {
            outcomes.push(
                ingest_fact_with_typed_sidecar_atomic(
                    pool,
                    sidecars,
                    unit.authorized,
                    unit.sidecar_payloads,
                    unit.embedding_model_id,
                )
                .await?,
            );
        }
        return Ok(outcomes);
    }

    let batch_size = usize::try_from(tuning.write_batch_size)
        .unwrap_or(usize::MAX)
        .max(1);
    for chunk in units.chunks(batch_size) {
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization. Every unit is data, so an attempt rebuilds the
        // batch from scratch.
        let landed = with_bounded_retry(|| async {
            let mut tx = pool.begin().await.map_err(internal)?;
            let landed = ingest_facts_batch_in_tx(&mut tx, tuning, sidecars, chunk).await?;
            tx.commit().await.map_err(map_err)?;
            Ok(landed)
        })
        .await?;
        outcomes.extend(landed);
    }
    Ok(outcomes)
}

/// Land a set of authorized Facts inside an already-open transaction, with
/// one statement per table rather than one per unit.
///
/// The caller owns commit and rollback. Outcomes are returned in unit order.
/// A chunk holding stateful Facts costs one statement per table per
/// segment; the ordinary chunk holds none and is a single segment.
///
/// Crate-private, and deliberately: this body IS the flag-on arm, so it
/// does not consult [`PgTuning::batched_writes`] — [`ingest_facts_batch`] is
/// where the flag decides, and an entry point reachable past it would run
/// the batched arm at default tuning.
///
/// # Errors
///
/// Returns `Suppressed` when any unit is under a compliance suppression key,
/// `ConstraintViolation` when any unit targets a closed source batch, and
/// storage errors from row materialization or sidecar insertion.
pub(crate) async fn ingest_facts_batch_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    tuning: &PgTuning,
    sidecars: &PgSidecarRegistryFrozen,
    units: &[FactIngestBatchUnit<'_>],
) -> Result<Vec<FactIngestOutcome>, StorageError> {
    if units.is_empty() {
        return Ok(Vec::new());
    }

    let shapes = units
        .iter()
        .map(unit_shape)
        .collect::<Result<Vec<_>, _>>()?;
    let dispositions = plan_dispositions(&shapes);
    let plan = BatchPlan {
        units,
        shapes: &shapes,
        dispositions: &dispositions,
    };
    let mut outcomes: Vec<Option<FactIngestOutcome>> = vec![None; units.len()];

    // Segments, in unit order: a run of set-based units, then the stateful
    // unit that cannot join one, then the next run. Each segment's rows —
    // and so its `change_event` seqs — land after everything before it in
    // the chunk, which is the order a serial run of the same units writes
    // them in. Hoisting the stateful units to the front would mint their
    // seqs first and reverse the feed for every owner that mixes the two.
    for step in plan_steps(&dispositions) {
        match step {
            BatchStep::Segment(segment) => {
                land_segment(tx, tuning, sidecars, plan, &segment, &mut outcomes).await?;
            }
            BatchStep::PerUnit(index) => {
                let unit = &units[index];
                outcomes[index] = Some(
                    ingest_fact_with_typed_sidecar_in_tx(
                        tx,
                        sidecars,
                        unit.authorized,
                        unit.sidecar_payloads,
                        unit.embedding_model_id,
                    )
                    .await?,
                );
            }
        }
    }

    outcomes
        .into_iter()
        .enumerate()
        .map(|(index, outcome)| {
            outcome.ok_or_else(|| {
                StorageError::Internal(format!("batched Fact ingest left unit {index} unresolved"))
            })
        })
        .collect()
}

/// One step of a chunk: a run of units the set-based statements carry, or
/// the one stateful unit that cannot join a run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BatchStep {
    Segment(Vec<usize>),
    PerUnit(usize),
}

/// Cut a chunk into the steps it runs, in unit order.
///
/// A stateful unit ends the run before it and starts a new one, so the rows
/// of a chunk land in the order its units were handed over. A chunk with no
/// stateful unit — the ordinary chat-write case — is one segment, so
/// segmentation costs nothing where nothing needs cutting.
fn plan_steps(dispositions: &[UnitDisposition]) -> Vec<BatchStep> {
    let mut steps = Vec::new();
    let mut segment: Vec<usize> = Vec::new();
    for (index, disposition) in dispositions.iter().enumerate() {
        if *disposition != UnitDisposition::PerUnit {
            segment.push(index);
            continue;
        }
        if !segment.is_empty() {
            steps.push(BatchStep::Segment(std::mem::take(&mut segment)));
        }
        steps.push(BatchStep::PerUnit(index));
    }
    if !segment.is_empty() {
        steps.push(BatchStep::Segment(segment));
    }
    steps
}

/// One chunk's units and everything resolved about them before any row was
/// written. Carried together because every step below reads all three by
/// the same index.
#[derive(Debug, Clone, Copy)]
struct BatchPlan<'a, 'u> {
    units: &'a [FactIngestBatchUnit<'u>],
    shapes: &'a [UnitShape],
    dispositions: &'a [UnitDisposition],
}

/// Land one run of set-based units, in one statement per table.
///
/// `segment` is a contiguous run of the chunk in unit order, holding no
/// [`UnitDisposition::PerUnit`] unit. A `ReplayOfUnit` inside it always
/// names an EARLIER unit, which an earlier segment or an earlier stateful
/// unit has already resolved.
#[allow(clippy::too_many_lines)] // one block per table the fan-out writes
async fn land_segment(
    tx: &mut Transaction<'_, Postgres>,
    tuning: &PgTuning,
    sidecars: &PgSidecarRegistryFrozen,
    plan: BatchPlan<'_, '_>,
    segment: &[usize],
    outcomes: &mut [Option<FactIngestOutcome>],
) -> Result<(), StorageError> {
    if segment.is_empty() {
        return Ok(());
    }
    let BatchPlan {
        units,
        shapes,
        dispositions,
    } = plan;

    // Admission covers every unit the per-unit path has not already admitted
    // for itself, INCLUDING the ones that duplicate an earlier unit's
    // receipt: two units can share a receipt and still name different source
    // batches, and a suppression key on either batch has to be answered.
    check_batch_suppression(tx, tuning, units, shapes, segment).await?;

    let mut pending: Vec<usize> = segment
        .iter()
        .copied()
        .filter(|index| dispositions[*index] == UnitDisposition::Fresh)
        .collect();

    // One probe for every receipt in the batch, in place of one probe per
    // unit.
    let replayed = live_facts_by_receipts(tx, receipt_bytes(shapes, &pending)).await?;
    let (replays, mut fresh): (Vec<usize>, Vec<usize>) = pending.drain(..).partition(
        |index| matches!(shapes[*index].receipt_id, Some(id) if replayed.contains_key(&id)),
    );
    resolve_replays(tx, units, shapes, &replays, &replayed, outcomes).await?;

    // Cited objects are content-addressed and deduplicated across the Facts
    // that cite them, so each is upserted on its own — the batched insert
    // below is the mapping row, which is per Fact.
    let mut citations: HashMap<usize, (uuid::Uuid, uuid::Uuid)> = HashMap::new();
    for index in &fresh {
        let Some(citation) = units[*index].authorized.draft().citation.as_ref() else {
            continue;
        };
        let cited_object_id =
            upsert_cited_object_hint_in_tx(tx, &unit_owner(&units[*index]), &citation.object)
                .await?;
        citations.insert(*index, (uuid::Uuid::now_v7(), cited_object_id));
    }

    open_source_batches(tx, units, shapes, &fresh).await?;
    let claimed = claim_fact_receipts(tx, units, shapes, &fresh).await?;
    let lost: Vec<usize> = fresh
        .iter()
        .copied()
        .filter(|index| matches!(shapes[*index].receipt_id, Some(id) if !claimed.contains(&id)))
        .collect();
    if !lost.is_empty() {
        // The receipt was claimed by a concurrent transaction between the
        // probe above and this insert. That is the race the per-unit path
        // catches with its SAVEPOINT, and its answer is the same: replay the
        // winner's committed Fact.
        fresh.retain(|index| !lost.contains(index));
        let winners = live_facts_by_receipts(tx, receipt_bytes(shapes, &lost)).await?;
        for index in &lost {
            let Some(receipt_id) = shapes[*index].receipt_id else {
                continue;
            };
            if !winners.contains_key(&receipt_id) {
                // Receipt occupied but no live memory row (e.g. tombstoned):
                // a genuine conflict, not a concurrent-ingest replay.
                return Err(StorageError::ConstraintViolation(
                    "fact receipt is already claimed by a Fact that is no longer live".into(),
                ));
            }
        }
        resolve_replays(tx, units, shapes, &lost, &winners, outcomes).await?;
    }

    let minted: Vec<(uuid::Uuid, uuid::Uuid)> = fresh
        .iter()
        .map(|_| (uuid::Uuid::now_v7(), uuid::Uuid::now_v7()))
        .collect();

    register_batch_languages(tx, tuning, units, &fresh).await?;
    insert_memories(tx, units, shapes, &fresh, &minted, &citations).await?;

    for (slot, index) in fresh.iter().enumerate() {
        assert_fact_index_rows(
            tx,
            &unit_owner(&units[*index]),
            minted[slot].0,
            unit_links(&units[*index]),
        )
        .await?;
    }

    let sidecar_rows: Vec<(MemoryId, &SidecarPayload)> = fresh
        .iter()
        .enumerate()
        .flat_map(|(slot, index)| {
            let memory_id = MemoryId::new(minted[slot].0);
            units[*index]
                .sidecar_payloads
                .iter()
                .map(move |payload| (memory_id, payload))
        })
        .collect();
    sidecars
        .insert_memory_sidecar_batch(tx, &sidecar_rows)
        .await?;

    enqueue_batch_embeddings(tx, units, &fresh, &minted).await?;
    insert_citation_mappings(tx, units, &fresh, &minted, &citations).await?;
    insert_change_events(tx, units, &fresh, &minted).await?;

    for (slot, index) in fresh.iter().enumerate() {
        outcomes[*index] = Some(FactIngestOutcome {
            receipt_id: shapes[*index].receipt_id,
            memory_id: MemoryId::new(minted[slot].0),
            change_event_seq: minted[slot].1,
            idempotent_replay: false,
            cited_object_id: citations.get(index).map(|(_, cited)| *cited),
        });
    }

    // Every unit that duplicates an earlier unit's receipt reports that
    // unit's Fact as a replay — the answer a serial run gives, without the
    // round trip that would re-read what this transaction already knows.
    for index in segment.iter().copied() {
        let UnitDisposition::ReplayOfUnit(first) = dispositions[index] else {
            continue;
        };
        let mut outcome = outcomes[first].clone().ok_or_else(|| {
            StorageError::Internal(format!(
                "batched Fact ingest unit {index} replays unresolved unit {first}"
            ))
        })?;
        outcome.idempotent_replay = true;
        assert_fact_index_rows(
            tx,
            &unit_owner(&units[index]),
            outcome.memory_id.into_inner(),
            unit_links(&units[index]),
        )
        .await?;
        outcomes[index] = Some(outcome);
    }

    Ok(())
}

/// Admit every unit of the batch against the compliance suppression keys.
///
/// With [`PgTuning::static_lookup_cache`] on this is one probe for the whole
/// batch; with it off it is the per-unit probe the serial path issues, once
/// per unit, so the flag's two arms differ in statement count and in nothing
/// else.
async fn check_batch_suppression(
    tx: &mut Transaction<'_, Postgres>,
    tuning: &PgTuning,
    units: &[FactIngestBatchUnit<'_>],
    shapes: &[UnitShape],
    admitted: &[usize],
) -> Result<(), StorageError> {
    // Receipt bytes are carried WITH the unit they belong to: a receiptless
    // unit in the middle of the batch must not shift the receipt every unit
    // after it is admitted against.
    let receipt_ids: Vec<(usize, [u8; 32])> = admitted
        .iter()
        .filter_map(|index| {
            shapes[*index]
                .receipt_id
                .map(|receipt_id| (*index, receipt_id.into_inner()))
        })
        .collect();
    let mut probes = Vec::with_capacity(receipt_ids.len());
    for (index, receipt_id) in &receipt_ids {
        let Some(receipt) = units[*index].authorized.draft().receipt.as_ref() else {
            continue;
        };
        probes.push(FactSuppressionProbe {
            owner: unit_owner(&units[*index]),
            source_id: &receipt.source_id,
            source_batch_id: receipt.source_batch_id.into_inner(),
            receipt_id: &receipt_id[..],
        });
    }

    if tuning.static_lookup_cache {
        return check_suppression_for_facts_tx(tx, &probes).await;
    }
    for probe in &probes {
        check_suppression_for_fact_tx(
            tx,
            probe.owner,
            probe.source_id,
            probe.source_batch_id,
            probe.receipt_id,
        )
        .await?;
    }
    Ok(())
}

fn receipt_bytes(shapes: &[UnitShape], indices: &[usize]) -> Vec<[u8; 32]> {
    indices
        .iter()
        .filter_map(|index| shapes[*index].receipt_id.map(FactReceiptId::into_inner))
        .collect()
}

async fn live_facts_by_receipts(
    tx: &mut Transaction<'_, Postgres>,
    receipt_ids: Vec<[u8; 32]>,
) -> Result<HashMap<FactReceiptId, uuid::Uuid>, StorageError> {
    if receipt_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let binds: Vec<&[u8]> = receipt_ids.iter().map(|bytes| &bytes[..]).collect();
    let rows: Vec<(Vec<u8>, uuid::Uuid)> = sqlx::query_as(SELECT_LIVE_FACTS_BY_RECEIPTS_SQL)
        .bind(&binds)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    rows.into_iter()
        .map(|(receipt_id, memory_id)| {
            let receipt_id = <[u8; 32]>::try_from(&receipt_id[..]).map_err(|_| {
                StorageError::Internal(format!(
                    "fact receipt id for memory {memory_id} is not 32 bytes"
                ))
            })?;
            Ok((FactReceiptId::new(receipt_id), memory_id))
        })
        .collect()
}

/// Build the `idempotent_replay = true` outcomes for units whose Fact is
/// already materialized, resolving the original `change_event` seq and the
/// citation the ORIGINAL write made in one query each rather than two per
/// unit.
async fn resolve_replays(
    tx: &mut Transaction<'_, Postgres>,
    units: &[FactIngestBatchUnit<'_>],
    shapes: &[UnitShape],
    indices: &[usize],
    replayed: &HashMap<FactReceiptId, uuid::Uuid>,
    outcomes: &mut [Option<FactIngestOutcome>],
) -> Result<(), StorageError> {
    if indices.is_empty() {
        return Ok(());
    }
    let memory_ids: Vec<uuid::Uuid> = indices
        .iter()
        .filter_map(|index| {
            shapes[*index]
                .receipt_id
                .and_then(|receipt_id| replayed.get(&receipt_id).copied())
        })
        .collect();

    let seqs: HashMap<uuid::Uuid, uuid::Uuid> = sqlx::query_as(SELECT_FIRST_CHANGE_SEQS_SQL)
        .bind(&memory_ids)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?
        .into_iter()
        .collect();
    let cited: HashMap<uuid::Uuid, uuid::Uuid> = sqlx::query_as(SELECT_CITED_OBJECTS_SQL)
        .bind(&memory_ids)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?
        .into_iter()
        .collect();

    for index in indices {
        let Some(memory_id) = shapes[*index]
            .receipt_id
            .and_then(|receipt_id| replayed.get(&receipt_id).copied())
        else {
            continue;
        };
        let change_event_seq = seqs.get(&memory_id).copied().ok_or_else(|| {
            StorageError::Internal(format!("replayed Fact {memory_id} has no change event"))
        })?;
        outcomes[*index] = Some(FactIngestOutcome {
            receipt_id: shapes[*index].receipt_id,
            memory_id: MemoryId::new(memory_id),
            change_event_seq,
            idempotent_replay: true,
            cited_object_id: cited.get(&memory_id).copied(),
        });
        assert_fact_index_rows(
            tx,
            &unit_owner(&units[*index]),
            memory_id,
            unit_links(&units[*index]),
        )
        .await?;
    }
    Ok(())
}

/// Open every source batch the fresh units write into, then take the batch
/// rows' locks and refuse the whole batch if any of them is closed.
async fn open_source_batches(
    tx: &mut Transaction<'_, Postgres>,
    units: &[FactIngestBatchUnit<'_>],
    shapes: &[UnitShape],
    fresh: &[usize],
) -> Result<(), StorageError> {
    let mut ids = Vec::new();
    let mut source_ids = Vec::new();
    let mut owner_kinds = Vec::new();
    let mut owner_ids = Vec::new();
    for index in fresh {
        let draft = units[*index].authorized.draft();
        let (Some(receipt), Some(_)) = (draft.receipt.as_ref(), shapes[*index].receipt_id) else {
            continue;
        };
        let id = receipt.source_batch_id.into_inner();
        if ids.contains(&id) {
            continue;
        }
        let (owner_kind, owner_id) = owner_binds(&unit_owner(&units[*index]));
        ids.push(id);
        source_ids.push(receipt.source_id.as_str());
        owner_kinds.push(owner_kind);
        owner_ids.push(owner_id);
    }
    if ids.is_empty() {
        return Ok(());
    }

    sqlx::query(INSERT_SOURCE_BATCHES_SQL)
        .bind(&ids)
        .bind(&source_ids)
        .bind(&owner_kinds)
        .bind(&owner_ids)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    let closed: Vec<(uuid::Uuid, bool)> = sqlx::query_as(LOCK_SOURCE_BATCHES_SQL)
        .bind(&ids)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    if closed.iter().any(|(_, closed)| *closed) {
        return Err(StorageError::ConstraintViolation(
            "cannot ingest Fact into closed source batch".into(),
        ));
    }
    Ok(())
}

/// Claim one receipt row per receipt-bearing fresh unit, and answer which
/// receipts this transaction actually claimed.
async fn claim_fact_receipts(
    tx: &mut Transaction<'_, Postgres>,
    units: &[FactIngestBatchUnit<'_>],
    shapes: &[UnitShape],
    fresh: &[usize],
) -> Result<Vec<FactReceiptId>, StorageError> {
    let mut receipt_ids = Vec::new();
    let mut sources = Vec::new();
    let mut source_batch_ids = Vec::new();
    let mut owner_kinds = Vec::new();
    let mut owner_ids = Vec::new();
    let mut schema_ids = Vec::new();
    let mut schema_versions = Vec::new();
    let mut observed_at = Vec::new();
    let mut occurred_at = Vec::new();
    for index in fresh {
        let draft = units[*index].authorized.draft();
        let (Some(receipt), Some(receipt_id)) = (draft.receipt.as_ref(), shapes[*index].receipt_id)
        else {
            continue;
        };
        let (owner_kind, owner_id) = owner_binds(&unit_owner(&units[*index]));
        receipt_ids.push(receipt_id.into_inner());
        sources.push(receipt.source_id.as_str());
        source_batch_ids.push(receipt.source_batch_id.into_inner());
        owner_kinds.push(owner_kind);
        owner_ids.push(owner_id);
        schema_ids.push(draft.schema_id.as_str());
        schema_versions.push(draft.schema_version.into_inner().cast_signed());
        observed_at.push(receipt.observed_at);
        occurred_at.push(receipt.occurred_at);
    }
    if receipt_ids.is_empty() {
        return Ok(Vec::new());
    }

    let binds: Vec<&[u8]> = receipt_ids.iter().map(|bytes| &bytes[..]).collect();
    let claimed: Vec<(Vec<u8>,)> = sqlx::query_as(INSERT_FACT_RECEIPTS_SQL)
        .bind(&binds)
        .bind(&sources)
        .bind(&source_batch_ids)
        .bind(&owner_kinds)
        .bind(&owner_ids)
        .bind(&schema_ids)
        .bind(&schema_versions)
        .bind(&observed_at)
        .bind(&occurred_at)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    claimed
        .into_iter()
        .map(|(receipt_id,)| {
            let receipt_id = <[u8; 32]>::try_from(&receipt_id[..])
                .map_err(|_| StorageError::Internal("claimed receipt id is not 32 bytes".into()))?;
            Ok(FactReceiptId::new(receipt_id))
        })
        .collect()
}

/// Register the DISTINCT languages the batch stamps, once each.
///
/// One registration per language rather than per unit: the `FOR KEY SHARE`
/// hold it takes lasts until this transaction ends, so a second registration
/// of the same language inside it would re-take a lock already held.
async fn register_batch_languages(
    tx: &mut Transaction<'_, Postgres>,
    tuning: &PgTuning,
    units: &[FactIngestBatchUnit<'_>],
    fresh: &[usize],
) -> Result<(), StorageError> {
    let mut registered: Vec<&str> = Vec::new();
    for index in fresh {
        let Some(language) = units[*index].authorized.draft().lexical_language.as_deref() else {
            continue;
        };
        if registered.contains(&language) {
            continue;
        }
        registered.push(language);
        if tuning.static_lookup_cache {
            super::lexical_language::register_lexical_language_cached_in_tx(tx, language).await?;
        } else {
            super::lexical_language::register_lexical_language_in_tx(tx, language).await?;
        }
    }
    Ok(())
}

async fn insert_memories(
    tx: &mut Transaction<'_, Postgres>,
    units: &[FactIngestBatchUnit<'_>],
    shapes: &[UnitShape],
    fresh: &[usize],
    minted: &[(uuid::Uuid, uuid::Uuid)],
    citations: &HashMap<usize, (uuid::Uuid, uuid::Uuid)>,
) -> Result<(), StorageError> {
    if fresh.is_empty() {
        return Ok(());
    }
    let mut memory_ids = Vec::with_capacity(fresh.len());
    let mut owner_kinds: Vec<OwnerRefKind> = Vec::with_capacity(fresh.len());
    let mut owner_ids = Vec::with_capacity(fresh.len());
    let mut schema_ids = Vec::with_capacity(fresh.len());
    let mut schema_versions = Vec::with_capacity(fresh.len());
    let mut receipt_ids: Vec<Option<[u8; 32]>> = Vec::with_capacity(fresh.len());
    let mut citation_mapping_ids = Vec::with_capacity(fresh.len());
    let mut texts = Vec::with_capacity(fresh.len());
    let mut authoring_perspective_ids = Vec::with_capacity(fresh.len());
    let mut languages = Vec::with_capacity(fresh.len());
    for (slot, index) in fresh.iter().enumerate() {
        let draft = units[*index].authorized.draft();
        let (owner_kind, owner_id) = owner_binds(&unit_owner(&units[*index]));
        memory_ids.push(minted[slot].0);
        owner_kinds.push(owner_kind);
        owner_ids.push(owner_id);
        schema_ids.push(draft.schema_id.as_str());
        schema_versions.push(draft.schema_version.into_inner().cast_signed());
        receipt_ids.push(shapes[*index].receipt_id.map(FactReceiptId::into_inner));
        citation_mapping_ids.push(citations.get(index).map(|(mapping, _)| *mapping));
        texts.push(draft.rendered_text.as_deref());
        authoring_perspective_ids.push(None::<uuid::Uuid>);
        languages.push(draft.lexical_language.as_deref());
    }
    let receipt_binds: Vec<Option<&[u8]>> = receipt_ids
        .iter()
        .map(|receipt_id| receipt_id.as_ref().map(|bytes| &bytes[..]))
        .collect();

    sqlx::query(INSERT_MEMORIES_SQL)
        .bind(&memory_ids)
        .bind(&owner_kinds)
        .bind(&owner_ids)
        .bind(&schema_ids)
        .bind(&schema_versions)
        .bind(&receipt_binds)
        .bind(&citation_mapping_ids)
        .bind(&texts)
        .bind(&authoring_perspective_ids)
        .bind(&languages)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

async fn enqueue_batch_embeddings(
    tx: &mut Transaction<'_, Postgres>,
    units: &[FactIngestBatchUnit<'_>],
    fresh: &[usize],
    minted: &[(uuid::Uuid, uuid::Uuid)],
) -> Result<(), StorageError> {
    let mut owner_kinds = Vec::new();
    let mut owner_ids = Vec::new();
    let mut entity_kinds = Vec::new();
    let mut entity_ids = Vec::new();
    let mut model_ids = Vec::new();
    for (slot, index) in fresh.iter().enumerate() {
        let Some(model_id) = units[*index].embedding_model_id else {
            continue;
        };
        let (owner_kind, owner_id) = owner_binds(&unit_owner(&units[*index]));
        owner_kinds.push(owner_kind);
        owner_ids.push(owner_id);
        entity_kinds.push(EntityKind::Fact);
        entity_ids.push(minted[slot].0);
        model_ids.push(model_id);
    }
    crate::verbs::fact_embeddings::enqueue_embedding_jobs_in_tx(
        tx,
        &owner_kinds,
        &owner_ids,
        &entity_kinds,
        &entity_ids,
        &model_ids,
    )
    .await
}

async fn insert_citation_mappings(
    tx: &mut Transaction<'_, Postgres>,
    units: &[FactIngestBatchUnit<'_>],
    fresh: &[usize],
    minted: &[(uuid::Uuid, uuid::Uuid)],
    citations: &HashMap<usize, (uuid::Uuid, uuid::Uuid)>,
) -> Result<(), StorageError> {
    if citations.is_empty() {
        return Ok(());
    }
    let mut citation_mapping_ids = Vec::new();
    let mut schema_ids = Vec::new();
    let mut memory_ids = Vec::new();
    let mut cited_object_ids = Vec::new();
    let mut owner_kinds = Vec::new();
    let mut owner_ids = Vec::new();
    for (slot, index) in fresh.iter().enumerate() {
        let (Some((citation_mapping_id, cited_object_id)), Some(citation)) = (
            citations.get(index),
            units[*index].authorized.draft().citation.as_ref(),
        ) else {
            continue;
        };
        let (owner_kind, owner_id) = owner_binds(&unit_owner(&units[*index]));
        citation_mapping_ids.push(*citation_mapping_id);
        schema_ids.push(citation.mapping.schema_id.as_str());
        memory_ids.push(minted[slot].0);
        cited_object_ids.push(*cited_object_id);
        owner_kinds.push(owner_kind);
        owner_ids.push(owner_id);
    }
    if citation_mapping_ids.is_empty() {
        return Ok(());
    }

    sqlx::query(INSERT_CITATION_MAPPINGS_SQL)
        .bind(&citation_mapping_ids)
        .bind(&schema_ids)
        .bind(&memory_ids)
        .bind(&cited_object_ids)
        .bind(&owner_kinds)
        .bind(&owner_ids)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

async fn insert_change_events(
    tx: &mut Transaction<'_, Postgres>,
    units: &[FactIngestBatchUnit<'_>],
    fresh: &[usize],
    minted: &[(uuid::Uuid, uuid::Uuid)],
) -> Result<(), StorageError> {
    if fresh.is_empty() {
        return Ok(());
    }
    let mut seqs = Vec::with_capacity(fresh.len());
    let mut owner_kinds = Vec::with_capacity(fresh.len());
    let mut owner_ids = Vec::with_capacity(fresh.len());
    let mut memory_ids = Vec::with_capacity(fresh.len());
    let mut schema_ids = Vec::with_capacity(fresh.len());
    let mut schema_versions = Vec::with_capacity(fresh.len());
    for (slot, index) in fresh.iter().enumerate() {
        let draft = units[*index].authorized.draft();
        let (owner_kind, owner_id) = owner_binds(&unit_owner(&units[*index]));
        seqs.push(minted[slot].1);
        owner_kinds.push(owner_kind);
        owner_ids.push(owner_id);
        memory_ids.push(minted[slot].0);
        schema_ids.push(draft.schema_id.as_str());
        schema_versions.push(draft.schema_version.into_inner().cast_signed());
    }

    sqlx::query(INSERT_CHANGE_EVENTS_SQL)
        .bind(&seqs)
        .bind(&owner_kinds)
        .bind(&owner_ids)
        .bind(&memory_ids)
        .bind(&schema_ids)
        .bind(&schema_versions)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(receipt: Option<u8>, stateful: bool) -> UnitShape {
        UnitShape {
            receipt_id: receipt.map(|seed| FactReceiptId::new([seed; 32])),
            stateful,
        }
    }

    #[test]
    fn an_empty_batch_plans_nothing() {
        assert!(plan_dispositions(&[]).is_empty());
    }

    /// A batch of one is the serial path's unit of work: one fresh Fact,
    /// deferred to nothing and duplicating nothing.
    #[test]
    fn a_batch_of_one_is_one_fresh_unit() {
        assert_eq!(
            plan_dispositions(&[shape(Some(1), false)]),
            vec![UnitDisposition::Fresh]
        );
    }

    /// A stateful Fact's head derivation reads back the sidecar row it just
    /// wrote, which no set-based statement can express, so the unit runs the
    /// per-unit path — inside the batch transaction, but statement for
    /// statement what it always was.
    #[test]
    fn a_stateful_fact_takes_the_per_unit_path() {
        assert_eq!(
            plan_dispositions(&[shape(Some(1), true), shape(None, true)]),
            vec![UnitDisposition::PerUnit, UnitDisposition::PerUnit]
        );
    }

    /// The replay a batch has to detect on its own: the same receipt twice
    /// in one batch, where no committed row exists to probe for. The first
    /// carrier materializes the Fact and every later carrier — whichever
    /// path it would otherwise take — replays it.
    #[test]
    fn a_receipt_repeated_in_one_batch_replays_its_first_carrier() {
        assert_eq!(
            plan_dispositions(&[
                shape(Some(1), false),
                shape(Some(1), false),
                shape(Some(2), false),
                shape(Some(1), true),
            ]),
            vec![
                UnitDisposition::Fresh,
                UnitDisposition::ReplayOfUnit(0),
                UnitDisposition::Fresh,
                UnitDisposition::ReplayOfUnit(0),
            ]
        );
    }

    /// A stateful first carrier still owns the receipt, so a later
    /// set-based unit replays it rather than racing it inside the
    /// transaction.
    #[test]
    fn a_stateful_first_carrier_still_owns_the_receipt() {
        assert_eq!(
            plan_dispositions(&[shape(Some(7), true), shape(Some(7), false)]),
            vec![UnitDisposition::PerUnit, UnitDisposition::ReplayOfUnit(0)]
        );
    }

    /// The ordinary chat-write chunk holds no stateful unit, so it is one
    /// segment and one set of statements — segmentation costs nothing where
    /// nothing needs cutting.
    #[test]
    fn a_chunk_without_stateful_units_is_one_segment() {
        let dispositions = plan_dispositions(&[
            shape(Some(1), false),
            shape(None, false),
            shape(Some(1), false),
        ]);

        assert_eq!(
            plan_steps(&dispositions),
            vec![BatchStep::Segment(vec![0, 1, 2])]
        );
    }

    /// A stateful unit cuts the chunk where it stands rather than being
    /// hoisted to the front: the rows either side of it — and so their
    /// `change_event` seqs — land in the order the units were handed over,
    /// which is the order a serial run writes them in.
    #[test]
    fn a_stateful_unit_cuts_the_chunk_where_it_stands() {
        let dispositions = plan_dispositions(&[
            shape(Some(1), false),
            shape(Some(2), true),
            shape(Some(3), false),
            shape(Some(4), false),
        ]);

        assert_eq!(
            plan_steps(&dispositions),
            vec![
                BatchStep::Segment(vec![0]),
                BatchStep::PerUnit(1),
                BatchStep::Segment(vec![2, 3]),
            ]
        );
    }

    /// A chunk of nothing but stateful units is the per-unit path in unit
    /// order, with no empty segment between its steps.
    #[test]
    fn an_all_stateful_chunk_runs_unit_by_unit() {
        let dispositions = plan_dispositions(&[shape(Some(1), true), shape(Some(2), true)]);

        assert_eq!(
            plan_steps(&dispositions),
            vec![BatchStep::PerUnit(0), BatchStep::PerUnit(1)]
        );
    }

    /// Every replay resolves against a unit an earlier step already landed,
    /// which is what lets a segment answer its duplicates from `outcomes`
    /// instead of re-reading the row.
    #[test]
    fn a_replay_never_precedes_the_unit_it_replays() {
        let dispositions = plan_dispositions(&[
            shape(Some(1), true),
            shape(Some(1), false),
            shape(Some(2), false),
            shape(Some(2), true),
        ]);

        for (index, disposition) in dispositions.iter().enumerate() {
            if let UnitDisposition::ReplayOfUnit(first) = *disposition {
                assert!(first < index, "unit {index} replays later unit {first}");
            }
        }
    }

    /// Receiptless Facts are never receipt-replayed — two identical
    /// receiptless units are two Facts, on both paths.
    #[test]
    fn receiptless_units_are_never_replays() {
        assert_eq!(
            plan_dispositions(&[shape(None, false), shape(None, false)]),
            vec![UnitDisposition::Fresh, UnitDisposition::Fresh]
        );
    }

    /// Golden text for every statement the batched path issues. The flag's
    /// claim is a statement count per unit, so the statements themselves are
    /// pinned: a batch of any size issues these and nothing else, and W2
    /// hashes exactly this text.
    #[test]
    fn the_batched_statements_are_pinned() {
        assert_eq!(
            SELECT_LIVE_FACTS_BY_RECEIPTS_SQL,
            "SELECT receipt_id, memory_id
           FROM proxima_core.memories
          WHERE receipt_id = ANY($1::bytea[])
            AND tombstoned_at IS NULL"
        );
        assert_eq!(
            INSERT_SOURCE_BATCHES_SQL,
            "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_kind,
             owner_id)
         SELECT * FROM unnest($1::uuid[], $2::text[],
                              $3::proxima_core.owner_ref_kind[], $4::uuid[])
         ON CONFLICT DO NOTHING"
        );
        assert_eq!(
            LOCK_SOURCE_BATCHES_SQL,
            "SELECT id, closed_at IS NOT NULL
           FROM proxima_core.source_batches
          WHERE id = ANY($1::uuid[])
          ORDER BY id
            FOR UPDATE"
        );
        assert_eq!(
            INSERT_FACT_RECEIPTS_SQL,
            "INSERT INTO proxima_core.fact_receipts
            (receipt_id, source, source_batch_id,
             owner_kind, owner_id,
             schema_id, schema_version, observed_at, occurred_at)
         SELECT * FROM unnest($1::bytea[], $2::text[], $3::uuid[],
                              $4::proxima_core.owner_ref_kind[], $5::uuid[],
                              $6::text[], $7::int[],
                              $8::timestamptz[], $9::timestamptz[])
         ON CONFLICT (receipt_id) DO NOTHING
         RETURNING receipt_id"
        );
        assert_eq!(
            INSERT_MEMORIES_SQL,
            "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version,
             receipt_id, citation_mapping_id, text, authoring_perspective_id,
             lexical_language)
         SELECT u.memory_id, u.owner_kind, u.owner_id, u.schema_id,
                u.schema_version, u.receipt_id, u.citation_mapping_id, u.text,
                u.authoring_perspective_id,
                COALESCE(u.lexical_language::regconfig,
                         proxima_core.lexical_config())
           FROM unnest($1::uuid[], $2::proxima_core.owner_ref_kind[],
                       $3::uuid[], $4::text[], $5::int[], $6::bytea[],
                       $7::uuid[], $8::text[], $9::uuid[], $10::text[])
                AS u(memory_id, owner_kind, owner_id, schema_id,
                     schema_version, receipt_id, citation_mapping_id, text,
                     authoring_perspective_id, lexical_language)"
        );
        assert_eq!(
            INSERT_CITATION_MAPPINGS_SQL,
            "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id,
             cited_object_id, owner_kind,
             owner_id)
         SELECT * FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::uuid[],
                              $5::proxima_core.owner_ref_kind[], $6::uuid[])"
        );
        assert_eq!(
            INSERT_CHANGE_EVENTS_SQL,
            "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind,
             entity_memory_id, entity_schema_id,
             entity_schema_version)
         SELECT t.seq, t.owner_kind, t.owner_id, 'EntityAppend', 'Fact',
                t.memory_id, t.schema_id, t.schema_version
           FROM unnest($1::uuid[], $2::proxima_core.owner_ref_kind[],
                       $3::uuid[], $4::uuid[], $5::text[], $6::int[])
                AS t(seq, owner_kind, owner_id, memory_id, schema_id,
                     schema_version)"
        );
        assert_eq!(
            SELECT_FIRST_CHANGE_SEQS_SQL,
            "SELECT DISTINCT ON (entity_memory_id)
                entity_memory_id, seq
           FROM proxima_core.change_event
          WHERE entity_memory_id = ANY($1::uuid[])
          ORDER BY entity_memory_id, seq ASC"
        );
        assert_eq!(
            SELECT_CITED_OBJECTS_SQL,
            "SELECT m.memory_id, cm.cited_object_id
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm USING (citation_mapping_id)
          WHERE m.memory_id = ANY($1::uuid[])"
        );
    }

    /// An empty batch is answered without a connection, on both arms of the
    /// flag: the pool below never resolves, so a batch that opened a
    /// transaction to write nothing would fail here rather than in a
    /// harness.
    #[tokio::test]
    async fn an_empty_batch_never_reaches_the_pool() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(1))
            .connect_lazy("postgres://proxima-batch-test.invalid/none")
            .expect("a lazy pool does not connect");
        let sidecars = PgSidecarRegistryFrozen::default();

        for batched_writes in [false, true] {
            let tuning = PgTuning {
                batched_writes,
                ..PgTuning::default()
            };
            assert!(
                ingest_facts_batch(&pool, &tuning, &sidecars, &[])
                    .await
                    .expect("an empty batch writes nothing")
                    .is_empty()
            );
        }
    }
}

// The batched path's rows are the point, so its tests are DB-backed and
// in-crate: `ingest_facts_batch_in_tx` takes a transaction the caller owns,
// which no external test binary can hand it.
#[cfg(test)]
mod pg_tests {
    use proxima_core::test_fixtures::owner_fixture;
    use proxima_core::verbs::fact_ingest::{CitationSpec, FactWriteCommand};
    use proxima_core::{
        AuthPath, AuthzContext, Engine, FactPayload, FlavorRegistry, Owner, Relation,
        SidecarPayload, SourceBatchId, Speaker, UtteranceV1,
    };
    use proxima_pg_testkit::drop_db;
    use uuid::Uuid;

    use super::{FactIngestBatchUnit, ingest_facts_batch};
    use crate::PgTuning;
    use crate::test_fixtures::fresh_pg;

    const EMBEDDING_MODEL: &str = "stub-batch-embed";

    /// Batched writes at a size that actually batches: the default is one
    /// unit per transaction, which is the amortization curve's left end and
    /// not the path these tests are about.
    fn batched_tuning(static_lookup_cache: bool) -> PgTuning {
        PgTuning {
            batched_writes: true,
            write_batch_size: 100,
            static_lookup_cache,
            ..PgTuning::default()
        }
    }

    fn utterance(text: &str) -> UtteranceV1 {
        UtteranceV1 {
            speaker: Speaker::User,
            conversation_id: "batch-conversation".into(),
            text: text.into(),
        }
    }

    /// The chat-ingest unit `core_record_utterance` builds, minus the
    /// transport: one authorized Fact plus the typed sidecar payload it
    /// carries.
    async fn authorized_utterance(
        engine: &Engine,
        authz: &AuthzContext,
        source_id: &str,
        payload: &UtteranceV1,
    ) -> Result<(proxima_core::AuthorizedFactWrite, Vec<SidecarPayload>), Box<dyn std::error::Error>>
    {
        let draft = FactWriteCommand::from_payload(
            source_id,
            SourceBatchId::new(Uuid::now_v7()),
            payload,
            time::OffsetDateTime::now_utc(),
        );
        let sidecars = vec![SidecarPayload::fact(payload.clone())];
        let authorized = engine
            .authorize_fact_ingest(authz, Relation::Editor, draft, &sidecars)
            .await?;
        Ok((authorized, sidecars))
    }

    /// Every table one landed chat turn writes.
    async fn row_counts(pool: &sqlx::PgPool) -> Result<[i64; 5], sqlx::Error> {
        let mut counts = [0_i64; 5];
        for (slot, table) in [
            "proxima_core.memories",
            "proxima_core.fact_receipts",
            "proxima_core.utterance_v1",
            "proxima_core.change_event",
            "proxima_core.embedding_jobs",
        ]
        .iter()
        .enumerate()
        {
            // SQL-POLICY: the table names are the literals above, not input.
            counts[slot] =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                    .fetch_one(pool)
                    .await?;
        }
        Ok(counts)
    }

    fn authz_for(owner: &Owner) -> AuthzContext {
        let Owner::Personal(user_id) = owner else {
            panic!("batched ingest test fixture expects a personal owner");
        };
        AuthzContext::for_subject(*user_id, AuthPath::HostBearer)
    }

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    /// The whole flag in one run: a batch lands every fresh unit, detects
    /// the unit that repeats an earlier unit's receipt as the replay it is,
    /// and re-running the same batch replays all of it without writing a
    /// second row anywhere.
    #[tokio::test]
    async fn a_batch_lands_its_fresh_units_and_replays_the_repeated_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_batch").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let authz = authz_for(&owner);
            let engine = engine();
            let tuning = batched_tuning(true);

            let payloads = [utterance("first turn"), utterance("second turn")];
            let mut authorized = Vec::new();
            for payload in &payloads {
                authorized
                    .push(authorized_utterance(&engine, &authz, "test/batch", payload).await?);
            }
            // A third unit repeating the FIRST unit's source and payload:
            // same receipt id, no committed row to probe for.
            authorized
                .push(authorized_utterance(&engine, &authz, "test/batch", &payloads[0]).await?);

            let units: Vec<FactIngestBatchUnit<'_>> = authorized
                .iter()
                .map(|(authorized, sidecars)| FactIngestBatchUnit {
                    authorized,
                    sidecar_payloads: sidecars,
                    embedding_model_id: Some(EMBEDDING_MODEL),
                })
                .collect();

            let outcomes =
                ingest_facts_batch(pg.pool_for_tests(), &tuning, pg.sidecars(), &units).await?;

            assert_eq!(outcomes.len(), 3);
            assert!(!outcomes[0].idempotent_replay);
            assert!(!outcomes[1].idempotent_replay);
            assert!(
                outcomes[2].idempotent_replay,
                "the repeated receipt replays"
            );
            assert_eq!(outcomes[2].memory_id, outcomes[0].memory_id);
            assert_eq!(outcomes[2].change_event_seq, outcomes[0].change_event_seq);
            assert_ne!(outcomes[0].memory_id, outcomes[1].memory_id);
            assert_eq!(row_counts(pg.pool_for_tests()).await?, [2, 2, 2, 2, 2]);

            // The same batch again: every unit is now a receipt replay
            // resolved against committed rows, and nothing is written.
            let replayed =
                ingest_facts_batch(pg.pool_for_tests(), &tuning, pg.sidecars(), &units).await?;
            assert!(replayed.iter().all(|outcome| outcome.idempotent_replay));
            assert_eq!(replayed[0].memory_id, outcomes[0].memory_id);
            assert_eq!(replayed[1].memory_id, outcomes[1].memory_id);
            assert_eq!(replayed[2].memory_id, outcomes[0].memory_id);
            assert_eq!(row_counts(pg.pool_for_tests()).await?, [2, 2, 2, 2, 2]);
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result
    }

    /// A citation on a batched draft: the cited object is upserted per Fact
    /// (it is content-addressed and shared), and the mapping rows land in
    /// one statement, after the memories rows their deferred foreign key
    /// points at.
    #[tokio::test]
    async fn a_batch_lands_the_citation_mappings_its_drafts_carry()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_batch").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let authz = authz_for(&owner);
            let engine = engine();
            let payloads = [utterance("cited one"), utterance("cited two")];
            let mut authorized = Vec::new();
            for (index, payload) in payloads.iter().enumerate() {
                let draft = FactWriteCommand::from_payload(
                    "test/citation",
                    SourceBatchId::new(Uuid::now_v7()),
                    payload,
                    time::OffsetDateTime::now_utc(),
                )
                .with_citation(CitationSpec::v1(
                    proxima_core::citations::UPLOADED_BLOB_SCHEMA_ID,
                    [u8::try_from(index).unwrap_or_default(); 32],
                    proxima_core::citations::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                ));
                let sidecars = vec![SidecarPayload::fact(payload.clone())];
                authorized.push((
                    engine
                        .authorize_fact_ingest(&authz, Relation::Editor, draft, &sidecars)
                        .await?,
                    sidecars,
                ));
            }
            let units: Vec<FactIngestBatchUnit<'_>> = authorized
                .iter()
                .map(|(authorized, sidecars)| FactIngestBatchUnit {
                    authorized,
                    sidecar_payloads: sidecars,
                    embedding_model_id: None,
                })
                .collect();

            let outcomes = ingest_facts_batch(
                pg.pool_for_tests(),
                &batched_tuning(false),
                pg.sidecars(),
                &units,
            )
            .await?;

            assert!(
                outcomes
                    .iter()
                    .all(|outcome| outcome.cited_object_id.is_some())
            );
            let mappings: Vec<(Uuid, Uuid)> = sqlx::query_as(
                "SELECT m.memory_id, cm.cited_object_id
                   FROM proxima_core.memories m
                   JOIN proxima_core.citation_mappings cm
                     USING (citation_mapping_id)
                  ORDER BY m.text",
            )
            .fetch_all(pg.pool_for_tests())
            .await?;
            assert_eq!(mappings.len(), 2);
            for (index, outcome) in outcomes.iter().enumerate() {
                assert_eq!(mappings[index].0, outcome.memory_id.into_inner());
                assert_eq!(Some(mappings[index].1), outcome.cited_object_id);
            }
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result
    }

    /// The two arms of the static-lookup flag admit the same batch and
    /// stamp the same language: with the flag on, one suppression probe and
    /// a process-cached catalog check; with it off, the per-unit probe and
    /// the catalog check the serial path issues.
    #[tokio::test]
    async fn both_static_lookup_arms_land_the_same_language_stamped_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_batch").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let authz = authz_for(&owner);
            let engine = engine();

            for (index, static_lookup_cache) in [true, false].into_iter().enumerate() {
                let payloads = [
                    utterance(&format!("cached {index} one")),
                    utterance(&format!("cached {index} two")),
                ];
                let mut authorized = Vec::new();
                for payload in &payloads {
                    let draft = FactWriteCommand::from_payload(
                        "test/language",
                        SourceBatchId::new(Uuid::now_v7()),
                        payload,
                        time::OffsetDateTime::now_utc(),
                    )
                    .with_lexical_language(Some("english".into()));
                    let sidecars = vec![SidecarPayload::fact(payload.clone())];
                    authorized.push((
                        engine
                            .authorize_fact_ingest(&authz, Relation::Editor, draft, &sidecars)
                            .await?,
                        sidecars,
                    ));
                }
                let units: Vec<FactIngestBatchUnit<'_>> = authorized
                    .iter()
                    .map(|(authorized, sidecars)| FactIngestBatchUnit {
                        authorized,
                        sidecar_payloads: sidecars,
                        embedding_model_id: None,
                    })
                    .collect();

                let outcomes = ingest_facts_batch(
                    pg.pool_for_tests(),
                    &batched_tuning(static_lookup_cache),
                    pg.sidecars(),
                    &units,
                )
                .await?;
                assert!(outcomes.iter().all(|outcome| !outcome.idempotent_replay));
            }

            let stamped: Vec<String> =
                sqlx::query_scalar("SELECT lexical_language::text FROM proxima_core.memories")
                    .fetch_all(pg.pool_for_tests())
                    .await?;
            assert_eq!(stamped.len(), 4);
            assert!(stamped.iter().all(|language| language == "english"));
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result
    }

    /// A receiptless unit beside a receipted one, and a suppressed receipt
    /// behind it.
    ///
    /// Receiptless Facts carry no key material, so they contribute no probe;
    /// the unit they precede must still be admitted against ITS receipt, and
    /// a suppressed receipt refuses the batch on both arms of the flag.
    #[tokio::test]
    async fn a_suppressed_receipt_refuses_the_batch_it_arrives_in()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_batch").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let authz = authz_for(&owner);
            let engine = engine();
            let receiptless = |text: &str| {
                let payload = utterance(text);
                FactWriteCommand {
                    schema_id: proxima_core::UtteranceV1::schema_id(),
                    schema_version: proxima_core::SchemaVersion::new(1),
                    payload: payload.receipt_key(),
                    rendered_text: Some(text.to_string()),
                    lexical_language: None,
                    receipt: None,
                    citation: None,
                    derived_from: Vec::new(),
                }
            };

            let suppressed_payload = utterance("suppressed turn");
            let suppressed = authorized_utterance(
                &engine,
                &authz,
                "test/suppressed",
                &suppressed_payload,
            )
            .await?;
            let receipt_id = suppressed
                .0
                .draft()
                .receipt_id_for_owner(owner)
                .expect("a receipted draft has a receipt id");

            // The rows an Art. 17 erase of that receipt's content leaves
            // behind, written directly so the test does not have to run one.
            let operation_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.compliance_audit_log
                    (operation_id, target_kind, outcome, owner_ref_digest,
                     derived_auth_path, requested_at)
                 VALUES ($1, 'owner', 'Completed', '\\x00'::bytea, 'test', now())",
            )
            .bind(operation_id)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.compliance_suppression_keys
                    (key_class, suppression_key, operation_id)
                 VALUES ('receipt_content'::proxima_core.compliance_suppression_key_class,
                         decode(md5($1 || chr(31) || 'receipt_content' || chr(31) || $2 || chr(31) || $3 || chr(31) || coalesce($4, '') || chr(31) || encode($5::bytea, 'hex')), 'hex'),
                         $6)",
            )
            .bind("proxima-compliance-suppression-v2")
            .bind(proxima_core::OwnerRefKind::of(&owner).as_str())
            .bind(owner.stable_key_uuid().to_string())
            .bind(owner.stable_key_uuid().to_string())
            .bind(&receipt_id.into_inner()[..])
            .bind(operation_id)
            .execute(pg.pool_for_tests())
            .await?;

            for static_lookup_cache in [true, false] {
                let open = receiptless(&format!("open turn {static_lookup_cache}"));
                let open_sidecars =
                    vec![SidecarPayload::fact(utterance(&format!(
                        "open turn {static_lookup_cache}"
                    )))];
                let open = engine
                    .authorize_fact_ingest(&authz, Relation::Editor, open, &open_sidecars)
                    .await?;
                let units = [
                    FactIngestBatchUnit {
                        authorized: &open,
                        sidecar_payloads: &open_sidecars,
                        embedding_model_id: None,
                    },
                    FactIngestBatchUnit {
                        authorized: &suppressed.0,
                        sidecar_payloads: &suppressed.1,
                        embedding_model_id: None,
                    },
                ];
                let refused = ingest_facts_batch(
                    pg.pool_for_tests(),
                    &batched_tuning(static_lookup_cache),
                    pg.sidecars(),
                    &units,
                )
                .await;
                assert!(
                    matches!(refused, Err(proxima_core::StorageError::Suppressed(_))),
                    "cache={static_lookup_cache} reported {refused:?}"
                );
            }
            assert_eq!(row_counts(pg.pool_for_tests()).await?, [0, 0, 0, 0, 0]);
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result
    }

    /// A closed source batch refuses the whole batch, with the sentence the
    /// per-unit path uses.
    #[tokio::test]
    async fn a_closed_source_batch_refuses_the_batch() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_batch").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let authz = authz_for(&owner);
            let engine = engine();
            let tuning = batched_tuning(false);
            let source_batch_id = SourceBatchId::new(Uuid::now_v7());

            let land = async |text: &str| -> Result<
                Vec<proxima_core::verbs::fact_ingest::FactIngestOutcome>,
                Box<dyn std::error::Error>,
            > {
                let payload = utterance(text);
                let draft = FactWriteCommand::from_payload(
                    "test/closed",
                    source_batch_id,
                    &payload,
                    time::OffsetDateTime::now_utc(),
                );
                let sidecars = vec![SidecarPayload::fact(payload.clone())];
                let authorized = engine
                    .authorize_fact_ingest(&authz, Relation::Editor, draft, &sidecars)
                    .await?;
                let units = [FactIngestBatchUnit {
                    authorized: &authorized,
                    sidecar_payloads: &sidecars,
                    embedding_model_id: None,
                }];
                Ok(ingest_facts_batch(pg.pool_for_tests(), &tuning, pg.sidecars(), &units).await?)
            };

            land("open batch turn").await?;
            sqlx::query("UPDATE proxima_core.source_batches SET closed_at = now() WHERE id = $1")
                .bind(source_batch_id.into_inner())
                .execute(pg.pool_for_tests())
                .await?;

            let refused = land("closed batch turn").await;
            let message = refused.err().map(|err| err.to_string()).unwrap_or_default();
            assert!(
                message.contains("cannot ingest Fact into closed source batch"),
                "closed batch reported {message:?}"
            );
            assert_eq!(row_counts(pg.pool_for_tests()).await?, [1, 1, 1, 1, 0]);
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result
    }

    /// A batch of one has to land what the serial path lands: the same rows
    /// in the same tables, and an outcome of the same shape.
    #[tokio::test]
    async fn a_batch_of_one_lands_what_the_serial_path_lands()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_batch").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let authz = authz_for(&owner);
            let engine = engine();

            let serial_payload = utterance("serial turn");
            let batched_payload = utterance("batched turn");
            let serial =
                authorized_utterance(&engine, &authz, "test/serial", &serial_payload).await?;
            let batched =
                authorized_utterance(&engine, &authz, "test/batched", &batched_payload).await?;

            let serial_unit = [FactIngestBatchUnit {
                authorized: &serial.0,
                sidecar_payloads: &serial.1,
                embedding_model_id: Some(EMBEDDING_MODEL),
            }];
            let batched_unit = [FactIngestBatchUnit {
                authorized: &batched.0,
                sidecar_payloads: &batched.1,
                embedding_model_id: Some(EMBEDDING_MODEL),
            }];

            let serial_outcome = ingest_facts_batch(
                pg.pool_for_tests(),
                &PgTuning::default(),
                pg.sidecars(),
                &serial_unit,
            )
            .await?;
            let after_serial = row_counts(pg.pool_for_tests()).await?;

            let batched_outcome = ingest_facts_batch(
                pg.pool_for_tests(),
                &batched_tuning(true),
                pg.sidecars(),
                &batched_unit,
            )
            .await?;
            let after_batched = row_counts(pg.pool_for_tests()).await?;

            assert_eq!(after_serial, [1, 1, 1, 1, 1]);
            assert_eq!(after_batched, [2, 2, 2, 2, 2]);
            assert_eq!(
                serial_outcome[0].idempotent_replay,
                batched_outcome[0].idempotent_replay
            );
            assert_eq!(
                serial_outcome[0].receipt_id.is_some(),
                batched_outcome[0].receipt_id.is_some()
            );
            assert_eq!(
                serial_outcome[0].cited_object_id,
                batched_outcome[0].cited_object_id
            );

            // The sidecar row the batched insert wrote is the row the
            // per-row insert writes, column for column.
            let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
                "SELECT memory_id, speaker::text, conversation_id, text
                   FROM proxima_core.utterance_v1
                  ORDER BY text",
            )
            .fetch_all(pg.pool_for_tests())
            .await?;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].0, batched_outcome[0].memory_id.into_inner());
            assert_eq!(rows[0].1, "user");
            assert_eq!(rows[0].2, "batch-conversation");
            assert_eq!(rows[0].3, "batched turn");
            assert_eq!(rows[1].0, serial_outcome[0].memory_id.into_inner());
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result
    }
}
