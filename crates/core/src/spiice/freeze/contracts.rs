//! The contract cross-checks: every spiice's declared [`SpiiceContract`]
//! against what that spiice actually registered.
//!
//! The partition exists because a contract is a claim and a registration is
//! the fact — the two are written in different places by different hands,
//! and nothing but these sweeps keeps them equal. The leg coverage
//! ([`super::legs`]) and the projection statement ([`super::projection`])
//! are cross-checks too, kept in files of their own because each answers a
//! question about one verb rather than about the declaration as a whole.
//!
//! [`SpiiceContract`]: crate::spiice::SpiiceContract

use super::{SpiiceRegistry, SpiiceRegistryError};

fn validate_tag_sidecar(
    contract: &crate::spiice::contract::SpiiceContract,
    schema: &crate::spiice::contract::SchemaContract,
) -> Result<(), SpiiceRegistryError> {
    if let crate::spiice::contract::SearchProjectionDecl::None {
        tag_column: Some(tag_column),
        ..
    } = schema.search
    {
        let memory_keyed = schema.sidecar_table.is_some_and(|table| {
            contract.all_surfaces().any(|surface| {
                surface.table == table
                    && matches!(
                        surface.key,
                        crate::spiice::contract::KeyShape::MemoryT { .. }
                    )
            })
        });
        if !memory_keyed {
            return Err(SpiiceRegistryError::TagColumnNotMemoryKeyed {
                spiice_id: contract.spiice_id,
                schema_id: schema.schema_id(),
                tag_column,
            });
        }
    }
    Ok(())
}

impl SpiiceRegistry {
    /// Cross-check the declarations against the registrations.
    ///
    /// This is the check that makes "everything is a spiice" structural
    /// rather than aspirational: a schema registered without a contract
    /// entry, or a contract entry with no registration, fails the build of
    /// the composed binary rather than going missing from an erase sweep.
    ///
    /// It takes two checks to close the first of those, because a schema
    /// carries its spiice in a PREFIX and a prefix nothing declares names
    /// nothing to accuse. `SchemaWithoutContract` sweeps the registrations
    /// under a declared spiice's prefix; `UnclaimedRegistration` holds the
    /// spiice itself — the `SpiiceDescriptor` every composed spiice
    /// registers — to declaring one. A registration made under neither a
    /// descriptor nor a contract is a registry no binary composes.
    pub(super) fn validate_contracts(&self) -> Result<(), SpiiceRegistryError> {
        let mut seen_ordinals = std::collections::HashSet::new();
        let mut has_core = false;
        for contract in &self.contracts {
            if !seen_ordinals.insert(contract.ordinal) {
                return Err(SpiiceRegistryError::DuplicateSpiiceOrdinal {
                    ordinal: contract.ordinal,
                    spiice_id: contract.spiice_id,
                });
            }
            if contract.is_core() {
                has_core = true;
            } else if !contract.resources.is_empty() {
                // Resources are spiice #0's. A spiice resource would need
                // its own scope-key namespace, a URI-template parser for its
                // parameters and a pagination contract — a feature with its
                // own design, not a forwarding line.
                return Err(SpiiceRegistryError::ResourcesNotPermitted {
                    spiice_id: contract.spiice_id,
                });
            }
            // Registrations first, declarations second, and deliberately
            // so. A contract entry with no registration is the defect that
            // makes every OTHER reading of that entry meaningless, so it is
            // the one to report. This order decides which error a broken
            // contract reports at boot, so a fixture that needs a later
            // validator registers rather than asking for a reorder (see
            // `register_fixture_schema`).
            self.validate_contract_schemas(contract)?;
            Self::validate_contract_projection(contract)?;
            // After the uniformity rules, deliberately. Both read the same
            // declaration, and a spiice whose schemas cannot agree on what
            // one statement spells is broken whether or not a query would
            // ever have reached that statement.
            Self::validate_projection_is_reachable(contract)?;
            // Before the reachability and leg checks, so a non-core spiice
            // leaning on a hand-written leg is told that, not that a surface
            // is unreachable or a leg mismatched.
            Self::validate_core_only_legs(contract)?;
            Self::validate_contract_surfaces(contract)?;
            Self::validate_erase_legs(contract)?;
            Self::validate_transfer_legs(contract)?;
            Self::validate_forget_legs(contract)?;
        }
        // An empty contracts list is the same defect at full width, not an
        // exemption from it. Core is non-removable, so any registry that
        // registered ANYTHING has a spiice #0 to declare — and reading an
        // empty list as "nothing to disagree with" is what let a registry
        // whose every schema is undeclared freeze clean. The registration
        // conjunct is what keeps a registry holding nothing at all (no
        // spiice, no schema, no tool) from being accused of losing core.
        let registers_nothing =
            self.contracts.is_empty() && self.schemas.is_empty() && self.mcp_tools.is_empty();
        if !has_core && !registers_nothing {
            return Err(SpiiceRegistryError::MissingCoreContract);
        }
        self.validate_registrations_are_claimed()?;
        self.validate_export_inverse()?;
        Ok(())
    }

    /// What an erase destroys, the export carries (#478).
    ///
    /// Registry-wide, because a spiice's surface may derive from a core
    /// table. A surface whose erase leg runs anything (keyed, owned,
    /// bespoke, cascade, host lifecycle) must be exported as rows, as an
    /// allowlist, or `DerivedFrom` a surface whose chain of sources ends at
    /// one the bundle carries. Only a declared non-erase may be `Excluded`.
    /// Host-lifecycle surfaces are in scope deliberately: the rule is about
    /// what the owner loses, not about who runs the statement.
    pub(super) fn validate_export_inverse(&self) -> Result<(), SpiiceRegistryError> {
        use crate::spiice::contract::{EraseLeg, ExportRule};

        let rules: std::collections::HashMap<&'static str, ExportRule> = self
            .contracts
            .iter()
            .flat_map(|contract| contract.all_surfaces())
            .map(|surface| (surface.table, surface.export))
            .collect();
        for contract in &self.contracts {
            for surface in contract.all_surfaces() {
                match surface.export {
                    ExportRule::Rows | ExportRule::Allowlist(_) => {}
                    ExportRule::Excluded { .. } => {
                        if !matches!(
                            EraseLeg::derive(&surface, contract.bespoke_erase_legs),
                            EraseLeg::Never { .. }
                        ) {
                            return Err(SpiiceRegistryError::ErasedSurfaceNotExported {
                                spiice_id: contract.spiice_id,
                                table: surface.table,
                            });
                        }
                    }
                    ExportRule::DerivedFrom { source, .. } => {
                        let mut seen = std::collections::HashSet::from([surface.table]);
                        let mut current = source;
                        loop {
                            let Some(rule) = rules.get(current) else {
                                return Err(SpiiceRegistryError::DerivedFromUnknownSource {
                                    spiice_id: contract.spiice_id,
                                    table: surface.table,
                                    source_table: current,
                                });
                            };
                            match *rule {
                                ExportRule::Rows | ExportRule::Allowlist(_) => break,
                                ExportRule::DerivedFrom { source: next, .. }
                                    if seen.insert(current) =>
                                {
                                    current = next;
                                }
                                ExportRule::Excluded { .. } | ExportRule::DerivedFrom { .. } => {
                                    return Err(SpiiceRegistryError::DerivedFromUnexported {
                                        spiice_id: contract.spiice_id,
                                        table: surface.table,
                                        source_table: source,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Every LINKED spiice declares a contract.
    ///
    /// [`Self::validate_contract_schemas`] sweeps both directions inside a
    /// spiice that HAS a contract — declarations against registrations, and
    /// registrations carrying its `{spiice_id}/` prefix against its
    /// declarations. Neither loop runs for a spiice that registered and
    /// declared nothing, and this is the check that reaches that one.
    ///
    /// The [`SpiiceDescriptor`] is what is swept, because it is the spiice
    /// SAYING it is in this binary: `proxiima_spiice!` emits it before every
    /// schema and tool it registers, and that macro's `contract =` is
    /// optional. A spiice that omits it registers a corpus that goes
    /// missing from every registry walk (erase, export, forget, transfer)
    /// and whose Memory writes are refused later by a `spiice_surface`
    /// constraint naming none of the cause. With the descriptor held to a
    /// contract, `SchemaWithoutContract` then covers each of that spiice's
    /// schemas in turn, so the pair is complete for every spiice composed
    /// the sanctioned way (docs/build-a-spiice.md).
    ///
    /// [`SpiiceDescriptor`]: crate::spiice::SpiiceDescriptor
    fn validate_registrations_are_claimed(&self) -> Result<(), SpiiceRegistryError> {
        for spiice in &self.spiices {
            if self
                .contracts
                .iter()
                .any(|contract| contract.spiice_id == spiice.spiice_id)
            {
                continue;
            }
            return Err(SpiiceRegistryError::UnclaimedRegistration {
                spiice_id: spiice.spiice_id.clone(),
            });
        }
        Ok(())
    }

    /// Only the core contract may name hand-written legs or declare a
    /// dedupe transfer (#478).
    ///
    /// A bespoke leg is code in `proxiima-storage-pg`, and that crate depends
    /// on no spiice but core. A non-core spiice naming one names a statement
    /// nobody wrote: the erase or transfer would classify the surface as
    /// covered and report success over rows nothing touched.
    /// `FollowOrDedupe` is the same promise under another name, because its
    /// orchestrator is hand-written too.
    fn validate_core_only_legs(
        contract: &crate::spiice::contract::SpiiceContract,
    ) -> Result<(), SpiiceRegistryError> {
        use crate::spiice::contract::TransferRule;

        if contract.is_core() {
            return Ok(());
        }
        // The dedupe first: it is the more specific answer for a surface
        // that also sits in the bespoke transfer list.
        if let Some(surface) = contract
            .all_surfaces()
            .find(|surface| matches!(surface.transfer, TransferRule::FollowOrDedupe { .. }))
        {
            return Err(SpiiceRegistryError::DedupeNotPermitted {
                spiice_id: contract.spiice_id,
                table: surface.table,
            });
        }
        if !contract.bespoke_erase_legs.is_empty() {
            return Err(SpiiceRegistryError::BespokeLegsNotPermitted {
                spiice_id: contract.spiice_id,
                verb: "erase",
            });
        }
        if !contract.bespoke_transfer_legs.is_empty() {
            return Err(SpiiceRegistryError::BespokeLegsNotPermitted {
                spiice_id: contract.spiice_id,
                verb: "transfer",
            });
        }
        Ok(())
    }

    /// Every surface the spiice says is exportable must be REACHABLE from
    /// the owner, and the check is here because the answer never depends on
    /// the request.
    ///
    /// The generic owner-export generator has exactly two shapes — filter
    /// the surface's own declared `owner_column`, or join the home table of
    /// its key and filter there. A generic surface that declares `Rows` or
    /// `Allowlist` while carrying neither is a bundle leg nothing can emit.
    /// An explicit `HostState` surface is different: its registered,
    /// transaction-bound lifecycle callback owns the export query. Storage
    /// validates that callback's exact participant and managed table set at
    /// boot and validates every returned table/row at export time. It must
    /// still declare its `owner_column`, because the erase of an owner
    /// fences that owner's rows through it.
    ///
    /// It is deliberately not a check on ERASE. An unreachable surface that
    /// declares `Excluded` is a stated non-export; one that cascades is
    /// deleted by a constraint whether or not anything can name its owner.
    fn validate_contract_surfaces(
        contract: &crate::spiice::contract::SpiiceContract,
    ) -> Result<(), SpiiceRegistryError> {
        use crate::spiice::contract::EraseRule;

        for surface in contract.all_surfaces() {
            if matches!(surface.erase, EraseRule::HostState { .. }) {
                // The column that names the owner of each row is how an erase
                // of one owner fences that owner's rows and no other's.
                if surface.owner_column.is_none() {
                    return Err(SpiiceRegistryError::HostStateWithoutOwnerColumn {
                        spiice_id: contract.spiice_id,
                        table: surface.table,
                    });
                }
                continue;
            }
            if !surface.export.carries_rows() {
                continue;
            }
            if surface.owner_column.is_none() && surface.key.home().is_none() {
                return Err(SpiiceRegistryError::UnreachableExportSurface {
                    spiice_id: contract.spiice_id,
                    table: surface.table,
                });
            }
        }
        Ok(())
    }

    /// Where a contract declaration and the registration it describes are
    /// checked for being ONE fact rather than two copies.
    ///
    /// The field here is declared twice — once on the contract, once on
    /// the registration — and nothing but this check keeps the two equal.
    fn validate_registration_agreement(
        contract: &crate::spiice::contract::SpiiceContract,
        schema: &crate::spiice::contract::SchemaContract,
        info: &crate::verbs::schema::SchemaInfo,
    ) -> Result<(), SpiiceRegistryError> {
        let schema_id = schema.schema_id();
        // `natural_key_columns` is declared twice — on the contract and on
        // the payload trait — and the ingest reads the trait's copy, so
        // without this the contract's copy is a description of the ingest
        // that can stop being true with no test noticing.
        if schema.natural_key_columns.len() != info.natural_key_columns.len()
            || !schema
                .natural_key_columns
                .iter()
                .zip(&info.natural_key_columns)
                .all(|(declared, registered)| *declared == registered.as_str())
        {
            return Err(SpiiceRegistryError::NaturalKeyDisagreement {
                spiice_id: contract.spiice_id,
                schema_id,
            });
        }
        Ok(())
    }

    /// A tool's contract entry against the descriptor the registry holds.
    ///
    /// `actions` is a second description of the dispatcher's
    /// `action_arg_specs`, and nothing but this check keeps the two equal.
    /// The contract states no behaviour: that is the tool's `ToolEffect`,
    /// declared once on the tool.
    fn validate_contract_tools(
        &self,
        contract: &crate::spiice::contract::SpiiceContract,
    ) -> Result<(), SpiiceRegistryError> {
        for tool in contract.tools {
            let Some(entry) = self
                .mcp_tools
                .iter()
                .find(|entry| entry.name == tool.wire_name)
            else {
                return Err(SpiiceRegistryError::ContractToolNotRegistered {
                    spiice_id: contract.spiice_id,
                    name: tool.wire_name,
                });
            };
            // `actions` is a SECOND description of a fact the registry
            // already holds. The dispatcher's truth is
            // `McpToolDescriptor::action_arg_specs`, validated against the
            // JSON schema and never against this list.
            //
            // The list is compared in ORDER, not as a set. A palette scope
            // key is `"<wire_name>:<action>"`, so the declaration is read by
            // people composing palettes; a list that agrees on membership
            // and disagrees on order is still a list that has stopped being
            // a copy of the thing it describes.
            let registered_actions = entry
                .action_arg_specs
                .iter()
                .map(|spec| spec.action)
                .collect::<Vec<_>>();
            if registered_actions != tool.actions {
                return Err(SpiiceRegistryError::ToolActionsDisagreement {
                    spiice_id: contract.spiice_id,
                    name: tool.wire_name,
                });
            }
        }
        Ok(())
    }

    fn validate_contract_schemas(
        &self,
        contract: &crate::spiice::contract::SpiiceContract,
    ) -> Result<(), SpiiceRegistryError> {
        let prefix = format!("{}/", contract.spiice_id);
        for schema in contract.schemas {
            let schema_id = schema.schema_id();
            if !schema_id.as_str().starts_with(&prefix) {
                return Err(SpiiceRegistryError::ContractSchemaPrefix {
                    spiice_id: contract.spiice_id,
                    schema_id,
                });
            }
            validate_tag_sidecar(contract, schema)?;
            // A NotTransferable that names no enforcement site is a comment,
            // not a contract: the refusal has to survive a code path that
            // forgets to ask. Freeze checks that a site is named, not that
            // the site acts: the engine and the storage transfer refuse
            // every NotTransferable schema, so naming either is true.
            if let crate::spiice::contract::TransferRule::NotTransferable { enforced_by, .. } =
                schema.transfer
                && enforced_by.is_empty()
            {
                return Err(SpiiceRegistryError::UnenforcedTransferRefusal {
                    spiice_id: contract.spiice_id,
                    schema_id,
                });
            }
            // `PostgreSQL` forces four tsvector weight classes on the
            // storage; the declaration is free of that limit and states
            // relative floats. Where the two meet is here: more distinct
            // levels than classes has no honest bucketing, so it is a
            // freeze error naming the mechanism rather than a silent
            // collapse of two levels into one class.
            if let Err(levels) = schema.search.weight_levels() {
                return Err(SpiiceRegistryError::ProjectionWeightLevels {
                    spiice_id: contract.spiice_id,
                    schema_id,
                    levels,
                    classes: crate::spiice::contract::TSVECTOR_WEIGHT_CLASSES.len(),
                });
            }
            // `Units(&[])` is `Never` with the reason deleted.
            //
            // The whole point of the `Never { why }` arm is that "this does
            // not embed" carries its reason instead of being a naked
            // `false` — that is what the arm's own doc claims for it. An
            // empty unit list makes the identical claim and states nothing,
            // and it is worse than a naked `false` because it reads as the
            // embedding arm: `resolve()` yields zero units, so the drain
            // gets no text, while `is_never()` answers `false`, so the
            // agreement check below classifies it as "embeds" and the
            // enqueue lane files jobs the drain can only drop — the same
            // failure `EmbeddabilityDisagreement` refuses, reached through
            // an arm that check does not watch.
            //
            // Refused here rather than folded into that check, because it
            // is wrong on its own: a `Units` arm with nothing in it is not
            // a disagreement between two declarations, it is one
            // declaration that declines to say anything. Every kind, not
            // just Fact — an empty list is empty whoever wrote it.
            if let crate::spiice::contract::EmbeddingRecipe::Units(units) = schema.embedding
                && units.is_empty()
            {
                return Err(SpiiceRegistryError::EmptyEmbeddingUnits {
                    spiice_id: contract.spiice_id,
                    schema_id,
                });
            }
            // The shared-blob dedupe arm's blind spot, refused.
            //
            // A cross-owner transfer of a shared blob gives the destination
            // a NEW `blob` row and repoints the columns that reference it.
            // Those columns are enumerable exactly because they are foreign
            // keys. A cited-object sidecar references a
            // blob by convention — `cited_object_id` holds a `blob_id` with
            // nothing in the catalog saying so — and the remap would walk
            // straight past it, leaving the rows pointing at the source
            // owner's row after the citation moved.
            //
            // Every such schema is opaque (`sidecar_table: None`), so the
            // remap has nothing to miss. Declaring one is the moment the
            // remap needs designing, so that is the moment this refuses,
            // rather than the moment a transfer silently splits a citation
            // from its bytes.
            if schema.kind == crate::verbs::schema::PayloadKind::CitedObject
                && let Some(table) = schema.sidecar_table
            {
                return Err(SpiiceRegistryError::CitationSidecarNotRemappable {
                    spiice_id: contract.spiice_id,
                    schema_id,
                    table,
                });
            }
            // `PerRow { column }` names a column the generator does not
            // read: the generator emits one language column per projection
            // table and names it itself. Left unchecked, a spiice could
            // declare
            // `PerRow { column: "row_config" }`, see no error, and get rows
            // stamped and ranked under a column its contract never named.
            // Consuming the payload as a constraint is the smallest honest
            // reading of it — the generator emits one column, so declaring
            // a different one is a need the shape cannot express, and the
            // rule is that such a need becomes a vocabulary extension
            // rather than a silent divergence.
            if let Some(declared) = schema.search.per_row_language_column() {
                let projection_column = contract
                    .projection
                    .spec()
                    .and_then(|spec| spec.surface().lexical_language_column);
                if projection_column != Some(declared) {
                    return Err(SpiiceRegistryError::ProjectionLanguageColumn {
                        spiice_id: contract.spiice_id,
                        schema_id,
                        declared,
                        projection_column,
                    });
                }
            }
            let registered = self.schemas.iter().find(|info| {
                info.schema_id == schema_id
                    && info.schema_version == schema.schema_version()
                    && info.kind == schema.kind
            });
            let Some(info) = registered else {
                return Err(SpiiceRegistryError::ContractSchemaNotRegistered {
                    spiice_id: contract.spiice_id,
                    schema_id,
                    schema_version: schema.schema_version(),
                    kind: schema.kind,
                });
            };
            Self::validate_registration_agreement(contract, schema, info)?;
        }
        for info in &self.schemas {
            if !info.schema_id.as_str().starts_with(&prefix) {
                continue;
            }
            let declared = contract.schemas.iter().any(|schema| {
                schema.schema_id() == info.schema_id
                    && schema.schema_version() == info.schema_version
                    && schema.kind == info.kind
            });
            if !declared {
                return Err(SpiiceRegistryError::SchemaWithoutContract {
                    spiice_id: contract.spiice_id,
                    schema_id: info.schema_id.clone(),
                    schema_version: info.schema_version,
                    kind: info.kind,
                });
            }
        }
        self.validate_contract_tools(contract)?;
        Ok(())
    }
}
