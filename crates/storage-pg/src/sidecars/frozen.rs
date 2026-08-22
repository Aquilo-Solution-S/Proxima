use super::{
    Arc, GoalId, HashMap, HashSet, MemoryId, PgConnection, PgSidecarEntry, PgSidecarKey,
    PgSidecarReadCtx, Postgres, SchemaInfo, SidecarPayload, StorageError, Transaction,
};

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
                memory_insert: Some(|_, _, _| Box::pin(async { Ok(()) })),
                memory_load: None,
                memory_load_batch: Some(|_, _, _| Box::pin(async { Ok(Vec::new()) })),
                cited_object_insert: None,
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
                projection_insert: None,
                projection_table: None,
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

    /// Insert a typed sidecar row for an already-created Memory row.
    ///
    /// `lexical_language` is the resolved text-search configuration the
    /// caller asked for, or `None` for the deployment default. It is stamped
    /// on the projection row, which is where a memory's language lives.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG memory sidecar is
    /// registered for the payload schema or when the erased payload type
    /// does not match the registered Rust type. Returns storage errors
    /// from the concrete inserter.
    pub async fn insert_memory_sidecar(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        payload: &SidecarPayload,
        lexical_language: Option<&str>,
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
        let insert = entry.memory_insert.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not a memory sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        insert(tx, memory_id, payload).await?;
        if let Some(sql) = entry.projection_insert.as_deref() {
            // SQL-POLICY: generated
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(memory_id.into_inner())
                .bind(lexical_language)
                .bind(key.schema_id.as_str())
                .execute(tx.as_mut())
                .await
                .map_err(crate::error::map_err)?;
        }
        Ok(())
    }

    /// Rebuild the projection row for one already-restored sidecar row.
    ///
    /// Hydrate restores sidecar rows generically from the cold dump, so it
    /// cannot go through [`Self::insert_memory_sidecar`]. It re-derives the
    /// projection from the restored row instead — the same statement, run
    /// against a row that is already there.
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

    /// Insert a typed sidecar row for an already-created cited-object row.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG cited-object sidecar is
    /// registered for the payload schema or when the erased payload type
    /// does not match the registered Rust type. Returns storage errors from
    /// the concrete inserter.
    pub async fn insert_cited_object_sidecar(
        &self,
        tx: &mut PgConnection,
        cited_object_id: uuid::Uuid,
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
        let insert = entry.cited_object_insert.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not a cited-object sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        insert(tx, cited_object_id, payload).await
    }

    /// Insert a typed sidecar row for an already-created citation-mapping row.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG citation-mapping sidecar is
    /// registered for the payload schema or when the erased payload type
    /// does not match the registered Rust type. Returns storage errors from
    /// the concrete inserter.
    pub async fn insert_citation_mapping_sidecar(
        &self,
        tx: &mut PgConnection,
        citation_mapping_id: uuid::Uuid,
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
        let insert = entry.citation_mapping_insert.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not a citation-mapping sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        insert(tx, citation_mapping_id, payload).await
    }

    /// Insert a typed sidecar row for an already-created Goal row.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG Goal sidecar is registered
    /// for the payload schema or when the erased payload type does not match
    /// the registered Rust type. Returns storage errors from the concrete
    /// inserter.
    pub async fn insert_goal_sidecar(
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
    pub async fn copy_goal_sidecar(
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
