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
        insert(tx, memory_id, payload).await
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
