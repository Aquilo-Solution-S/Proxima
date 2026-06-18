//! Postgres-owned sidecar registration.
//!
//! Core owns schema metadata. This module owns backend-specific sidecar
//! coverage so flavor composition can remain build-time and storage can
//! stay out of `proxima-core`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, EdgeId, EdgePayload,
    FactPayload, GoalId, GoalPayload, MemoryId, PerspectivePayload, SchemaId, SchemaVersion,
    SidecarPayload, StorageError,
};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::verbs::event_ingest::PgFactSidecar;

pub type PgSidecarFuture<'t> = Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 't>>;
pub type PgMemoryPayloadFuture<'t> =
    Pin<Box<dyn Future<Output = Result<Option<SidecarPayload>, StorageError>> + Send + 't>>;
pub type PgMemoryPayloadBatchFuture<'t> = Pin<
    Box<dyn Future<Output = Result<Vec<(MemoryId, SidecarPayload)>, StorageError>> + Send + 't>,
>;

pub trait PgMemorySidecar: Send + Sync + 'static {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t>;
}

pub trait PgMemoryPayload: Send + Sync + 'static {
    #[must_use]
    fn load_batch<'t>(
        pool: &'t PgPool,
        kind: PayloadKind,
        memory_ids: &'t [MemoryId],
    ) -> PgMemoryPayloadBatchFuture<'t> {
        Box::pin(async move {
            let _ = kind;
            let mut payloads = Vec::new();
            for memory_id in memory_ids {
                if let Some(payload) = Self::load_memory_payload(pool, *memory_id).await? {
                    payloads.push((*memory_id, payload));
                }
            }
            Ok(payloads)
        })
    }

    #[must_use]
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let memory_ids = [memory_id];
            let mut rows = Self::load_batch(pool, PayloadKind::Fact, &memory_ids).await?;
            Ok(rows.pop().map(|(_memory_id, payload)| payload))
        })
    }
}

pub trait PgEdgeSidecar: Send + Sync + 'static {
    fn insert_edge_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        edge_id: EdgeId,
    ) -> PgSidecarFuture<'t>;
}

pub trait PgCitedObjectSidecar: Send + Sync + 'static {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        cited_object_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t>;
}

pub trait PgCitationMappingSidecar: Send + Sync + 'static {
    fn insert_citation_mapping_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        citation_mapping_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t>;
}

pub trait PgGoalSidecar: Send + Sync + 'static {
    fn insert_goal_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
    ) -> PgSidecarFuture<'t>;

    fn copy_goal_sidecar<'t>(
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
        source_goal_id: GoalId,
    ) -> PgSidecarFuture<'t>;
}

impl<T> PgMemorySidecar for T
where
    T: PgFactSidecar + Clone + Send + Sync,
{
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        self.clone().insert_sidecar(tx, memory_id)
    }
}

type PgMemorySidecarInserter = for<'t> fn(
    &'t mut Transaction<'_, Postgres>,
    MemoryId,
    &'t SidecarPayload,
) -> PgSidecarFuture<'t>;
type PgMemoryPayloadLoader = for<'t> fn(&'t PgPool, MemoryId) -> PgMemoryPayloadFuture<'t>;
type PgMemoryPayloadBatchLoader =
    for<'t> fn(&'t PgPool, PayloadKind, &'t [MemoryId]) -> PgMemoryPayloadBatchFuture<'t>;

type PgEdgeSidecarInserter =
    for<'t> fn(&'t mut PgConnection, EdgeId, &'t SidecarPayload) -> PgSidecarFuture<'t>;
type PgCitedObjectSidecarInserter =
    for<'t> fn(&'t mut PgConnection, uuid::Uuid, &'t SidecarPayload) -> PgSidecarFuture<'t>;
type PgCitationMappingSidecarInserter =
    for<'t> fn(&'t mut PgConnection, uuid::Uuid, &'t SidecarPayload) -> PgSidecarFuture<'t>;
type PgGoalSidecarInserter = for<'t> fn(
    &'t mut Transaction<'_, Postgres>,
    GoalId,
    &'t SidecarPayload,
) -> PgSidecarFuture<'t>;
type PgGoalSidecarCopier =
    for<'t> fn(&'t mut Transaction<'_, Postgres>, GoalId, GoalId) -> PgSidecarFuture<'t>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PgSidecarKey {
    pub kind: PayloadKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
}

impl PgSidecarKey {
    #[must_use]
    pub fn new(kind: PayloadKind, schema_id: SchemaId, schema_version: SchemaVersion) -> Self {
        Self {
            kind,
            schema_id,
            schema_version,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PgSidecarEntry {
    pub key: PgSidecarKey,
    pub sidecar_table: String,
    memory_insert: Option<PgMemorySidecarInserter>,
    memory_load: Option<PgMemoryPayloadLoader>,
    memory_load_batch: Option<PgMemoryPayloadBatchLoader>,
    edge_insert: Option<PgEdgeSidecarInserter>,
    cited_object_insert: Option<PgCitedObjectSidecarInserter>,
    citation_mapping_insert: Option<PgCitationMappingSidecarInserter>,
    goal_insert: Option<PgGoalSidecarInserter>,
    goal_copy: Option<PgGoalSidecarCopier>,
}

#[derive(Debug, Clone, Default)]
pub struct PgSidecarRegistry {
    entries: HashMap<PgSidecarKey, PgSidecarEntry>,
}

impl PgSidecarRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one Fact sidecar with a typed memory inserter.
    ///
    /// # Panics
    ///
    /// Panics if the same Fact schema/version is registered twice.
    pub fn add_fact<P: FactPayload + PgMemorySidecar + PgMemoryPayload>(&mut self) {
        if let Some(table) = P::sidecar_table() {
            let key = PgSidecarKey::new(
                PayloadKind::Fact,
                P::schema_id(),
                SchemaVersion::new(P::SCHEMA_VERSION),
            );
            let prior = self.entries.insert(
                key.clone(),
                PgSidecarEntry {
                    key,
                    sidecar_table: table.to_string(),
                    memory_insert: Some(insert_memory_sidecar::<P>),
                    memory_load: Some(load_memory_payload::<P>),
                    memory_load_batch: Some(load_memory_payload_batch::<P>),
                    edge_insert: None,
                    cited_object_insert: None,
                    citation_mapping_insert: None,
                    goal_insert: None,
                    goal_copy: None,
                },
            );
            assert!(
                prior.is_none(),
                "duplicate PG sidecar registration for {:?}",
                prior.map(|entry| entry.key),
            );
        }
    }

    /// Register one Abstraction sidecar with a typed memory inserter.
    ///
    /// # Panics
    ///
    /// Panics if the same Abstraction schema/version is registered twice.
    pub fn add_abstraction<P: AbstractionPayload + PgMemorySidecar + PgMemoryPayload>(&mut self) {
        self.add_memory_schema::<P>(
            PayloadKind::Abstraction,
            P::schema_id(),
            SchemaVersion::new(P::SCHEMA_VERSION),
            P::sidecar_table(),
        );
    }

    /// Register one Perspective sidecar with a typed memory inserter.
    ///
    /// # Panics
    ///
    /// Panics if the same Perspective schema/version is registered twice.
    pub fn add_perspective<P: PerspectivePayload + PgMemorySidecar + PgMemoryPayload>(&mut self) {
        self.add_memory_schema::<P>(
            PayloadKind::Perspective,
            P::schema_id(),
            SchemaVersion::new(P::SCHEMA_VERSION),
            P::sidecar_table(),
        );
    }

    /// Register one Goal sidecar with a typed inserter and copy hook.
    ///
    /// # Panics
    ///
    /// Panics if the same Goal schema/version is registered twice.
    pub fn add_goal<P: GoalPayload + PgGoalSidecar>(&mut self) {
        if let Some(table) = P::sidecar_table() {
            let key = PgSidecarKey::new(
                PayloadKind::Goal,
                P::schema_id(),
                SchemaVersion::new(P::SCHEMA_VERSION),
            );
            let prior = self.entries.insert(
                key.clone(),
                PgSidecarEntry {
                    key,
                    sidecar_table: table.to_string(),
                    memory_insert: None,
                    memory_load: None,
                    memory_load_batch: None,
                    edge_insert: None,
                    cited_object_insert: None,
                    citation_mapping_insert: None,
                    goal_insert: Some(insert_goal_sidecar::<P>),
                    goal_copy: Some(copy_goal_sidecar::<P>),
                },
            );
            assert!(
                prior.is_none(),
                "duplicate PG sidecar registration for {:?}",
                prior.map(|entry| entry.key),
            );
        }
    }

    /// Register one Edge sidecar with a typed inserter.
    ///
    /// # Panics
    ///
    /// Panics if the same Edge schema/version is registered twice.
    pub fn add_edge<P: EdgePayload + PgEdgeSidecar>(&mut self) {
        let key = PgSidecarKey::new(
            PayloadKind::Edge,
            P::schema_id(),
            SchemaVersion::new(P::SCHEMA_VERSION),
        );
        let prior = self.entries.insert(
            key.clone(),
            PgSidecarEntry {
                key,
                sidecar_table: P::sidecar_table().to_string(),
                memory_insert: None,
                memory_load: None,
                memory_load_batch: None,
                edge_insert: Some(insert_edge_sidecar::<P>),
                cited_object_insert: None,
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
            },
        );
        assert!(
            prior.is_none(),
            "duplicate PG sidecar registration for {:?}",
            prior.map(|entry| entry.key),
        );
    }

    /// Register one `CitedObject` sidecar with a typed inserter.
    ///
    /// # Panics
    ///
    /// Panics if the same `CitedObject` schema/version is registered twice.
    pub fn add_cited_object<P: CitedObjectPayload + PgCitedObjectSidecar>(&mut self) {
        let key = PgSidecarKey::new(
            PayloadKind::CitedObject,
            P::schema_id(),
            SchemaVersion::new(P::SCHEMA_VERSION),
        );
        let prior = self.entries.insert(
            key.clone(),
            PgSidecarEntry {
                key,
                sidecar_table: P::sidecar_table().to_string(),
                memory_insert: None,
                memory_load: None,
                memory_load_batch: None,
                edge_insert: None,
                cited_object_insert: Some(insert_cited_object_sidecar::<P>),
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
            },
        );
        assert!(
            prior.is_none(),
            "duplicate PG sidecar registration for {:?}",
            prior.map(|entry| entry.key),
        );
    }

    /// Register one `CitationMapping` sidecar with a typed inserter.
    /// Pure-link mapping payloads with no sidecar table are intentionally
    /// skipped.
    ///
    /// # Panics
    ///
    /// Panics if the same sidecar-bearing `CitationMapping` schema/version is
    /// registered twice.
    pub fn add_citation_mapping<P: CitationMappingPayload + PgCitationMappingSidecar>(&mut self) {
        if let Some(table) = P::sidecar_table() {
            let key = PgSidecarKey::new(
                PayloadKind::CitationMapping,
                P::schema_id(),
                SchemaVersion::new(P::SCHEMA_VERSION),
            );
            let prior = self.entries.insert(
                key.clone(),
                PgSidecarEntry {
                    key,
                    sidecar_table: table.to_string(),
                    memory_insert: None,
                    memory_load: None,
                    memory_load_batch: None,
                    edge_insert: None,
                    cited_object_insert: None,
                    citation_mapping_insert: Some(insert_citation_mapping_sidecar::<P>),
                    goal_insert: None,
                    goal_copy: None,
                },
            );
            assert!(
                prior.is_none(),
                "duplicate PG sidecar registration for {:?}",
                prior.map(|entry| entry.key),
            );
        }
    }

    fn add_memory_schema<P>(
        &mut self,
        kind: PayloadKind,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        sidecar_table: impl Into<String>,
    ) where
        P: PgMemorySidecar + PgMemoryPayload,
    {
        let key = PgSidecarKey::new(kind, schema_id, schema_version);
        let prior = self.entries.insert(
            key.clone(),
            PgSidecarEntry {
                key,
                sidecar_table: sidecar_table.into(),
                memory_insert: Some(insert_memory_sidecar::<P>),
                memory_load: Some(load_memory_payload::<P>),
                memory_load_batch: Some(load_memory_payload_batch::<P>),
                edge_insert: None,
                cited_object_insert: None,
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
            },
        );
        assert!(
            prior.is_none(),
            "duplicate PG sidecar registration for {:?}",
            prior.map(|entry| entry.key),
        );
    }

    /// Seal the PG registry against the already-frozen core registry.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` if a sidecar-bearing schema has no
    /// PG registration, a registration is orphaned, a table name drifts,
    /// or an insert-capable kind has no typed inserter.
    pub fn freeze_against(
        self,
        schemas: &[SchemaInfo],
    ) -> Result<PgSidecarRegistryFrozen, StorageError> {
        let schema_keys = schemas
            .iter()
            .filter_map(|schema| {
                schema.sidecar_table.as_ref().map(|table| {
                    (
                        PgSidecarKey::new(
                            schema.kind,
                            schema.schema_id.clone(),
                            schema.schema_version,
                        ),
                        table.as_str(),
                    )
                })
            })
            .collect::<HashMap<_, _>>();

        for (key, schema_table) in &schema_keys {
            let Some(entry) = self.entries.get(key) else {
                return Err(StorageError::ConstraintViolation(format!(
                    "schema {} v{} {:?} declares sidecar table {schema_table} but no PG sidecar is registered",
                    key.schema_id.as_str(),
                    key.schema_version.into_inner(),
                    key.kind,
                )));
            };
            if *schema_table != entry.sidecar_table {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar registration for {} v{} {:?} uses table {}, registry declares {}",
                    key.schema_id.as_str(),
                    key.schema_version.into_inner(),
                    key.kind,
                    entry.sidecar_table,
                    schema_table,
                )));
            }
            let has_inserter = match key.kind {
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective => {
                    entry.memory_insert.is_some()
                        && entry.memory_load.is_some()
                        && entry.memory_load_batch.is_some()
                }
                PayloadKind::Edge => entry.edge_insert.is_some(),
                PayloadKind::CitedObject => entry.cited_object_insert.is_some(),
                PayloadKind::CitationMapping => entry.citation_mapping_insert.is_some(),
                PayloadKind::Goal => entry.goal_insert.is_some() && entry.goal_copy.is_some(),
            };
            if !has_inserter {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar registration for {} v{} {:?} has no typed inserter",
                    key.schema_id.as_str(),
                    key.schema_version.into_inner(),
                    key.kind,
                )));
            }
        }

        for entry in self.entries.values() {
            let Some(schema_table) = schema_keys.get(&entry.key) else {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar registration references unregistered schema {} v{} {:?}",
                    entry.key.schema_id.as_str(),
                    entry.key.schema_version.into_inner(),
                    entry.key.kind,
                )));
            };
            if *schema_table != entry.sidecar_table {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar registration for {} v{} {:?} uses table {}, registry declares {}",
                    entry.key.schema_id.as_str(),
                    entry.key.schema_version.into_inner(),
                    entry.key.kind,
                    entry.sidecar_table,
                    schema_table,
                )));
            }
        }

        Ok(PgSidecarRegistryFrozen {
            entries: Arc::new(self.entries),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct PgSidecarRegistryFrozen {
    entries: Arc<HashMap<PgSidecarKey, PgSidecarEntry>>,
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
        pool: &PgPool,
        key: PgSidecarKey,
        memory_id: MemoryId,
    ) -> Result<Option<SidecarPayload>, StorageError> {
        let mut rows = self
            .load_memory_payloads_batch(pool, &key, &[memory_id])
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
        pool: &PgPool,
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
        load(pool, key.kind, memory_ids).await
    }

    /// Insert a typed sidecar row for an already-created Edge row.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` when no PG edge sidecar is registered
    /// for the payload schema or when the erased payload type does not match
    /// the registered Rust type. Returns storage errors from the concrete
    /// inserter.
    pub async fn insert_edge_sidecar(
        &self,
        tx: &mut PgConnection,
        edge_id: EdgeId,
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
        let insert = entry.edge_insert.ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "PG sidecar for {} v{} {:?} is not an edge sidecar",
                key.schema_id.as_str(),
                key.schema_version.into_inner(),
                key.kind,
            ))
        })?;
        insert(tx, edge_id, payload).await
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

fn insert_memory_sidecar<'t, P>(
    tx: &'t mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgMemorySidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed.insert_memory_sidecar(tx, memory_id).await
    })
}

fn load_memory_payload<P>(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_>
where
    P: PgMemoryPayload,
{
    P::load_memory_payload(pool, memory_id)
}

fn load_memory_payload_batch<'t, P>(
    pool: &'t PgPool,
    kind: PayloadKind,
    memory_ids: &'t [MemoryId],
) -> PgMemoryPayloadBatchFuture<'t>
where
    P: PgMemoryPayload,
{
    P::load_batch(pool, kind, memory_ids)
}

/// Convert a byte slice into a fixed 32-byte array.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the input is not exactly 32 bytes.
pub fn bytes32(bytes: &[u8], column: &str) -> Result<[u8; 32], StorageError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        StorageError::Internal(format!(
            "{column} must be exactly 32 bytes, got {}",
            bytes.len()
        ))
    })
}

/// Convert a signed SQL `integer` value into `u32`.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the database value is negative.
pub fn int_to_u32(value: i32, column: &str) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|err| StorageError::Internal(format!("invalid {column}: {err}")))
}

/// Convert a signed SQL `bigint` value into `u64`.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the database value is negative.
pub fn int_to_u64(value: i64, column: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|err| StorageError::Internal(format!("invalid {column}: {err}")))
}

#[must_use]
pub fn memory_insert_sql(
    table: &str,
    key_column: &str,
    columns: &[(&str, Option<&str>)],
) -> String {
    let mut sql = String::new();
    write!(&mut sql, "INSERT INTO {table} ({key_column}")
        .expect("writing SQL into String cannot fail");
    for (column, _) in columns {
        write!(&mut sql, ", {column}").expect("writing SQL into String cannot fail");
    }
    sql.push_str(") VALUES ($1");
    for (index, (_, cast)) in columns.iter().enumerate() {
        write!(&mut sql, ", ${}", index + 2).expect("writing SQL into String cannot fail");
        if let Some(pg_type) = cast {
            write!(&mut sql, "::{pg_type}").expect("writing SQL into String cannot fail");
        }
    }
    sql.push(')');
    sql
}

#[must_use]
pub fn memory_select_batch_sql(table: &str, key_column: &str, columns: &[&str]) -> String {
    let mut sql = String::new();
    write!(&mut sql, "SELECT {key_column}").expect("writing SQL into String cannot fail");
    for column in columns {
        write!(&mut sql, ", {column}").expect("writing SQL into String cannot fail");
    }
    write!(&mut sql, " FROM {table} WHERE {key_column} = ANY($1)")
        .expect("writing SQL into String cannot fail");
    sql
}

fn parse_utterance_speaker(value: &str) -> Result<proxima_core::Speaker, StorageError> {
    match value {
        "user" => Ok(proxima_core::Speaker::User),
        "agent" => Ok(proxima_core::Speaker::Agent),
        other => Err(StorageError::Internal(format!(
            "invalid utterance speaker {other}"
        ))),
    }
}

#[macro_export]
macro_rules! pg_sidecar_row_ty {
    (uuid) => {
        ::uuid::Uuid
    };
    (uuid_array) => {
        ::std::vec::Vec<::uuid::Uuid>
    };
    (text) => {
        ::std::string::String
    };
    (opt_text) => {
        ::std::option::Option<::std::string::String>
    };
    (text_array) => {
        ::std::vec::Vec<::std::string::String>
    };
    (bool) => {
        bool
    };
    (timestamptz) => {
        ::time::OffsetDateTime
    };
    (bytea32) => {
        ::std::vec::Vec<u8>
    };
    (u32_as_i32) => {
        i32
    };
    (u32_as_i64) => {
        i64
    };
    (u64_as_i64) => {
        i64
    };
    (enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }) => {
        ::std::string::String
    };
}

#[macro_export]
macro_rules! pg_sidecar_cast {
    (enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }) => {
        ::std::option::Option::Some($pg_type)
    };
    ($kind:ident) => {
        ::std::option::Option::None
    };
}

#[macro_export]
macro_rules! pg_sidecar_bind {
    ((uuid), $self:ident, $field:ident) => {
        $self.$field
    };
    ((uuid_array), $self:ident, $field:ident) => {
        &$self.$field
    };
    ((text), $self:ident, $field:ident) => {
        &$self.$field
    };
    ((opt_text), $self:ident, $field:ident) => {
        $self.$field.as_deref()
    };
    ((text_array), $self:ident, $field:ident) => {
        &$self.$field
    };
    ((bool), $self:ident, $field:ident) => {
        $self.$field
    };
    ((timestamptz), $self:ident, $field:ident) => {
        $self.$field
    };
    ((bytea32), $self:ident, $field:ident) => {
        $self.$field.to_vec()
    };
    ((u32_as_i32), $self:ident, $field:ident) => {
        <::std::primitive::i32 as ::std::convert::TryFrom<_>>::try_from($self.$field).map_err(
            |err| {
                ::proxima_core::StorageError::ConstraintViolation(::std::format!(
                    "{} out of range: {err}",
                    ::std::stringify!($field)
                ))
            },
        )?
    };
    ((u32_as_i64), $self:ident, $field:ident) => {
        ::std::primitive::i64::from($self.$field)
    };
    ((u64_as_i64), $self:ident, $field:ident) => {
        <::std::primitive::i64 as ::std::convert::TryFrom<_>>::try_from($self.$field).map_err(
            |err| {
                ::proxima_core::StorageError::ConstraintViolation(::std::format!(
                    "{} out of range: {err}",
                    ::std::stringify!($field)
                ))
            },
        )?
    };
    (
        (
            enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }
        ),
        $self:ident,
        $field:ident
    ) => {
        $to_str(&$self.$field)
    };
}

#[macro_export]
macro_rules! pg_sidecar_decode {
    ((uuid), $row:ident, $field:ident) => {
        $row.$field
    };
    ((uuid_array), $row:ident, $field:ident) => {
        $row.$field
    };
    ((text), $row:ident, $field:ident) => {
        $row.$field
    };
    ((opt_text), $row:ident, $field:ident) => {
        $row.$field
    };
    ((text_array), $row:ident, $field:ident) => {
        $row.$field
    };
    ((bool), $row:ident, $field:ident) => {
        $row.$field
    };
    ((timestamptz), $row:ident, $field:ident) => {
        $row.$field
    };
    ((bytea32), $row:ident, $field:ident) => {
        $crate::sidecars::bytes32(&$row.$field, ::std::stringify!($field))?
    };
    ((u32_as_i32), $row:ident, $field:ident) => {
        $crate::sidecars::int_to_u32($row.$field, ::std::stringify!($field))?
    };
    ((u32_as_i64), $row:ident, $field:ident) => {
        <::std::primitive::u32 as ::std::convert::TryFrom<_>>::try_from($row.$field).map_err(
            |err| {
                ::proxima_core::StorageError::Internal(::std::format!(
                    "invalid {}: {err}",
                    ::std::stringify!($field)
                ))
            },
        )?
    };
    ((u64_as_i64), $row:ident, $field:ident) => {
        $crate::sidecars::int_to_u64($row.$field, ::std::stringify!($field))?
    };
    (
        (
            enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }
        ),
        $row:ident,
        $field:ident
    ) => {
        ($from_str)($row.$field.as_str())?
    };
}

#[macro_export]
macro_rules! pg_sidecar_payload {
    (@wrap Fact, $payload:expr) => {
        ::proxima_core::SidecarPayload::fact($payload)
    };
    (@wrap Abstraction, $payload:expr) => {
        ::proxima_core::SidecarPayload::abstraction($payload)
    };
    (@wrap Perspective, $payload:expr) => {
        ::proxima_core::SidecarPayload::perspective($payload)
    };
    ($payload_ty:path, $kind:expr, $payload:expr, [$($payload_kind:ident),+ $(,)?]) => {{
        match $kind {
            $(
                ::proxima_core::verbs::schema::PayloadKind::$payload_kind => {
                    ::std::result::Result::Ok($crate::pg_sidecar_payload!(@wrap $payload_kind, $payload))
                }
            )+
            other => ::std::result::Result::Err(::proxima_core::StorageError::ConstraintViolation(::std::format!(
                "payload kind {other:?} is not valid for {}",
                ::std::any::type_name::<$payload_ty>(),
            ))),
        }
    }};
}

#[macro_export]
macro_rules! pg_sidecar {
    (
        payload: $($payload_ty:ident)::+,
        row: $row_ty:ident,
        kinds: [$($payload_kind:ident),+ $(,)?],
        table: $table:literal,
        key: $key_column:ident,
        fields: {
            $(
                $field:ident => $column:ident : $column_kind:tt
            ),+ $(,)?
        } $(,)?
    ) => {
        impl $crate::sidecars::PgMemorySidecar for $($payload_ty)::+ {
            fn insert_memory_sidecar<'t>(
                &'t self,
                tx: &'t mut ::sqlx::Transaction<'_, ::sqlx::Postgres>,
                memory_id: ::proxima_core::MemoryId,
            ) -> $crate::sidecars::PgSidecarFuture<'t> {
                ::std::boxed::Box::pin(async move {
                    let sql = $crate::sidecars::memory_insert_sql(
                        $table,
                        ::std::stringify!($key_column),
                        &[$(
                            (::std::stringify!($column), $crate::pg_sidecar_cast! $column_kind),
                        )+],
                    );
                    ::sqlx::query(&sql)
                        .bind(memory_id.into_inner())
                        $(
                            .bind($crate::pg_sidecar_bind!($column_kind, self, $field))
                        )+
                        .execute(tx.as_mut())
                        .await
                        .map_err(|err| ::proxima_core::StorageError::Internal(err.to_string()))?;
                    ::std::result::Result::Ok(())
                })
            }
        }

        #[derive(Debug, ::sqlx::FromRow)]
        struct $row_ty {
            $key_column: ::uuid::Uuid,
            $(
                $field: $crate::pg_sidecar_row_ty! $column_kind,
            )+
        }

        impl $crate::sidecars::PgMemoryPayload for $($payload_ty)::+ {
            fn load_batch<'t>(
                pool: &'t ::sqlx::PgPool,
                kind: ::proxima_core::verbs::schema::PayloadKind,
                memory_ids: &'t [::proxima_core::MemoryId],
            ) -> $crate::sidecars::PgMemoryPayloadBatchFuture<'t> {
                ::std::boxed::Box::pin(async move {
                    if memory_ids.is_empty() {
                        return ::std::result::Result::Ok(::std::vec::Vec::new());
                    }
                    let raw_memory_ids = memory_ids
                        .iter()
                        .map(|memory_id| (*memory_id).into_inner())
                        .collect::<::std::vec::Vec<_>>();
                    let sql = $crate::sidecars::memory_select_batch_sql(
                        $table,
                        ::std::stringify!($key_column),
                        &[$(::std::stringify!($column)),+],
                    );
                    let rows = ::sqlx::query_as::<_, $row_ty>(&sql)
                        .bind(&raw_memory_ids)
                        .fetch_all(pool)
                        .await
                        .map_err(|err| ::proxima_core::StorageError::Internal(err.to_string()))?;
                    rows.into_iter()
                        .map(|row| {
                            let memory_id = ::proxima_core::MemoryId::new(row.$key_column);
                            let payload = $($payload_ty)::+ {
                                $(
                                    $field: $crate::pg_sidecar_decode!($column_kind, row, $field),
                                )+
                            };
                            ::std::result::Result::Ok((
                                memory_id,
                                $crate::pg_sidecar_payload!(
                                    $($payload_ty)::+,
                                    kind,
                                    payload,
                                    [$($payload_kind),+]
                                )?,
                            ))
                        })
                        .collect::<::std::result::Result<::std::vec::Vec<_>, ::proxima_core::StorageError>>()
                })
            }
        }
    };
}

#[macro_export]
macro_rules! goal_lifecycle_fact {
    ($($payload_ty:ident)::+, $row_ty:ident, $table:literal) => {
        $crate::pg_sidecar! {
            payload: $($payload_ty)::+,
            row: $row_ty,
            kinds: [Fact],
            table: $table,
            key: memory_id,
            fields: {
                goal_id => goal_id: (uuid),
                transitioned_at => transitioned_at: (timestamptz),
            },
        }
    };
}

fn insert_edge_sidecar<'t, P>(
    tx: &'t mut PgConnection,
    edge_id: EdgeId,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgEdgeSidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed.insert_edge_sidecar(tx, edge_id).await
    })
}

fn insert_cited_object_sidecar<'t, P>(
    tx: &'t mut PgConnection,
    cited_object_id: uuid::Uuid,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgCitedObjectSidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed.insert_cited_object_sidecar(tx, cited_object_id).await
    })
}

fn insert_citation_mapping_sidecar<'t, P>(
    tx: &'t mut PgConnection,
    citation_mapping_id: uuid::Uuid,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgCitationMappingSidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed
            .insert_citation_mapping_sidecar(tx, citation_mapping_id)
            .await
    })
}

fn insert_goal_sidecar<'t, P>(
    tx: &'t mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgGoalSidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed.insert_goal_sidecar(tx, goal_id).await
    })
}

fn copy_goal_sidecar<'t, P>(
    tx: &'t mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    source_goal_id: GoalId,
) -> PgSidecarFuture<'t>
where
    P: PgGoalSidecar,
{
    P::copy_goal_sidecar(tx, goal_id, source_goal_id)
}

pg_sidecar! {
    payload: proxima_core::AgentNoteV1,
    row: AgentNotePayloadRow,
    kinds: [Fact],
    table: "proxima_core.agent_note_v1",
    key: memory_id,
    fields: {
        note_id => note_id: (uuid),
        title => title: (text),
        body => body: (text),
        tags => tags: (text_array),
        idempotency_key => idempotency_key: (opt_text),
    },
}

pg_sidecar! {
    payload: proxima_core::UtteranceV1,
    row: UtterancePayloadRow,
    kinds: [Fact],
    table: "proxima_core.utterance_v1",
    key: memory_id,
    fields: {
        speaker => speaker: (enum {
            to_str: proxima_core::Speaker::as_str,
            pg_type: "text",
            from_str: parse_utterance_speaker
        }),
        conversation_id => conversation_id: (text),
        text => text: (text),
    },
}

pg_sidecar! {
    payload: proxima_core::AgentDerivationV1,
    row: AgentDerivationPayloadRow,
    kinds: [Abstraction, Perspective],
    table: "proxima_core.agent_derivation_v1",
    key: memory_id,
    fields: {
        title => title: (text),
        body => body: (text),
        tags => tags: (text_array),
        idempotency_key => idempotency_key: (opt_text),
        source_memory_ids => source_memory_ids: (uuid_array),
        model_id => model_id: (text),
        client_name => client_name: (text),
        client_version => client_version: (text),
    },
}

pg_sidecar! {
    payload: proxima_core::verbs::persist_mcp_call::McpCallLoggedV1,
    row: McpCallLoggedPayloadRow,
    kinds: [Fact],
    table: "proxima_core.mcp_call_logged_v1",
    key: memory_id,
    fields: {
        tool_name => tool_name: (text),
        actor_oid => actor_oid: (text),
        actor_upn => actor_upn: (text),
        ok => ok: (bool),
        error => error: (opt_text),
        latency_ms => latency_ms: (u32_as_i64),
        io_byte_len => io_byte_len: (u64_as_i64),
        io_truncated => io_truncated: (bool),
        io_content_hash => io_content_hash: (bytea32),
    },
}

goal_lifecycle_fact!(
    proxima_core::GoalActivatedV1,
    GoalActivatedPayloadRow,
    "proxima_core.goal_activated_v1"
);
goal_lifecycle_fact!(
    proxima_core::GoalPausedV1,
    GoalPausedPayloadRow,
    "proxima_core.goal_paused_v1"
);
goal_lifecycle_fact!(
    proxima_core::GoalAchievedV1,
    GoalAchievedPayloadRow,
    "proxima_core.goal_achieved_v1"
);
goal_lifecycle_fact!(
    proxima_core::GoalAbandonedV1,
    GoalAbandonedPayloadRow,
    "proxima_core.goal_abandoned_v1"
);

impl PgGoalSidecar for proxima_core::TaskGoalV1 {
    fn insert_goal_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.task_goal_v1 (goal_id, due_at, priority)
                 VALUES ($1, $2, $3::proxima_core.task_priority)",
            )
            .bind(goal_id.into_inner())
            .bind(self.due_at)
            .bind(self.priority.map(proxima_core::TaskPriority::as_str))
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }

    fn copy_goal_sidecar<'t>(
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
        source_goal_id: GoalId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            let result = sqlx::query(
                "INSERT INTO proxima_core.task_goal_v1 (goal_id, due_at, priority)
                 SELECT $1, due_at, priority
                   FROM proxima_core.task_goal_v1
                  WHERE goal_id = $2",
            )
            .bind(goal_id.into_inner())
            .bind(source_goal_id.into_inner())
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            if result.rows_affected() == 0 {
                return Err(StorageError::ConstraintViolation(format!(
                    "missing source Goal sidecar for {}",
                    source_goal_id.into_inner(),
                )));
            }
            Ok(())
        })
    }
}

impl PgEdgeSidecar for proxima_core::AgentLinkV1 {
    fn insert_edge_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        edge_id: EdgeId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.agent_link_v1
                    (edge_id, reason, confidence)
                 VALUES ($1, $2, $3)",
            )
            .bind(edge_id.into_inner())
            .bind(&self.reason)
            .bind(i16::from(self.confidence))
            .execute(tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgCitedObjectSidecar for proxima_core::UploadedBlobPayload {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        cited_object_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            let byte_len = i64::try_from(self.byte_len).map_err(|err| {
                StorageError::ConstraintViolation(format!("byte_len out of range: {err}"))
            })?;
            sqlx::query(
                "INSERT INTO proxima_core.cited_uploaded_blob_v1
                    (cited_object_id, bucket, object_key, sha256, byte_len,
                     mime, filename, etag, uploaded_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(cited_object_id)
            .bind(&self.bucket)
            .bind(&self.object_key)
            .bind(&self.sha256[..])
            .bind(byte_len)
            .bind(&self.mime)
            .bind(&self.filename)
            .bind(self.etag.as_deref())
            .bind(self.uploaded_at)
            .execute(tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgCitedObjectSidecar for proxima_core::verbs::persist_mcp_call::McpCallIoV1 {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        cited_object_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            let byte_len = i64::try_from(self.byte_len).map_err(|err| {
                StorageError::ConstraintViolation(format!("byte_len out of range: {err}"))
            })?;
            sqlx::query(
                "INSERT INTO proxima_core.cited_mcp_call_io_v1
                    (cited_object_id, byte_len, truncated, body)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(cited_object_id)
            .bind(byte_len)
            .bind(self.truncated)
            .bind(&self.body)
            .execute(tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

/// Frozen core sidecar registry used by plain substrate `PgStorage`.
///
/// # Panics
///
/// Panics only if the core hardcoded sidecar registrations drift from
/// the core schema registry.
#[must_use]
pub fn core_pg_sidecars() -> PgSidecarRegistryFrozen {
    let mut registry = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut registry);
    registry
        .freeze_against(proxima_core::FlavorRegistry::new().freeze().schemas())
        .expect("core PG sidecars match core schema registry")
}

pub fn register_core_pg_sidecars(registry: &mut PgSidecarRegistry) {
    registry.add_fact::<proxima_core::AgentNoteV1>();
    registry.add_fact::<proxima_core::UtteranceV1>();
    registry.add_fact::<proxima_core::verbs::persist_mcp_call::McpCallLoggedV1>();
    registry.add_fact::<proxima_core::GoalActivatedV1>();
    registry.add_fact::<proxima_core::GoalPausedV1>();
    registry.add_fact::<proxima_core::GoalAchievedV1>();
    registry.add_fact::<proxima_core::GoalAbandonedV1>();
    registry.add_abstraction::<proxima_core::AgentDerivationV1>();
    registry.add_perspective::<proxima_core::AgentDerivationV1>();
    registry.add_goal::<proxima_core::TaskGoalV1>();
    registry.add_edge::<proxima_core::AgentLinkV1>();
    registry.add_cited_object::<proxima_core::UploadedBlobPayload>();
    registry.add_cited_object::<proxima_core::verbs::persist_mcp_call::McpCallIoV1>();
}
