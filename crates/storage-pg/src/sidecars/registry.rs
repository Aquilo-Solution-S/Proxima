use super::dispatch::{
    copy_goal_sidecar, insert_citation_mapping_sidecar, insert_cited_object_sidecar,
    insert_goal_sidecar, insert_memory_sidecar, load_memory_payload, load_memory_payload_batch,
};
use super::{
    AbstractionPayload, Arc, CitationMappingPayload, CitedObjectPayload, FactPayload, GoalPayload,
    HashMap, PayloadKind, PerspectivePayload, PgCitationMappingSidecar, PgCitedObjectSidecar,
    PgGoalSidecar, PgMemoryPayload, PgMemorySidecar, PgSidecarEntry, PgSidecarKey,
    PgSidecarRegistryFrozen, SchemaId, SchemaVersion, StorageError,
};

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
                    owner_pinned: <P as PgMemoryPayload>::OWNER_PINNED,
                    memory_insert: Some(insert_memory_sidecar::<P>),
                    memory_load: Some(load_memory_payload::<P>),
                    memory_load_batch: Some(load_memory_payload_batch::<P>),
                    cited_object_insert: None,
                    citation_mapping_insert: None,
                    goal_insert: None,
                    goal_copy: None,
                    projection_insert: None,
                    projection_table: None,
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
                    owner_pinned: false,
                    memory_insert: None,
                    memory_load: None,
                    memory_load_batch: None,
                    cited_object_insert: None,
                    citation_mapping_insert: None,
                    goal_insert: Some(insert_goal_sidecar::<P>),
                    goal_copy: Some(copy_goal_sidecar::<P>),
                    projection_insert: None,
                    projection_table: None,
                },
            );
            assert!(
                prior.is_none(),
                "duplicate PG sidecar registration for {:?}",
                prior.map(|entry| entry.key),
            );
        }
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
                owner_pinned: false,
                memory_insert: None,
                memory_load: None,
                memory_load_batch: None,
                cited_object_insert: Some(insert_cited_object_sidecar::<P>),
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
                projection_insert: None,
                projection_table: None,
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
                    owner_pinned: false,
                    memory_insert: None,
                    memory_load: None,
                    memory_load_batch: None,
                    cited_object_insert: None,
                    citation_mapping_insert: Some(insert_citation_mapping_sidecar::<P>),
                    goal_insert: None,
                    goal_copy: None,
                    projection_insert: None,
                    projection_table: None,
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
                owner_pinned: <P as PgMemoryPayload>::OWNER_PINNED,
                memory_insert: Some(insert_memory_sidecar::<P>),
                memory_load: Some(load_memory_payload::<P>),
                memory_load_batch: Some(load_memory_payload_batch::<P>),
                cited_object_insert: None,
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
                projection_insert: None,
                projection_table: None,
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
    /// Takes the whole frozen registry, not just its `SchemaInfo` slice,
    /// because the flavor contracts are part of what a PG registration has
    /// to agree with: `pg_sidecar!(owner_pinned: true)` and
    /// `TransferRule::RetainAtSource` are two statements of the same fact,
    /// and until this check existed nothing compared them.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` if a sidecar-bearing schema has no
    /// PG registration, a registration is orphaned, a table name drifts,
    /// an insert-capable kind has no typed inserter, or a registration's
    /// `owner_pinned` flag contradicts its schema's declared transfer rule.
    pub fn freeze_against(
        mut self,
        registry: &proxima_core::FlavorRegistryFrozen,
    ) -> Result<PgSidecarRegistryFrozen, StorageError> {
        let schemas = registry.schemas();
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
            if matches!(
                entry.key.kind,
                PayloadKind::CitedObject | PayloadKind::CitationMapping
            ) {
                continue;
            }
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

        self.check_owner_pinned_against_contracts(registry)?;
        self.check_keep_is_owner_pinned(registry)?;
        self.attach_projections(registry)?;

        Ok(PgSidecarRegistryFrozen {
            entries: Arc::new(self.entries),
        })
    }

    /// Generate each projected schema's maintenance statement, once, here.
    ///
    /// The write path then has no decision left to make: a schema whose
    /// contract declares a search surface carries the `INSERT` that keeps
    /// its projection row, and one that does not carries `None`. Nothing
    /// downstream re-reads the contract, so "the write path forgot a new
    /// searchable schema" is not a reachable state — registering the
    /// sidecar is what wires it.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when a declared table, column or configuration
    /// name is not a valid `PostgreSQL` identifier.
    fn attach_projections(
        &mut self,
        registry: &proxima_core::FlavorRegistryFrozen,
    ) -> Result<(), StorageError> {
        for entry in self.entries.values_mut() {
            if !matches!(
                entry.key.kind,
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
            ) {
                continue;
            }
            let Some((flavor_id, _)) = entry.key.schema_id.as_str().split_once('/') else {
                continue;
            };
            let Some(contract) = registry.flavor_contract(flavor_id) else {
                continue;
            };
            let Some(spec) = contract.projection.spec() else {
                continue;
            };
            let Some(schema) = contract
                .schemas
                .iter()
                .find(|schema| schema.schema_id() == entry.key.schema_id)
            else {
                continue;
            };
            if !schema.search.is_projected() {
                continue;
            }
            entry.projection_insert = Some(crate::projection::projection_insert_sql(spec, schema)?);
            entry.projection_table = Some(spec.table.to_owned());
        }
        Ok(())
    }

    /// The macro flag and the contract must agree about which sidecars stay
    /// with the source owner on transfer.
    ///
    /// Before the flavor contract, `owner_pinned` was declared once in
    /// `pg_sidecar!` and consumed by the Postgres adapter alone: core built
    /// the owner-inverse table lists without it and the adapter appended its
    /// own leg on the way past. A schema could therefore be owner-pinned in
    /// storage and `Follow` in the contract with nothing to notice, which is
    /// a wrong-owner bundle rather than a crash.
    ///
    /// Schemas whose flavor registered no contract are skipped: a flavor
    /// without a contract has made no claim to contradict.
    ///
    /// EVERY LINKED FLAVOR THAT SHIPS A CONTRACT IS CHECKED, and both
    /// shipped flavors now do: the code flavor's sixteen sidecars stopped
    /// being exempt when `proxima-code` declared its own contract, and none
    /// of them may be `owner_pinned` because none declares
    /// `TransferRule::RetainAtSource`. Skipping a contract-less flavor
    /// remains the only sound reading of an absent contract (the
    /// alternative asserts every contract-less sidecar is `Follow`, which
    /// is a claim nobody made), so the cross-check still grows teeth per
    /// flavor rather than all at once.
    fn check_owner_pinned_against_contracts(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
    ) -> Result<(), StorageError> {
        let retained = registry.retain_at_source_sidecar_tables();
        for entry in self.entries.values() {
            if !matches!(
                entry.key.kind,
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
            ) {
                continue;
            }
            let Some((flavor_id, _)) = entry.key.schema_id.as_str().split_once('/') else {
                continue;
            };
            if registry.flavor_contract(flavor_id).is_none() {
                continue;
            }
            let declared = retained.iter().any(|table| table == &entry.sidecar_table);
            if declared != entry.owner_pinned {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar {} registers owner_pinned={} but {} v{} declares {}",
                    entry.sidecar_table,
                    entry.owner_pinned,
                    entry.key.schema_id.as_str(),
                    entry.key.schema_version.into_inner(),
                    if declared {
                        "TransferRule::RetainAtSource"
                    } else {
                        "a transfer rule that follows the memory"
                    },
                )));
            }
        }
        Ok(())
    }

    /// `ForgetRule::Keep` on a memory sidecar and
    /// `pg_sidecar!(owner_pinned: true)` are the same fact, because
    /// owner-pinning is the ONLY mechanism by which the forget can honour
    /// the declaration.
    ///
    /// The forget touches a stamped memory sidecar twice — `dump_stamped_
    /// sidecars` reads its row into the cold record, and
    /// `delete_memory_dependents` deletes it — and the single test either
    /// walk applies is `is_owner_pinned`. Nothing in either one reads
    /// `ForgetRule`. So a surface that declared `Keep` without the flag had
    /// its rows dumped and deleted like any other, and the declaration was
    /// prose. Core ships exactly one `Keep` memory sidecar,
    /// `mcp_call_logged_v1`, and its rows survive because it is ALSO
    /// `RetainAtSource` and therefore owner-pinned — an unrelated property
    /// that happens to imply the right behaviour.
    ///
    /// WHY A REFUSAL AND NOT A SKIP IN THE WALK. Teaching
    /// `delete_memory_dependents` to skip `Kept` fixes half of it and breaks
    /// the other half: the dump walk would still copy the row into the cold
    /// record, the rows would still be sitting in the hot table, and
    /// `restore_registered_sidecars` INSERTs without `ON CONFLICT` — so the
    /// next hydrate dies on a primary key. Honouring `Keep` properly means
    /// exempting the table from the forget/hydrate cycle in both walks,
    /// which is precisely and entirely what `owner_pinned` already means
    /// ("Owner-pinned sidecars do not take part in forget/hydrate at all").
    /// Building a second mechanism for it would be the two-descriptions-of-
    /// one-fact defect this whole phase exists to remove.
    ///
    /// Checked in BOTH directions, like its sibling above. An owner-pinned
    /// sidecar's rows are kept whatever its `ForgetRule` says, so declaring
    /// `DumpThenDelete` there is equally a claim the substrate does not
    /// honour.
    ///
    /// `ForgetLeg::derive` is the classifier — the same one freeze and the
    /// forget read — rather than a second reading of `ForgetRule` here.
    fn check_keep_is_owner_pinned(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
    ) -> Result<(), StorageError> {
        let surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(registry);
        for entry in self.entries.values() {
            if !matches!(
                entry.key.kind,
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
            ) {
                continue;
            }
            let Some((flavor_id, _)) = entry.key.schema_id.as_str().split_once('/') else {
                continue;
            };
            if registry.flavor_contract(flavor_id).is_none() {
                continue;
            }
            let kept = matches!(
                surfaces.forget_leg(&entry.sidecar_table),
                proxima_core::flavor::ForgetLeg::Kept { .. }
            );
            if kept != entry.owner_pinned {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar {} registers owner_pinned={} but {} v{} declares {}; \
                     the forget exempts a memory sidecar from dump and delete on the \
                     owner_pinned flag alone, so these two must agree or the \
                     declaration is prose",
                    entry.sidecar_table,
                    entry.owner_pinned,
                    entry.key.schema_id.as_str(),
                    entry.key.schema_version.into_inner(),
                    if kept {
                        "ForgetRule::Keep"
                    } else {
                        "a forget rule that destroys or dumps its rows"
                    },
                )));
            }
        }
        Ok(())
    }
}
