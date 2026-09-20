//! The contract cross-checks: every flavor's declared [`FlavorContract`]
//! against what that flavor actually registered.
//!
//! The partition exists because a contract is a claim and a registration is
//! the fact — the two are written in different places by different hands,
//! and nothing but these sweeps keeps them equal. The leg coverage
//! ([`super::legs`]) and the projection statement ([`super::projection`])
//! are cross-checks too, kept in files of their own because each answers a
//! question about one verb rather than about the declaration as a whole.
//!
//! [`FlavorContract`]: crate::flavor::FlavorContract

use super::{FlavorRegistry, FlavorRegistryError};

impl FlavorRegistry {
    /// Cross-check the declarations against the registrations.
    ///
    /// This is the check that makes "everything is a flavor" structural
    /// rather than aspirational: a schema registered without a contract
    /// entry, or a contract entry with no registration, fails the build of
    /// the composed binary rather than going missing from an erase sweep.
    ///
    /// It takes two checks to close the first of those, because a schema
    /// carries its flavor in a PREFIX and a prefix nothing declares names
    /// nothing to accuse. `SchemaWithoutContract` sweeps the registrations
    /// under a declared flavor's prefix; `UnclaimedRegistration` holds the
    /// flavor itself — the `FlavorDescriptor` every composed flavor
    /// registers — to declaring one. A registration made under neither a
    /// descriptor nor a contract is a registry no binary composes.
    pub(super) fn validate_contracts(&self) -> Result<(), FlavorRegistryError> {
        let mut seen_ordinals = std::collections::HashSet::new();
        let mut has_core = false;
        for contract in &self.contracts {
            if !seen_ordinals.insert(contract.ordinal) {
                return Err(FlavorRegistryError::DuplicateFlavorOrdinal {
                    ordinal: contract.ordinal,
                    flavor_id: contract.flavor_id,
                });
            }
            if contract.is_core() {
                has_core = true;
            } else if !contract.resources.is_empty() {
                // Resources are flavor #0's. A flavor resource would need
                // its own scope-key namespace, a URI-template parser for its
                // parameters and a pagination contract — a feature with its
                // own design, not a forwarding line.
                return Err(FlavorRegistryError::ResourcesNotPermitted {
                    flavor_id: contract.flavor_id,
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
            // declaration, and a flavor whose schemas cannot agree on what
            // one statement spells is broken whether or not a query would
            // ever have reached that statement.
            Self::validate_projection_is_reachable(contract)?;
            Self::validate_contract_surfaces(contract)?;
            Self::validate_erase_legs(contract)?;
            Self::validate_transfer_legs(contract)?;
            Self::validate_forget_legs(contract)?;
        }
        // An empty contracts list is the same defect at full width, not an
        // exemption from it. Core is non-removable, so any registry that
        // registered ANYTHING has a flavor #0 to declare — and reading an
        // empty list as "nothing to disagree with" is what let a registry
        // whose every schema is undeclared freeze clean. The registration
        // conjunct is what keeps a registry holding nothing at all (no
        // flavor, no schema, no tool) from being accused of losing core.
        let registers_nothing =
            self.contracts.is_empty() && self.schemas.is_empty() && self.mcp_tools.is_empty();
        if !has_core && !registers_nothing {
            return Err(FlavorRegistryError::MissingCoreContract);
        }
        self.validate_registrations_are_claimed()?;
        Ok(())
    }

    /// Every LINKED flavor declares a contract.
    ///
    /// [`Self::validate_contract_schemas`] sweeps both directions inside a
    /// flavor that HAS a contract — declarations against registrations, and
    /// registrations carrying its `{flavor_id}/` prefix against its
    /// declarations. Neither loop runs for a flavor that registered and
    /// declared nothing, and this is the check that reaches that one.
    ///
    /// The [`FlavorDescriptor`] is what is swept, because it is the flavor
    /// SAYING it is in this binary: `proxima_flavor!` emits it before every
    /// schema and tool it registers, and that macro's `contract =` is
    /// optional. A flavor that omits it registers a corpus that goes
    /// missing from every registry walk (erase, export, forget, transfer)
    /// and whose Memory writes are refused later by a `flavor_surface`
    /// constraint naming none of the cause. With the descriptor held to a
    /// contract, `SchemaWithoutContract` then covers each of that flavor's
    /// schemas in turn, so the pair is complete for every flavor composed
    /// the sanctioned way (docs/09 §Registration).
    ///
    /// [`FlavorDescriptor`]: crate::flavor::FlavorDescriptor
    fn validate_registrations_are_claimed(&self) -> Result<(), FlavorRegistryError> {
        for flavor in &self.flavors {
            if self
                .contracts
                .iter()
                .any(|contract| contract.flavor_id == flavor.flavor_id)
            {
                continue;
            }
            return Err(FlavorRegistryError::UnclaimedRegistration {
                flavor_id: flavor.flavor_id.clone(),
            });
        }
        Ok(())
    }

    /// Every surface the flavor says is exportable must be REACHABLE from
    /// the owner, and the check is here because the answer never depends on
    /// the request.
    ///
    /// The generic owner-export generator has exactly two shapes — filter
    /// the surface's own declared `owner_column`, or join the home table of
    /// its key and filter there. A generic surface that declares `Rows` or
    /// `Allowlist` while carrying neither is a bundle leg nothing can emit.
    /// An explicit `HostState` surface is different: its registered,
    /// transaction-bound lifecycle callback owns the export query and can
    /// join through its own authoritative owner relation. Storage validates
    /// that callback's exact participant and managed table set at boot and
    /// validates every returned table/row at export time.
    ///
    /// It is deliberately not a check on ERASE. An unreachable surface that
    /// declares `Excluded` is a stated non-export; one that cascades is
    /// deleted by a constraint whether or not anything can name its owner.
    fn validate_contract_surfaces(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::{EraseRule, ExportRule};

        for surface in contract.all_surfaces() {
            if matches!(surface.export, ExportRule::Excluded { .. }) {
                continue;
            }
            if matches!(surface.erase, EraseRule::HostState { .. }) {
                continue;
            }
            if surface.owner_column.is_none() && surface.key.home().is_none() {
                return Err(FlavorRegistryError::UnreachableExportSurface {
                    flavor_id: contract.flavor_id,
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
        contract: &crate::flavor::contract::FlavorContract,
        schema: &crate::flavor::contract::SchemaContract,
        info: &crate::verbs::schema::SchemaInfo,
    ) -> Result<(), FlavorRegistryError> {
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
            return Err(FlavorRegistryError::NaturalKeyDisagreement {
                flavor_id: contract.flavor_id,
                schema_id,
            });
        }
        Ok(())
    }

    /// A tool's contract entry against the descriptor the registry holds.
    ///
    /// `actions` and `idempotent` are both second descriptions of facts the
    /// registry already carries — the dispatcher's `action_arg_specs` and
    /// the wire's `McpToolAnnotations` — and nothing but this check keeps
    /// either equal.
    fn validate_contract_tools(
        &self,
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        for tool in contract.tools {
            let Some(entry) = self
                .mcp_tools
                .iter()
                .find(|entry| entry.name == tool.wire_name)
            else {
                return Err(FlavorRegistryError::ContractToolNotRegistered {
                    flavor_id: contract.flavor_id,
                    name: tool.wire_name,
                });
            };
            // `actions` and `idempotent` are a SECOND description of facts
            // the registry already holds, and nothing but this check keeps
            // the two equal. The dispatcher's truth is
            // `McpToolDescriptor::action_arg_specs`, validated against the
            // JSON schema and never against this list; the wire's truth is
            // `McpToolAnnotations::idempotent`.
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
                return Err(FlavorRegistryError::ToolActionsDisagreement {
                    flavor_id: contract.flavor_id,
                    name: tool.wire_name,
                });
            }
            let annotations = entry.resolved_annotations();
            let resolved = annotations
                .and_then(|value| value.idempotent)
                // A read-only tool is idempotent by construction — calling
                // it twice is calling it once — and MCP's `readOnlyHint`
                // carries that, which is why the substrate annotations do
                // not restate it. Reading the implication here is what lets
                // the contract's `idempotent` stay a claim about BEHAVIOUR
                // rather than a copy of one optional field.
                .or_else(|| {
                    annotations
                        .and_then(|value| value.read_only)
                        .filter(|ro| *ro)
                })
                .unwrap_or(false);
            if resolved != tool.idempotent {
                return Err(FlavorRegistryError::ToolIdempotenceDisagreement {
                    flavor_id: contract.flavor_id,
                    name: tool.wire_name,
                    declared: tool.idempotent,
                    resolved,
                });
            }
        }
        Ok(())
    }

    fn validate_contract_schemas(
        &self,
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        let prefix = format!("{}/", contract.flavor_id);
        for schema in contract.schemas {
            let schema_id = schema.schema_id();
            if !schema_id.as_str().starts_with(&prefix) {
                return Err(FlavorRegistryError::ContractSchemaPrefix {
                    flavor_id: contract.flavor_id,
                    schema_id,
                });
            }
            // A NotTransferable that names no enforcement site is a comment,
            // not a contract: the refusal has to survive a code path that
            // forgets to ask.
            if let crate::flavor::contract::TransferRule::NotTransferable { enforced_by, .. } =
                schema.transfer
                && enforced_by.is_empty()
            {
                return Err(FlavorRegistryError::UnenforcedTransferRefusal {
                    flavor_id: contract.flavor_id,
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
                return Err(FlavorRegistryError::ProjectionWeightLevels {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    levels,
                    classes: crate::flavor::contract::TSVECTOR_WEIGHT_CLASSES.len(),
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
            if let crate::flavor::contract::EmbeddingRecipe::Units(units) = schema.embedding
                && units.is_empty()
            {
                return Err(FlavorRegistryError::EmptyEmbeddingUnits {
                    flavor_id: contract.flavor_id,
                    schema_id,
                });
            }
            // The shared-blob dedupe arm's blind spot, refused.
            //
            // A cross-owner transfer of a shared blob gives the destination
            // a NEW `blob` row and repoints the columns that reference it.
            // Those columns are enumerable exactly because they are foreign
            // keys. A cited-object or citation-mapping sidecar references a
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
            if matches!(
                schema.kind,
                crate::verbs::schema::PayloadKind::CitedObject
                    | crate::verbs::schema::PayloadKind::CitationMapping
            ) && let Some(table) = schema.sidecar_table
            {
                return Err(FlavorRegistryError::CitationSidecarNotRemappable {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    table,
                });
            }
            // `PerRow { column }` names a column the generator does not
            // read: the generator emits one language column per projection
            // table and names it itself. Left unchecked, a flavor could
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
                    return Err(FlavorRegistryError::ProjectionLanguageColumn {
                        flavor_id: contract.flavor_id,
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
                return Err(FlavorRegistryError::ContractSchemaNotRegistered {
                    flavor_id: contract.flavor_id,
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
                return Err(FlavorRegistryError::SchemaWithoutContract {
                    flavor_id: contract.flavor_id,
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
