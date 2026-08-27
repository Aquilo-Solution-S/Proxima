use proxima_core::flavor::LanguagePolicy;
use proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT;
use proxima_core::verbs::fact_ingest::FactWriteCommand;

use super::{
    Arc, GoalId, HashMap, HashSet, MemoryId, PgSidecarEntry, PgSidecarKey, PgSidecarReadCtx,
    Postgres, SchemaInfo, SidecarPayload, StorageError, Transaction,
};

// The two suites that write a sidecar row through the registry deliberately —
// to prove the projection row follows it, and that a transfer moves both.
// `PgSidecarWriter::insert_memory_sidecar` is `pub(crate)`, so they live
// beside it. Their goldens stay in `tests/golden/` with the erase
// differential's; three pinned baselines in one place beat three beside
// three readers.
#[cfg(test)]
#[path = "projection_maintenance_pg_tests.rs"]
mod projection_maintenance_pg_tests;

#[cfg(test)]
#[path = "owner_transfer_differential_pg_tests.rs"]
mod owner_transfer_differential_pg_tests;

#[derive(Debug, Clone, Default)]
pub struct PgSidecarRegistryFrozen {
    pub(super) entries: Arc<HashMap<PgSidecarKey, PgSidecarEntry>>,
}

impl PgSidecarRegistryFrozen {
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn contains(&self, key: &PgSidecarKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Tables actually inserted for these payloads, in name order.
    ///
    /// # Errors
    ///
    /// `ConstraintViolation` when a payload has no PG memory sidecar.
    pub fn tables_for_payloads(
        &self,
        payloads: &[SidecarPayload],
    ) -> Result<Vec<String>, StorageError> {
        let mut tables = Vec::new();
        for payload in payloads {
            let key = PgSidecarKey::new(
                payload.kind,
                payload.schema_id.clone(),
                payload.schema_version,
            );
            let table = self
                .entries
                .get(&key)
                .ok_or_else(|| {
                    StorageError::ConstraintViolation(format!(
                        "no PG sidecar registered for {} v{} {:?}",
                        key.schema_id.as_str(),
                        key.schema_version.into_inner(),
                        key.kind
                    ))
                })?
                .sidecar_table
                .clone();
            if !tables.contains(&table) {
                tables.push(table);
            }
        }
        tables.sort();
        Ok(tables)
    }

    #[must_use]
    pub fn table_for_schema(
        &self,
        kind: proxima_core::verbs::schema::PayloadKind,
        schema_id: &proxima_core::SchemaId,
        schema_version: proxima_core::SchemaVersion,
    ) -> Option<&str> {
        let key = PgSidecarKey::new(kind, schema_id.clone(), schema_version);
        self.entries
            .get(&key)
            .map(|entry| entry.sidecar_table.as_str())
    }

    /// The column `table` stores its memory `t` under, as its registration
    /// declares it (`pg_sidecar!(key: …)`), or `None` when `table` is not a
    /// registered memory sidecar.
    ///
    /// The one reader is the series-head lookup, which joins a sidecar back
    /// to `proxima_core.memory` knowing only the table name. Freeze has
    /// already held this equal to the contract's
    /// `KeyShape::MemoryT { column }` for the same table, so the two sides
    /// of that join are one declaration read twice, not two spellings.
    #[must_use]
    pub fn memory_key_column(&self, table: &str) -> Option<&'static str> {
        self.entries
            .values()
            .filter(|entry| entry.sidecar_table == table)
            .find_map(|entry| entry.memory_key_column)
    }

    #[must_use]
    pub fn is_memory_sidecar_table(&self, table: &str) -> bool {
        self.entries.values().any(|entry| {
            (entry.memory_insert.is_some() || entry.memory_load_batch.is_some())
                && entry.sidecar_table == table
        })
    }

    /// Test-only: a registered memory table that does not exist in PG.
    /// Forget must not SELECT it unless the row stamped it.
    #[must_use]
    pub fn with_unusable_memory_table(&self, schema_id: &str, table: &str) -> Self {
        use proxima_core::verbs::schema::PayloadKind;
        use proxima_core::{SchemaId, SchemaVersion};
        let mut entries = (*self.entries).clone();
        let key = PgSidecarKey::new(
            PayloadKind::Fact,
            SchemaId::new(schema_id.to_owned()),
            SchemaVersion::new(1),
        );
        entries.insert(
            key.clone(),
            PgSidecarEntry {
                key,
                sidecar_table: table.to_owned(),
                owner_pinned: false,
                memory_key_column: Some("t"),
                memory_insert: Some(|_, _, _, _| Box::pin(async { Ok(()) })),
                memory_load: None,
                memory_load_batch: Some(|_, _, _| Box::pin(async { Ok(Vec::new()) })),
                cited_object_insert: None,
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
                projection_insert: None,
                projection_table: None,
                projection_language: None,
            },
        );
        Self {
            entries: Arc::new(entries),
        }
    }

    /// Distinct memory sidecar tables (core + flavor), for forget/hydrate.
    #[must_use]
    pub fn memory_sidecar_tables(&self) -> Vec<&str> {
        let mut tables: Vec<&str> = self
            .entries
            .values()
            .filter(|entry| entry.memory_insert.is_some() || entry.memory_load_batch.is_some())
            .map(|entry| entry.sidecar_table.as_str())
            .collect();
        tables.sort_unstable();
        tables.dedup();
        tables
    }

    /// Memory sidecar tables that carry their own `owner_id` (see
    /// [`super::PgMemoryPayload::OWNER_PINNED`]), in name order.
    ///
    /// Owner erase and export select these by the sidecar's OWN owner
    /// rather than through the Memory, because a transfer leaves them
    /// behind: joining through the Memory would put them out of the writing
    /// owner's reach and into the receiving owner's bundle.
    #[must_use]
    pub fn owner_pinned_memory_sidecar_tables(&self) -> Vec<String> {
        let mut tables: Vec<String> = self
            .entries
            .values()
            .filter(|entry| {
                entry.owner_pinned
                    && (entry.memory_insert.is_some() || entry.memory_load_batch.is_some())
            })
            .map(|entry| entry.sidecar_table.clone())
            .collect();
        tables.sort_unstable();
        tables.dedup();
        tables
    }

    #[must_use]
    pub(crate) fn is_owner_pinned_memory_sidecar_table(&self, table: &str) -> bool {
        self.entries.values().any(|entry| {
            entry.sidecar_table == table
                && entry.owner_pinned
                && (entry.memory_insert.is_some() || entry.memory_load_batch.is_some())
        })
    }

    /// Resolve the primary memory sidecar table by the stored `(kind,
    /// schema_id)` selector. Memory rows do not carry a schema version, and
    /// F/A/P registry freeze guarantees this lookup is unique.
    #[must_use]
    pub(crate) fn memory_sidecar_table_for_schema(
        &self,
        kind: proxima_core::EntityKind,
        schema_id: &str,
    ) -> Option<&str> {
        let kind = match kind {
            proxima_core::EntityKind::Fact => proxima_core::verbs::schema::PayloadKind::Fact,
            proxima_core::EntityKind::Abstraction => {
                proxima_core::verbs::schema::PayloadKind::Abstraction
            }
            proxima_core::EntityKind::Perspective => {
                proxima_core::verbs::schema::PayloadKind::Perspective
            }
            proxima_core::EntityKind::Goal => return None,
        };
        self.entries.values().find_map(|entry| {
            (entry.key.kind == kind
                && entry.key.schema_id.as_str() == schema_id
                && entry.memory_insert.is_some())
            .then_some(entry.sidecar_table.as_str())
        })
    }

    #[must_use]
    pub fn missing_for(&self, schemas: &[SchemaInfo]) -> Vec<PgSidecarKey> {
        let mut missing = schemas
            .iter()
            .filter(|schema| schema.sidecar_table.is_some())
            .map(|schema| {
                PgSidecarKey::new(schema.kind, schema.schema_id.clone(), schema.schema_version)
            })
            .filter(|key| !self.contains(key))
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        missing.retain(|key| seen.insert(key.clone()));
        missing
    }

    /// This registry bound to ONE authorized write.
    ///
    /// The language stamped on a projection row is the WRITE's, so it is
    /// taken off the authorized draft here rather than travelling as its
    /// own argument through the verbs and closures between the port and
    /// the insert. See [`PgSidecarWriter`].
    pub(crate) fn writing(&self, draft: &FactWriteCommand) -> PgSidecarWriter {
        PgSidecarWriter {
            registry: self.clone(),
            draft_language: draft.lexical_language.clone(),
        }
    }

    /// This registry bound to one authorized DERIVED write — the same
    /// binding as [`Self::writing`], for the draft the derived path carries.
    pub(crate) fn writing_derived(
        &self,
        draft: &crate::verbs::derive_append::DerivedDraft<'_>,
    ) -> PgSidecarWriter {
        PgSidecarWriter {
            registry: self.clone(),
            draft_language: draft.lexical_language.map(str::to_owned),
        }
    }

    /// Rebuild the projection row for one already-restored sidecar row.
    ///
    /// The one public mutating method left on the frozen registry, and the
    /// reason it may stay public is the reason it needs no
    /// [`super::SidecarInsertPermit`]: rebuild IS the maintenance. It
    /// re-derives a projection row FROM a sidecar row that already exists,
    /// so invoking it can only restore the invariant the permit protects,
    /// never break it. Who may trigger a full-table rebuild is a cost and
    /// authorization question, not an integrity one, and it already sits
    /// behind a registry handle only the host context holds.
    ///
    /// Hydrate restores sidecar rows generically from the cold dump, so it
    /// cannot go through `PgSidecarWriter::insert_memory_sidecar`. It
    /// re-derives the projection from the restored row instead — the same
    /// statement, run against a row that is already there.
    ///
    /// `lexical_language` is a RESTORE input, not a write's declaration,
    /// which is why this path takes one where the write path does not and
    /// why `None` here is the deployment default rather than a refusal:
    /// the cold record carries no stamp, so there is no writer's choice
    /// left to honour or to refuse for. Carrying it through a
    /// forget/hydrate cycle is a cold-format change.
    ///
    /// `schema_id` is the RESTORED ROW's, and it selects the entry as well as
    /// filling the bind. `entries` is keyed by `(schema_id, version, kind)`, so
    /// selecting by table alone matches whichever entry for that table comes
    /// first and runs for every dumped table, including the extra sidecars a
    /// memory may be stamped with. A memory has one `schema_id` and
    /// `projection` is keyed `(memory_id, schema_id)`, so at most one of those
    /// tables can produce a row: the one belonging to the memory's own schema,
    /// which is what the write path does. Selecting on both leaves no accident
    /// in either direction.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the generated statement.
    pub async fn rebuild_projection_for_table(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        table: &str,
        schema_id: &str,
        lexical_language: Option<&str>,
    ) -> Result<(), StorageError> {
        let Some(entry) = self.entries.values().find(|entry| {
            entry.sidecar_table == table
                && entry.key.schema_id.as_str() == schema_id
                && entry.projection_insert.is_some()
        }) else {
            return Ok(());
        };
        let Some(sql) = entry.projection_insert.as_deref() else {
            return Ok(());
        };
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(memory_id.into_inner())
            .bind(lexical_language)
            .bind(schema_id)
            .execute(tx.as_mut())
            .await
            .map_err(crate::error::map_err)?;
        Ok(())
    }

    /// The declaration triggers ONE flavor's registered memory sidecars
    /// need, in table order.
    ///
    /// Selected by flavor rather than emitted whole, because the DDL lands
    /// in that flavor's own migration and a flavor's migration may only
    /// create objects on tables its own migrations created. The flavor is
    /// read off the schema id's prefix, which is where `attach_projections`
    /// reads it too, so a table's migration and its trigger are chosen by
    /// one rule.
    ///
    /// [`crate::integrity::DECLARATION_TRIGGER_FUNCTION`] is deliberately
    /// NOT part of this list: it is one function for every flavor, so it
    /// lives once, in core's `0002_v009_declaration_triggers.sql`, and a
    /// flavor's migration carries only the triggers that call it.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when a registered table, its declared memory-key
    /// column or the trigger name derived from it is not a legal
    /// `PostgreSQL` identifier, or when a registered memory sidecar
    /// declares no memory-key column.
    pub fn declaration_trigger_artifacts(
        &self,
        flavor_id: &str,
    ) -> Result<Vec<crate::projection::Artifact>, StorageError> {
        let mut tables: Vec<(&str, &'static str)> = self
            .entries
            .values()
            .filter(|entry| entry.memory_insert.is_some() || entry.memory_load_batch.is_some())
            .filter(|entry| {
                entry
                    .key
                    .schema_id
                    .as_str()
                    .split_once('/')
                    .is_some_and(|(prefix, _)| prefix == flavor_id)
            })
            .map(|entry| {
                let column = entry.memory_key_column.ok_or_else(|| {
                    StorageError::Internal(format!(
                        "{} is a registered memory sidecar of flavor {flavor_id} with no \
                         declared memory-key column",
                        entry.sidecar_table
                    ))
                })?;
                Ok((entry.sidecar_table.as_str(), column))
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        tables.sort_unstable();
        tables.dedup();
        tables
            .into_iter()
            .map(|(table, column)| crate::integrity::declaration_trigger(table, column))
            .collect()
    }

    /// The sibling of [`Self::rebuild_projection_for_table`]: what rebuild
    /// repairs, asked of every row already there.
    ///
    /// Two classes, both hard errors and never warnings, because both are
    /// rows a lane is already walking past:
    ///
    /// 1. A projected schema's sidecar row with no projection row. Search
    ///    ranks on the projection, so the row is not searchable. Repairable
    ///    — [`Self::rebuild_projection_for_table`] re-derives the projection
    ///    row from the sidecar row, and the finding names it.
    /// 2. A row in a registered memory-sidecar table whose memory does not
    ///    declare that table. Forget, owner erase and owner export all walk
    ///    `memory.sidecar_tables`, so the row is reachable by none of them.
    ///    NOT repairable, and the finding says so: the row is outside every
    ///    declaration, so nothing records what it was meant to be part of.
    ///    With the declaration trigger installed this class is unreachable
    ///    through `INSERT`; the check exists for rows written before it, and
    ///    for a database whose trigger someone dropped.
    ///
    /// Every registered flavor is checked, so a downstream flavor's CI can
    /// run this once after its own ingest suite and assert zero drift.
    /// Under the `proxima` crate's `testkit` feature that is one line:
    ///
    /// ```ignore
    /// embedded.pg_sidecars.integrity_check(embedded.pool_for_tests()).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// [`IntegrityViolation::Drift`] listing every finding, or
    /// [`IntegrityViolation::Storage`] when a statement could not run.
    ///
    /// [`IntegrityViolation::Drift`]: crate::integrity::IntegrityViolation::Drift
    /// [`IntegrityViolation::Storage`]: crate::integrity::IntegrityViolation::Storage
    pub async fn integrity_check(
        &self,
        pool: &sqlx::PgPool,
    ) -> Result<crate::integrity::IntegrityReport, crate::integrity::IntegrityViolation> {
        use crate::integrity::{IntegrityFinding, IntegrityReport, ProjectedSchema};

        let mut findings = Vec::new();

        // Class one. Keyed on `(table, schema)` rather than on the table:
        // sibling registrations may share one sidecar table and the
        // projection is keyed `(memory_id, schema_id)`.
        let mut projected: Vec<(&str, &str, &str, &'static str)> = self
            .entries
            .values()
            .filter_map(|entry| {
                let table = entry.projection_table.as_deref()?;
                entry.projection_insert.as_deref()?;
                let key = entry.memory_key_column?;
                Some((
                    entry.sidecar_table.as_str(),
                    entry.key.schema_id.as_str(),
                    table,
                    key,
                ))
            })
            .collect();
        projected.sort_unstable();
        projected.dedup();

        let mut projected_schemas = Vec::with_capacity(projected.len());
        for (sidecar_table, schema_id, projection_table, key) in projected {
            let sql = crate::integrity::unprojected_rows_sql(sidecar_table, key, projection_table)?;
            // SQL-POLICY: generated
            let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(schema_id)
                .fetch_one(pool)
                .await
                .map_err(crate::error::map_err)?;
            projected_schemas.push(ProjectedSchema {
                sidecar_table: sidecar_table.to_owned(),
                schema_id: schema_id.to_owned(),
                projection_table: projection_table.to_owned(),
            });
            if rows > 0 {
                findings.push(IntegrityFinding::UnprojectedSidecarRows {
                    sidecar_table: sidecar_table.to_owned(),
                    schema_id: schema_id.to_owned(),
                    projection_table: projection_table.to_owned(),
                    rows,
                });
            }
        }

        // Class two. Per TABLE, not per schema: the stamp names a table.
        let mut declared_tables = Vec::new();
        for table in self.memory_sidecar_tables() {
            let key = self.memory_key_column(table).ok_or_else(|| {
                StorageError::Internal(format!(
                    "{table} is a registered memory sidecar with no declared memory-key column"
                ))
            })?;
            let sql = crate::integrity::undeclared_rows_sql(table, key)?;
            // SQL-POLICY: generated
            let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(table)
                .fetch_one(pool)
                .await
                .map_err(crate::error::map_err)?;
            declared_tables.push(table.to_owned());
            if rows > 0 {
                findings.push(IntegrityFinding::UndeclaredSidecarRows {
                    sidecar_table: table.to_owned(),
                    rows,
                });
            }
        }

        if findings.is_empty() {
            Ok(IntegrityReport {
                declared_tables,
                projected_schemas,
            })
        } else {
            Err(crate::integrity::IntegrityViolation::Drift(findings))
        }
    }

    /// Every projection table the linked flavors maintain, in name order.
    ///
    /// Transfer walks this rather than a hardcoded list, so a flavor that
    /// declares a projection gets owner-following for free and one that
    /// does not contributes nothing.
    #[must_use]
    pub fn projection_tables(&self) -> Vec<String> {
        let mut tables: Vec<String> = self
            .entries
            .values()
            .filter_map(|entry| entry.projection_table.clone())
            .collect();
        tables.sort_unstable();
        tables.dedup();
        tables
    }

    /// Load a typed sidecar payload projection for an already-created
    /// Memory row.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG memory sidecar is
    /// registered for the schema. Returns storage errors from the
    /// concrete loader.
    pub async fn load_memory_payload(
        &self,
        ctx: PgSidecarReadCtx<'_>,
        key: PgSidecarKey,
        memory_id: MemoryId,
    ) -> Result<Option<SidecarPayload>, StorageError> {
        let mut rows = self
            .load_memory_payloads_batch(ctx, &key, &[memory_id])
            .await?;
        Ok(rows.pop().map(|(_memory_id, payload)| payload))
    }

    /// Load typed sidecar payload projections for already-created Memory rows.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG memory sidecar is
    /// registered for the schema. Returns storage errors from the
    /// concrete loader.
    pub async fn load_memory_payloads_batch(
        &self,
        ctx: PgSidecarReadCtx<'_>,
        key: &PgSidecarKey,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<(MemoryId, SidecarPayload)>, StorageError> {
        let entry = self.entries.get(key).ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "no PG sidecar registered for {} v{} {:?}",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        let load = entry.memory_load_batch.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not a memory sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        load(
            ctx.for_registered_table(&entry.sidecar_table),
            key.kind,
            memory_ids,
        )
        .await
    }

    /// Insert a typed sidecar row for an already-created Goal row.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG Goal sidecar is registered
    /// for the payload schema or when the erased payload type does not match
    /// the registered Rust type. Returns storage errors from the concrete
    /// inserter.
    pub(crate) async fn insert_goal_sidecar(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        goal_id: GoalId,
        payload: &SidecarPayload,
    ) -> Result<(), StorageError> {
        let key = PgSidecarKey::new(
            payload.kind,
            payload.schema_id.clone(),
            payload.schema_version,
        );
        let entry = self.entries.get(&key).ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "no PG sidecar registered for {} v{} {:?}",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        let insert = entry.goal_insert.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not a goal sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        insert(tx, goal_id, payload).await
    }

    /// Copy a typed sidecar row from a superseded Goal to its successor.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG Goal sidecar copy hook is
    /// registered for the schema. Returns storage errors from the concrete
    /// copier.
    pub(crate) async fn copy_goal_sidecar(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        key: PgSidecarKey,
        goal_id: GoalId,
        source_goal_id: GoalId,
    ) -> Result<(), StorageError> {
        let entry = self.entries.get(&key).ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "no PG sidecar registered for {} v{} {:?}",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        let copy = entry.goal_copy.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not a goal sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        copy(tx, goal_id, source_goal_id).await
    }
}

/// The frozen registry bound to one authorized write's lexical language.
///
/// A projection row's `lexical_language` is a property of the WRITE, not of
/// the caller that happens to be holding the registry, so it is read off
/// the authorized draft once — [`PgSidecarRegistryFrozen::writing`] — and
/// the insert takes no language argument at all. What each schema does with
/// it is then a property of its CONTRACT rather than of the call site:
/// a pinned policy's statement inlines its configuration and reads no bind,
/// so the draft's value is never consulted; `PerRow` declares that the row's
/// language IS the writer's, so the draft must carry one.
///
/// Bound rather than borrowed because the sidecar closures the write verbs
/// invoke outlive the call frame; the registry itself is an `Arc` clone.
#[derive(Debug, Clone)]
pub(crate) struct PgSidecarWriter {
    registry: PgSidecarRegistryFrozen,
    draft_language: Option<String>,
}

impl PgSidecarWriter {
    /// Insert a typed sidecar row for an already-created Memory row, and
    /// the projection row that follows it.
    ///
    /// Crate-private: this is the one write that maintains the projection
    /// row alongside the sidecar row, and a public door onto it — even the
    /// CORRECT door — is a second write path. Callers reach it through
    /// `Engine`/`UnitOfWork` and the write ports.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG memory sidecar is
    /// registered for the payload schema, when the erased payload type does
    /// not match the registered Rust type, or when the schema declares
    /// `LanguagePolicy::PerRow` and the write declares no language. Returns
    /// storage errors from the concrete inserter.
    pub(crate) async fn insert_memory_sidecar(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        payload: &SidecarPayload,
    ) -> Result<(), StorageError> {
        let key = PgSidecarKey::new(
            payload.kind,
            payload.schema_id.clone(),
            payload.schema_version,
        );
        let entry = self.registry.entries.get(&key).ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "no PG sidecar registered for {} v{} {:?}",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        let insert = entry.memory_insert.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not a memory sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        insert(tx, memory_id, payload, super::SidecarInsertPermit::new()).await?;
        if let Some(sql) = entry.projection_insert.as_deref() {
            let language = self.language_bind(entry, &key)?;
            // SQL-POLICY: generated
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(memory_id.into_inner())
                .bind(language)
                .bind(key.schema_id.as_str())
                .execute(tx.as_mut())
                .await
                .map_err(crate::error::map_err)?;
        }
        Ok(())
    }

    /// What the generated statement's language bind carries, decided by the
    /// schema's declared policy.
    ///
    /// `None` means "the statement does not read it": a pinned policy's
    /// configuration is a literal in the SQL, and the deployment default is
    /// what an explicit [`LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT`] request
    /// resolves to through the statement's own `COALESCE`.
    fn language_bind(
        &self,
        entry: &PgSidecarEntry,
        key: &PgSidecarKey,
    ) -> Result<Option<&str>, StorageError> {
        match entry.projection_language {
            // Not a projected schema, or a policy that fixes the
            // configuration: the statement has no language bind to read.
            None | Some(LanguagePolicy::Pinned(_) | LanguagePolicy::PinnedUnion(_)) => Ok(None),
            Some(LanguagePolicy::PerRow { .. }) => match self.draft_language.as_deref() {
                Some(LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT) => Ok(None),
                Some(config) => Ok(Some(config)),
                // Explicit over implicit: the schema says the row's
                // language is the writer's, so a writer that named none
                // made no choice, and stamping the deployment default here
                // would make its own row unmatchable by its own words
                // without anyone having decided that.
                None => Err(StorageError::ConstraintViolation(format!(
                    "{} declares LanguagePolicy::PerRow, so its projection row is stamped with \
                     the writing draft's lexical language, and this draft carries none: resolve \
                     the write's language with proxima_core::lexical_language::\
                     resolve_lexical_language (an omitted argument asks for the deployment \
                     configuration), or declare a pinned language policy on the schema",
                    key.schema_id.as_str()
                ))),
            },
        }
    }
}

#[cfg(test)]
mod language_bind_tests {
    use super::{LanguagePolicy, PgSidecarEntry, PgSidecarKey, PgSidecarWriter};
    use proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT;
    use proxima_core::verbs::schema::PayloadKind;
    use proxima_core::{SchemaId, SchemaVersion};

    fn key() -> PgSidecarKey {
        PgSidecarKey::new(
            PayloadKind::Fact,
            SchemaId::new("core/agent-note".to_owned()),
            SchemaVersion::new(1),
        )
    }

    fn entry(language: Option<LanguagePolicy>) -> PgSidecarEntry {
        PgSidecarEntry {
            key: key(),
            sidecar_table: "proxima_core.agent_note_v1".to_owned(),
            owner_pinned: false,
            memory_key_column: Some("t"),
            memory_insert: None,
            memory_load: None,
            memory_load_batch: None,
            cited_object_insert: None,
            citation_mapping_insert: None,
            goal_insert: None,
            goal_copy: None,
            projection_insert: language.map(|_| "INSERT".to_owned()),
            projection_table: language.map(|_| "proxima_core.projection".to_owned()),
            projection_language: language,
        }
    }

    fn writer(draft_language: Option<&str>) -> PgSidecarWriter {
        PgSidecarWriter {
            registry: super::PgSidecarRegistryFrozen::default(),
            draft_language: draft_language.map(str::to_owned),
        }
    }

    /// A pinned policy's configuration is a literal in the generated
    /// statement, so there is no bind to fill and nothing the write could
    /// say that would change the row — including a value that disagrees
    /// with the pin.
    #[test]
    fn a_pinned_policy_reads_nothing_off_the_write() {
        for policy in [
            LanguagePolicy::Pinned("english"),
            LanguagePolicy::PinnedUnion(&["simple", "english"]),
        ] {
            for draft in [
                None,
                Some("german"),
                Some(LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT),
            ] {
                assert_eq!(
                    writer(draft)
                        .language_bind(&entry(Some(policy)), &key())
                        .expect("a pinned policy never refuses"),
                    None,
                    "{policy:?} with draft {draft:?}"
                );
            }
        }
    }

    /// A schema that writes no projection row has no language question.
    #[test]
    fn an_unprojected_schema_reads_nothing_off_the_write() {
        assert_eq!(
            writer(None)
                .language_bind(&entry(None), &key())
                .expect("an unprojected schema never refuses"),
            None
        );
    }

    #[test]
    fn a_per_row_policy_binds_the_drafts_configuration() {
        let per_row = LanguagePolicy::PerRow {
            column: "lexical_language",
        };
        assert_eq!(
            writer(Some("german"))
                .language_bind(&entry(Some(per_row)), &key())
                .expect("a named configuration is bound"),
            Some("german")
        );
        // The deployment-default REQUEST is a choice the write made, and
        // the statement's own COALESCE is where it resolves.
        assert_eq!(
            writer(Some(LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT))
                .language_bind(&entry(Some(per_row)), &key())
                .expect("an explicit deployment-default request is not a refusal"),
            None
        );
    }

    /// Explicit over implicit: no language at all is not a request for the
    /// deployment default, and the error says how to make it one.
    #[test]
    fn a_per_row_policy_refuses_a_write_that_named_no_language() {
        let per_row = LanguagePolicy::PerRow {
            column: "lexical_language",
        };
        let err = writer(None)
            .language_bind(&entry(Some(per_row)), &key())
            .expect_err("a PerRow schema refuses a write that named no language");
        let message = err.to_string();
        assert!(
            message.contains("core/agent-note"),
            "the refusal names the schema: {message}"
        );
        assert!(
            message.contains("declares LanguagePolicy::PerRow"),
            "the refusal names the policy: {message}"
        );
        assert!(
            message.contains("resolve_lexical_language")
                && message.contains("declare a pinned language policy"),
            "the refusal names both fixes: {message}"
        );
    }
}
