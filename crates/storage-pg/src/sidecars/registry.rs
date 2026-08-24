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
                    memory_key_column: Some(<P as PgMemoryPayload>::MEMORY_KEY_COLUMN),
                    memory_insert: Some(insert_memory_sidecar::<P>),
                    memory_load: Some(load_memory_payload::<P>),
                    memory_load_batch: Some(load_memory_payload_batch::<P>),
                    cited_object_insert: None,
                    citation_mapping_insert: None,
                    goal_insert: None,
                    goal_copy: None,
                    projection_insert: None,
                    projection_table: None,
                    projection_language: None,
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
                    memory_key_column: None,
                    memory_insert: None,
                    memory_load: None,
                    memory_load_batch: None,
                    cited_object_insert: None,
                    citation_mapping_insert: None,
                    goal_insert: Some(insert_goal_sidecar::<P>),
                    goal_copy: Some(copy_goal_sidecar::<P>),
                    projection_insert: None,
                    projection_table: None,
                    projection_language: None,
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
                memory_key_column: None,
                memory_insert: None,
                memory_load: None,
                memory_load_batch: None,
                cited_object_insert: Some(insert_cited_object_sidecar::<P>),
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
                projection_insert: None,
                projection_table: None,
                projection_language: None,
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
                    memory_key_column: None,
                    memory_insert: None,
                    memory_load: None,
                    memory_load_batch: None,
                    cited_object_insert: None,
                    citation_mapping_insert: Some(insert_citation_mapping_sidecar::<P>),
                    goal_insert: None,
                    goal_copy: None,
                    projection_insert: None,
                    projection_table: None,
                    projection_language: None,
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
                memory_key_column: Some(<P as PgMemoryPayload>::MEMORY_KEY_COLUMN),
                memory_insert: Some(insert_memory_sidecar::<P>),
                memory_load: Some(load_memory_payload::<P>),
                memory_load_batch: Some(load_memory_payload_batch::<P>),
                cited_object_insert: None,
                citation_mapping_insert: None,
                goal_insert: None,
                goal_copy: None,
                projection_insert: None,
                projection_table: None,
                projection_language: None,
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
    /// and this is what compares them.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` if a sidecar-bearing schema has no
    /// PG registration, a registration is orphaned, a table name drifts,
    /// an insert-capable kind has no typed inserter, a registration's
    /// `owner_pinned` flag contradicts its schema's declared transfer rule,
    /// or its `pg_sidecar!(key: …)` column contradicts the `Surface` that
    /// declares the same table.
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
        self.check_memory_key_against_contracts(registry)?;
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
            let proxima_core::flavor::SearchProjectionDecl::Projected { language, .. } =
                &schema.search
            else {
                continue;
            };
            entry.projection_insert =
                Some(crate::projection::projection_insert_sql(contract, schema)?);
            entry.projection_table = Some(spec.table.to_owned());
            // Frozen beside the statement it explains: the statement's
            // shape and the policy that produced it are one decision, and
            // reading the policy back off the contract at write time would
            // let the two answer differently.
            entry.projection_language = Some(*language);
        }
        Ok(())
    }

    /// The macro flag and the contract must agree about which sidecars stay
    /// with the source owner on transfer.
    ///
    /// The two are read by different code: core builds the owner-inverse
    /// table lists from the contract, and the Postgres adapter appends its
    /// owner-pinned leg from the macro flag. A schema owner-pinned in
    /// storage and `Follow` in the contract is a wrong-owner bundle rather
    /// than a crash, which is why they are compared here.
    ///
    /// EVERY LINKED FLAVOR THAT SHIPS A CONTRACT IS CHECKED. Schemas whose
    /// flavor registered no contract are skipped: a flavor without a
    /// contract has made no claim to contradict, and that is the only sound
    /// reading of an absent contract — the alternative asserts every
    /// contract-less sidecar is `Follow`, which is a claim nobody made. So
    /// the cross-check grows teeth per flavor rather than all at once.
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

    /// `pg_sidecar!(key: …)` and the contract `Surface`'s
    /// `KeyShape::MemoryT { column }` are the same fact, and this is what
    /// compares them.
    ///
    /// WHY IT MATTERS THAT THEY DISAGREE SILENTLY. The two are read by
    /// different generators: the typed `INSERT` and the batch read spell the
    /// macro's column, while the projection `INSERT`, the substring arm and
    /// the snippet lookup spell the `Surface`'s. A disagreement therefore
    /// writes the sidecar row on one column and looks for it on another —
    /// a table whose rows exist and whose projection is empty, with nothing
    /// failing anywhere. Neither generator can notice: each is internally
    /// consistent with the declaration it reads.
    ///
    /// A table with NO surface is skipped rather than refused. The
    /// projection and embedding lanes already refuse it at core freeze when
    /// they need one (`ProjectedSidecarNotMemoryKeyed`,
    /// `EmbeddedSidecarNotMemoryKeyed`); a sidecar that is neither projected
    /// nor embedded has made no second claim for this check to compare
    /// against, and inventing one here would be this function asserting a
    /// declaration nobody wrote.
    fn check_memory_key_against_contracts(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
    ) -> Result<(), StorageError> {
        for entry in self.entries.values() {
            let Some(registered) = entry.memory_key_column else {
                continue;
            };
            let Some((flavor_id, _)) = entry.key.schema_id.as_str().split_once('/') else {
                continue;
            };
            let Some(contract) = registry.flavor_contract(flavor_id) else {
                continue;
            };
            let Some(declared) = contract.sidecar_memory_key_column(&entry.sidecar_table) else {
                continue;
            };
            if declared != registered {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar {} registers `pg_sidecar!(key: {registered})` but flavor \
                     {flavor_id} declares its Surface as KeyShape::MemoryT {{ column: \
                     {declared:?} }}; {} v{} would have its row written on {registered} and its \
                     projection, substring and snippet statements read from {declared}, so the \
                     two declarations have to name one column",
                    entry.sidecar_table,
                    entry.key.schema_id.as_str(),
                    entry.key.schema_version.into_inner(),
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
    /// `ForgetRule`. So a surface that declares `Keep` without the flag has
    /// its rows dumped and deleted like any other, and the declaration is
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
    /// A second mechanism for it would be a second description of one fact.
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

#[cfg(test)]
mod memory_key_agreement_tests {
    use super::{PgSidecarEntry, PgSidecarKey, PgSidecarRegistry};
    use proxima_core::verbs::schema::PayloadKind;
    use proxima_core::{AgentNoteV1, FactPayload, SchemaId, SchemaVersion, StorageError};
    use std::collections::HashMap;

    const AGENT_NOTE: &str = "proxima_core.agent_note_v1";

    /// One registration of flavor #0's note sidecar, keyed on `key_column`.
    ///
    /// Everything else is what `add_fact` would build. The inserters are
    /// stubs because this check reads neither: it compares two declarations.
    fn registry_keyed_on(key_column: &'static str) -> PgSidecarRegistry {
        let key = PgSidecarKey::new(
            PayloadKind::Fact,
            SchemaId::new(AgentNoteV1::SCHEMA_ID.to_owned()),
            SchemaVersion::new(1),
        );
        let entry = PgSidecarEntry {
            key: key.clone(),
            sidecar_table: AGENT_NOTE.to_owned(),
            owner_pinned: false,
            memory_key_column: Some(key_column),
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
        };
        PgSidecarRegistry {
            entries: HashMap::from([(key, entry)]),
        }
    }

    /// The shipped registration agrees with the contract, so freeze passes.
    ///
    /// The positive half matters as much as the negative: a check that only
    /// ever fires on doctored input is indistinguishable from one whose
    /// lookup silently finds nothing.
    #[test]
    fn a_registration_keyed_as_its_surface_declares_is_admitted() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        assert_eq!(
            proxima_core::FLAVOR_0.sidecar_memory_key_column(AGENT_NOTE),
            Some("t"),
            "the note's Surface declares its memory key"
        );
        registry_keyed_on("t")
            .check_memory_key_against_contracts(&registry)
            .expect("the macro key and the Surface key are the same column");
    }

    /// A registration keyed on a column its `Surface` does not declare is
    /// refused, by an error naming BOTH declarations and both values.
    ///
    /// Without this the two generators stay internally consistent and
    /// disagree with each other: the typed `INSERT` writes the row on
    /// `note_memory_id`, the projection `INSERT` looks for it on `t`, and
    /// the schema's search corpus is silently empty.
    #[test]
    fn a_registration_keyed_off_its_surface_is_refused_naming_both() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let err = registry_keyed_on("note_memory_id")
            .check_memory_key_against_contracts(&registry)
            .expect_err("the two declarations name different columns");
        let StorageError::ConstraintViolation(message) = err else {
            panic!("a declaration disagreement is a constraint violation");
        };
        for named in [
            AGENT_NOTE,
            "note_memory_id",
            "KeyShape::MemoryT",
            "\"t\"",
            AgentNoteV1::SCHEMA_ID,
        ] {
            assert!(
                message.contains(named),
                "the refusal names {named}, so the reader can find both declarations: {message}"
            );
        }
    }
}
