use super::dispatch::{
    copy_goal_sidecar, insert_goal_sidecar, insert_memory_sidecar, load_memory_payload,
    load_memory_payload_batch,
};
use super::{
    AbstractionPayload, Arc, FactPayload, GoalPayload, HashMap, PayloadKind, PerspectivePayload,
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
                    owner: <P as PgMemoryPayload>::OWNER,
                    memory_key_column: Some(<P as PgMemoryPayload>::MEMORY_KEY_COLUMN),
                    memory_insert: Some(insert_memory_sidecar::<P>),
                    memory_load: Some(load_memory_payload::<P>),
                    memory_load_batch: Some(load_memory_payload_batch::<P>),
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
                    owner: crate::sidecars::SidecarOwnerMode::None,
                    memory_key_column: None,
                    memory_insert: None,
                    memory_load: None,
                    memory_load_batch: None,
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
                owner: <P as PgMemoryPayload>::OWNER,
                memory_key_column: Some(<P as PgMemoryPayload>::MEMORY_KEY_COLUMN),
                memory_insert: Some(insert_memory_sidecar::<P>),
                memory_load: Some(load_memory_payload::<P>),
                memory_load_batch: Some(load_memory_payload_batch::<P>),
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
    /// to agree with: `pg_sidecar!` owner mode (`Pinned` / `Follows` / `None`)
    /// and the contract's transfer / `owner_column` / Keep declarations are
    /// statements of the same fact, and this is what compares them.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` if a sidecar-bearing schema has no
    /// PG registration, a registration is orphaned, a table name drifts,
    /// an insert-capable kind has no typed inserter, a registration's
    /// `OWNER` mode contradicts its schema's declared transfer / Keep rule,
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
                PayloadKind::Goal => entry.goal_insert.is_some() && entry.goal_copy.is_some(),
                // Citation schemas, typed or opaque, have no sidecar: core
                // records no sidecar table for them, so no schema reaches
                // this loop under this kind and no PG registration can
                // serve one.
                PayloadKind::CitedObject => false,
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

        self.check_owner_pinned_against_contracts(registry)?;
        self.check_keep_is_owner_pinned(registry)?;
        self.check_memory_key_against_contracts(registry)?;
        self.attach_projections(registry)?;

        Ok(PgSidecarRegistryFrozen {
            entries: Arc::new(self.entries),
            flavor_schema_names: proxima_core::composed_schema_names(registry)
                .into_iter()
                .filter(|schema| schema != "proxima_core")
                .collect::<Vec<_>>()
                .into(),
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

    /// The macro's owner mode and the contract must agree about which
    /// sidecars carry an owner and what that owner does on transfer.
    ///
    /// The mapping is three-way: `SidecarOwnerMode::Pinned` ⇔
    /// `TransferRule::RetainAtSource`; `SidecarOwnerMode::Follows` ⇔
    /// `TransferRule::Follow` with `Surface.owner_column` set; anything else
    /// ⇔ `SidecarOwnerMode::None`.
    ///
    /// The two are read by different code: core builds the owner-inverse
    /// table lists from the contract, and the Postgres adapter appends its
    /// pinned leg and stamps `owner_id` from the macro mode. A disagreement
    /// is a wrong-owner bundle or an unstamped row rather than a crash,
    /// which is why they are compared here.
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
        use crate::sidecars::SidecarOwnerMode;
        use proxima_core::flavor::TransferRule;

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
            let Some(contract) = registry.flavor_contract(flavor_id) else {
                continue;
            };
            let expected = if retained.iter().any(|table| table == &entry.sidecar_table) {
                SidecarOwnerMode::Pinned
            } else if contract
                .surface_for(&entry.sidecar_table)
                .is_some_and(|surface| {
                    matches!(surface.transfer, TransferRule::Follow)
                        && surface.owner_column.is_some()
                })
            {
                SidecarOwnerMode::Follows
            } else {
                SidecarOwnerMode::None
            };
            if expected != entry.owner {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar {} registers OWNER={:?} but {} v{} declares {}",
                    entry.sidecar_table,
                    entry.owner,
                    entry.key.schema_id.as_str(),
                    entry.key.schema_version.into_inner(),
                    match expected {
                        SidecarOwnerMode::Pinned => "TransferRule::RetainAtSource",
                        SidecarOwnerMode::Follows => {
                            "TransferRule::Follow with Surface.owner_column set"
                        }
                        SidecarOwnerMode::None => "a transfer rule that follows the memory",
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

    /// `Keep` and [`crate::sidecars::SidecarOwnerMode::Pinned`] must agree: both dump
    /// and delete exempt owner-pinned rows. Follows + Keep is refused.
    /// A mismatch either loses kept data or duplicates it on restore.
    /// `ForgetLeg::derive` supplies the shared classifier.
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
            if kept != entry.owner.is_pinned() {
                return Err(StorageError::ConstraintViolation(format!(
                    "PG sidecar {} registers OWNER={:?} but {} v{} declares {}; the forget exempts a memory sidecar from dump and delete on SidecarOwnerMode::Pinned alone, so Keep requires Pinned (Follows + Keep is refused) or the declaration is prose",
                    entry.sidecar_table,
                    entry.owner,
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
            owner: crate::sidecars::SidecarOwnerMode::None,
            memory_key_column: Some(key_column),
            memory_insert: Some(|_, _, _, _| Box::pin(async { Ok(()) })),
            memory_load: None,
            memory_load_batch: Some(|_, _, _| Box::pin(async { Ok(Vec::new()) })),
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

#[cfg(test)]
mod owner_mode_agreement_tests {
    use super::{PgSidecarEntry, PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen};
    use crate::sidecars::SidecarOwnerMode;
    use proxima_core::flavor::{
        CounterRule, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule, KeyShape,
        ProjectionDecl, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl, Surface,
        TransferRule,
    };
    use proxima_core::verbs::schema::PayloadKind;
    use proxima_core::{FactPayload, SchemaId, SchemaVersion, StorageError};
    use std::collections::HashMap;

    const FLAVOR: &str = "test-owner-mode";
    const TABLE: &str = "test_owner_mode.stamp_v1";
    const SCHEMA_ID: &str = "test-owner-mode/stamp-v1";

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct StampFixture {
        mark: String,
    }

    impl FactPayload for StampFixture {
        const SCHEMA_ID: &'static str = SCHEMA_ID;
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            self.mark.as_bytes().to_vec()
        }

        fn render(&self) -> String {
            self.mark.clone()
        }

        fn sidecar_table() -> Option<&'static str> {
            Some(TABLE)
        }

        fn json_schema() -> Option<serde_json::Value> {
            Some(serde_json::json!({
                "type": "object",
                "properties": { "mark": { "type": "string" } },
                "required": ["mark"],
            }))
        }
    }

    const fn surface(
        transfer: TransferRule,
        forget: ForgetRule,
        owner_column: Option<&'static str>,
    ) -> Surface {
        Surface {
            table: TABLE,
            key: KeyShape::MemoryT { column: "t" },
            owner_column,
            transfer,
            erase: EraseRule::ByKey,
            export: ExportRule::Rows,
            forget,
            lexical_language_column: None,
            counter: CounterRule::Counted("sidecar_rows"),
            completeness: None,
        }
    }

    fn pg_registry_with(owner: SidecarOwnerMode) -> PgSidecarRegistry {
        let key = PgSidecarKey::new(
            PayloadKind::Fact,
            SchemaId::new(SCHEMA_ID.to_owned()),
            SchemaVersion::new(1),
        );
        let entry = PgSidecarEntry {
            key: key.clone(),
            sidecar_table: TABLE.to_owned(),
            owner,
            memory_key_column: Some("t"),
            memory_insert: Some(|_, _, _, _| Box::pin(async { Ok(()) })),
            memory_load: Some(|_, _| Box::pin(async { Ok(None) })),
            memory_load_batch: Some(|_, _, _| Box::pin(async { Ok(Vec::new()) })),
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

    #[test]
    fn follows_tables_omit_other_owner_modes_and_are_distinct_in_name_order() {
        let mut entries = HashMap::new();
        for (version, owner, table) in [
            (1, SidecarOwnerMode::Follows, "test_owner_mode.zeta_v1"),
            (2, SidecarOwnerMode::Pinned, "test_owner_mode.pinned_v1"),
            (3, SidecarOwnerMode::None, "test_owner_mode.plain_v1"),
            (4, SidecarOwnerMode::Follows, "test_owner_mode.alpha_v1"),
            (5, SidecarOwnerMode::Follows, "test_owner_mode.zeta_v1"),
        ] {
            let mut entry = pg_registry_with(owner)
                .entries
                .into_values()
                .next()
                .expect("one fixture registration");
            entry.key.schema_version = SchemaVersion::new(version);
            entry.sidecar_table = table.to_owned();
            entries.insert(entry.key.clone(), entry);
        }
        let frozen = PgSidecarRegistryFrozen {
            entries: std::sync::Arc::new(entries),
            ..PgSidecarRegistryFrozen::default()
        };
        assert_eq!(
            frozen.owner_follows_memory_sidecar_tables(),
            ["test_owner_mode.alpha_v1", "test_owner_mode.zeta_v1"]
        );
    }

    fn freeze_contract(contract: &'static FlavorContract) -> proxima_core::FlavorRegistryFrozen {
        let mut registry = proxima_core::FlavorRegistry::new();
        registry.add_fact_schema_or_panic_for_tests::<StampFixture>();
        registry.add_contract_or_panic_for_tests(contract);
        registry.freeze_or_panic_for_tests()
    }

    #[test]
    fn freeze_refuses_follows_declared_retain_at_source() {
        static CONTRACT: FlavorContract = FlavorContract {
            flavor_id: FLAVOR,
            ordinal: 91,
            schemas: &[SchemaContract {
                id: SchemaRef::new(FLAVOR, "stamp", 1),
                kind: PayloadKind::Fact,
                sidecar_table: Some(TABLE),
                search: SearchProjectionDecl::None {
                    why: "owner-mode freeze fixture",
                },
                embedding: EmbeddingRecipe::Never {
                    why: "owner-mode freeze fixture",
                },
                transfer: TransferRule::RetainAtSource {
                    why: "fixture: retain so Follows must disagree",
                },
                provenance: Provenance::None,
                surfaces: &[surface(
                    TransferRule::RetainAtSource {
                        why: "fixture: retain so Follows must disagree",
                    },
                    ForgetRule::Keep {
                        why: "a retained sidecar stays with its source owner",
                    },
                    Some("owner_id"),
                )],
                natural_key_columns: &[],
            }],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            projection: ProjectionDecl::None {
                why: "owner-mode freeze fixture",
            },
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
        };
        let frozen = freeze_contract(&CONTRACT);
        let err = pg_registry_with(SidecarOwnerMode::Follows)
            .check_owner_pinned_against_contracts(&frozen)
            .expect_err("Follows vs RetainAtSource");
        let StorageError::ConstraintViolation(message) = err else {
            panic!("expected ConstraintViolation, got {err:?}");
        };
        for named in ["OWNER=Follows", "RetainAtSource", TABLE] {
            assert!(message.contains(named), "refusal names {named}: {message}");
        }

        let mut reg = pg_registry_with(SidecarOwnerMode::Pinned);
        crate::sidecars::register_core_pg_sidecars(&mut reg);
        let frozen = reg
            .freeze_against(&frozen)
            .expect("the matching pinned registration freezes");
        assert!(frozen.owner_follows_memory_sidecar_tables().is_empty());
        assert_eq!(
            frozen.flavor_schema_names(),
            ["test_owner_mode"],
            "a composition with no Follows must still clear its stale marks"
        );
    }

    #[test]
    fn freeze_refuses_follows_declared_keep() {
        static CONTRACT: FlavorContract = FlavorContract {
            flavor_id: FLAVOR,
            ordinal: 92,
            schemas: &[SchemaContract {
                id: SchemaRef::new(FLAVOR, "stamp", 1),
                kind: PayloadKind::Fact,
                sidecar_table: Some(TABLE),
                search: SearchProjectionDecl::None {
                    why: "owner-mode freeze fixture",
                },
                embedding: EmbeddingRecipe::Never {
                    why: "owner-mode freeze fixture",
                },
                transfer: TransferRule::Follow,
                provenance: Provenance::None,
                surfaces: &[surface(
                    TransferRule::Follow,
                    ForgetRule::Keep {
                        why: "fixture: Keep so Follows must disagree",
                    },
                    Some("owner_id"),
                )],
                natural_key_columns: &[],
            }],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            projection: ProjectionDecl::None {
                why: "owner-mode freeze fixture",
            },
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
        };
        let frozen = freeze_contract(&CONTRACT);
        pg_registry_with(SidecarOwnerMode::Follows)
            .check_owner_pinned_against_contracts(&frozen)
            .expect("Follows matches Follow+owner_column");
        let err = pg_registry_with(SidecarOwnerMode::Follows)
            .check_keep_is_owner_pinned(&frozen)
            .expect_err("Follows + Keep");
        let StorageError::ConstraintViolation(message) = err else {
            panic!("expected ConstraintViolation, got {err:?}");
        };
        for named in ["OWNER=Follows", "ForgetRule::Keep", "Pinned"] {
            assert!(message.contains(named), "refusal names {named}: {message}");
        }
    }

    #[test]
    fn freeze_admits_follows_with_follow_and_owner_column() {
        static CONTRACT: FlavorContract = FlavorContract {
            flavor_id: FLAVOR,
            ordinal: 93,
            schemas: &[SchemaContract {
                id: SchemaRef::new(FLAVOR, "stamp", 1),
                kind: PayloadKind::Fact,
                sidecar_table: Some(TABLE),
                search: SearchProjectionDecl::None {
                    why: "owner-mode freeze fixture",
                },
                embedding: EmbeddingRecipe::Never {
                    why: "owner-mode freeze fixture",
                },
                transfer: TransferRule::Follow,
                provenance: Provenance::None,
                surfaces: &[surface(
                    TransferRule::Follow,
                    ForgetRule::DumpThenDelete,
                    Some("owner_id"),
                )],
                natural_key_columns: &[],
            }],
            state_surfaces: &[Surface {
                table: "test_owner_state.jobs",
                key: KeyShape::Custom(&["id"]),
                erase: EraseRule::ByOwner,
                ..surface(
                    TransferRule::RetainAtSource {
                        why: "state stays with its owner",
                    },
                    ForgetRule::Keep {
                        why: "state is independent of memory",
                    },
                    Some("owner_id"),
                )
            }],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            projection: ProjectionDecl::None {
                why: "owner-mode freeze fixture",
            },
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
        };
        let frozen = freeze_contract(&CONTRACT);
        let mut reg = pg_registry_with(SidecarOwnerMode::Follows);
        reg.check_owner_pinned_against_contracts(&frozen)
            .expect("Follows matches");
        reg.check_keep_is_owner_pinned(&frozen)
            .expect("DumpThenDelete admits Follows");
        crate::sidecars::register_core_pg_sidecars(&mut reg);
        let frozen = reg
            .freeze_against(&frozen)
            .expect("complete fixture coverage");
        assert_eq!(frozen.owner_follows_memory_sidecar_tables(), [TABLE]);
        assert_eq!(
            frozen.flavor_schema_names(),
            ["test_owner_mode", "test_owner_state"]
        );
        let amended =
            frozen.with_unusable_memory_table("test-owner-mode/unusable-v1", "unusable.sidecar_v1");
        assert_eq!(amended.flavor_schema_names(), frozen.flavor_schema_names());
    }

    /// `Follows` needs somewhere to stamp the owner. A `Follow` surface with
    /// no `owner_column` never reaches the PG check: core freeze refuses it
    /// first as a surface no transfer leg can move, so no frozen registry can
    /// pair a `Follows` registration with one.
    #[test]
    fn freeze_refuses_follows_declared_follow_without_owner_column() {
        static CONTRACT: FlavorContract = FlavorContract {
            flavor_id: FLAVOR,
            ordinal: 94,
            schemas: &[SchemaContract {
                id: SchemaRef::new(FLAVOR, "stamp", 1),
                kind: PayloadKind::Fact,
                sidecar_table: Some(TABLE),
                search: SearchProjectionDecl::None {
                    why: "owner-mode freeze fixture",
                },
                embedding: EmbeddingRecipe::Never {
                    why: "owner-mode freeze fixture",
                },
                transfer: TransferRule::Follow,
                provenance: Provenance::None,
                surfaces: &[surface(
                    TransferRule::Follow,
                    ForgetRule::DumpThenDelete,
                    None,
                )],
                natural_key_columns: &[],
            }],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            projection: ProjectionDecl::None {
                why: "owner-mode freeze fixture",
            },
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
        };
        let mut registry = proxima_core::FlavorRegistry::new();
        registry.add_fact_schema_or_panic_for_tests::<StampFixture>();
        registry.add_contract_or_panic_for_tests(&CONTRACT);
        let err = registry
            .try_freeze()
            .expect_err("core freeze refuses a Follow surface without owner_column");
        assert!(
            matches!(
                err,
                proxima_core::flavor::FlavorRegistryError::UnmovableSurface {
                    flavor_id: FLAVOR,
                    table: TABLE,
                }
            ),
            "expected UnmovableSurface for {TABLE}, got {err:?}"
        );
    }

    /// The symmetric case: a `Follow` surface with `owner_column` set expects
    /// `Follows`, so a `None` registration (which never stamps the owner
    /// column) is refused.
    #[test]
    fn freeze_refuses_none_declared_follow_with_owner_column() {
        static CONTRACT: FlavorContract = FlavorContract {
            flavor_id: FLAVOR,
            ordinal: 95,
            schemas: &[SchemaContract {
                id: SchemaRef::new(FLAVOR, "stamp", 1),
                kind: PayloadKind::Fact,
                sidecar_table: Some(TABLE),
                search: SearchProjectionDecl::None {
                    why: "owner-mode freeze fixture",
                },
                embedding: EmbeddingRecipe::Never {
                    why: "owner-mode freeze fixture",
                },
                transfer: TransferRule::Follow,
                provenance: Provenance::None,
                surfaces: &[surface(
                    TransferRule::Follow,
                    ForgetRule::DumpThenDelete,
                    Some("owner_id"),
                )],
                natural_key_columns: &[],
            }],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            projection: ProjectionDecl::None {
                why: "owner-mode freeze fixture",
            },
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
        };
        let frozen = freeze_contract(&CONTRACT);
        let err = pg_registry_with(SidecarOwnerMode::None)
            .check_owner_pinned_against_contracts(&frozen)
            .expect_err("None vs Follow with owner_column");
        let StorageError::ConstraintViolation(message) = err else {
            panic!("expected ConstraintViolation, got {err:?}");
        };
        for named in [
            "OWNER=None",
            "TransferRule::Follow with Surface.owner_column set",
            TABLE,
        ] {
            assert!(message.contains(named), "refusal names {named}: {message}");
        }
    }
}
