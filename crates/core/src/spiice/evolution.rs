//! Build-time declarations for explicit spiice schema evolution.

use std::collections::{HashMap, HashSet};

use super::{SpiiceContract, SpiiceRegistryError};
use crate::spiice::contract::{SchemaContract, SchemaRef};
use crate::verbs::schema::PayloadKind;

/// One complete, spiice-owned representation change.
///
/// Sources are historical migration inputs and need not be active registry
/// schemas. Targets are the schemas the composed binary serves after the
/// migration. Core validates the graph and metadata at freeze; execution is a
/// separate storage concern.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SchemaEvolution {
    pub id: &'static str,
    pub sources: &'static [SchemaContract],
    pub targets: &'static [SchemaContract],
    pub after_migration: i64,
    pub requires: &'static [&'static str],
    pub change: &'static str,
    pub consumer_action: &'static str,
}

impl SchemaEvolution {
    #[must_use]
    pub const fn new(
        id: &'static str,
        sources: &'static [SchemaContract],
        targets: &'static [SchemaContract],
        after_migration: i64,
        requires: &'static [&'static str],
        change: &'static str,
        consumer_action: &'static str,
    ) -> Self {
        Self {
            id,
            sources,
            targets,
            after_migration,
            requires,
            change,
            consumer_action,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Step<'a> {
    evolution: &'a SchemaEvolution,
}

#[expect(
    clippy::too_many_lines,
    reason = "the freeze pass keeps evolution invariants in one ordered audit"
)]
pub(super) fn validate_evolutions(
    contracts: &[&'static SpiiceContract],
) -> Result<(), SpiiceRegistryError> {
    let mut steps = HashMap::new();
    let mut consumed = HashSet::new();
    let mut produced = HashMap::new();
    for contract in contracts {
        for evolution in contract.evolutions {
            if evolution.id.is_empty()
                || evolution.change.trim().is_empty()
                || evolution.consumer_action.trim().is_empty()
                || evolution.sources.is_empty()
                || evolution.targets.is_empty()
            {
                return Err(SpiiceRegistryError::InvalidSchemaEvolution {
                    spiice_id: contract.spiice_id,
                    evolution_id: evolution.id,
                    reason: "id, source, target, change and consumer_action are required",
                });
            }
            if steps.insert(evolution.id, Step { evolution }).is_some() {
                return Err(SpiiceRegistryError::DuplicateSchemaEvolution {
                    evolution_id: evolution.id,
                });
            }
            validate_set(contract, evolution, evolution.sources)?;
            validate_set(contract, evolution, evolution.targets)?;
            let mut seen = HashSet::new();
            for source in evolution.sources {
                let key = schema_key(source);
                if !seen.insert(key) {
                    return Err(SpiiceRegistryError::SchemaEvolutionDuplicateSchema {
                        evolution_id: evolution.id,
                        schema_id: source.schema_id(),
                    });
                }
                if !consumed.insert(key) {
                    return Err(SpiiceRegistryError::SchemaEvolutionSourceReused {
                        evolution_id: evolution.id,
                        schema_id: source.schema_id(),
                    });
                }
            }
            for target in evolution.targets {
                let key = schema_key(target);
                if !seen.insert(key) {
                    return Err(SpiiceRegistryError::SchemaEvolutionDuplicateSchema {
                        evolution_id: evolution.id,
                        schema_id: target.schema_id(),
                    });
                }
                if produced.insert(key, evolution.id).is_some() {
                    return Err(SpiiceRegistryError::SchemaEvolutionTargetReused {
                        evolution_id: evolution.id,
                        schema_id: target.schema_id(),
                    });
                }
            }
            for required in evolution.requires {
                if !steps.contains_key(required)
                    && !contracts.iter().any(|candidate| {
                        candidate.evolutions.iter().any(|step| step.id == *required)
                    })
                {
                    return Err(SpiiceRegistryError::SchemaEvolutionMissingDependency {
                        evolution_id: evolution.id,
                        required,
                    });
                }
            }
        }
    }

    let active: HashSet<_> = contracts
        .iter()
        .flat_map(|contract| contract.schemas.iter().map(schema_key))
        .collect();
    let mut graph = HashMap::<&'static str, Vec<&'static str>>::new();
    for contract in contracts {
        for evolution in contract.evolutions {
            for required in evolution.requires {
                graph.entry(required).or_default().push(evolution.id);
            }
            for prior in contracts
                .iter()
                .flat_map(|candidate| candidate.evolutions.iter())
            {
                if prior.id != evolution.id
                    && prior.targets.iter().any(|target| {
                        evolution
                            .sources
                            .iter()
                            .any(|source| schema_key(source) == schema_key(target))
                    })
                {
                    graph.entry(prior.id).or_default().push(evolution.id);
                }
            }
        }
    }
    for id in steps.keys().copied() {
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        visit(id, &graph, &mut visiting, &mut visited)?;
    }

    for contract in contracts {
        for schema in contract.schemas {
            if schema.id.version <= 1 {
                continue;
            }
            if !has_initial_path(schema_key(schema), &produced, &steps, &mut HashSet::new()) {
                return Err(SpiiceRegistryError::SchemaEvolutionMissingPath {
                    spiice_id: contract.spiice_id,
                    schema_id: schema.schema_id(),
                });
            }
        }
    }
    for (key, producer) in &produced {
        let terminal = !steps.values().any(|step| {
            step.evolution
                .sources
                .iter()
                .any(|source| schema_key(source) == *key)
        });
        if terminal && !active.contains(key) {
            return Err(SpiiceRegistryError::SchemaEvolutionTargetNotRegistered {
                evolution_id: producer,
                schema_id: key.1.schema_id(),
            });
        }
    }
    Ok(())
}

fn validate_set(
    contract: &SpiiceContract,
    evolution: &SchemaEvolution,
    schemas: &[SchemaContract],
) -> Result<(), SpiiceRegistryError> {
    for schema in schemas {
        if schema.id.spiice != contract.spiice_id {
            return Err(SpiiceRegistryError::SchemaEvolutionForeignSchema {
                spiice_id: contract.spiice_id,
                evolution_id: evolution.id,
                schema_id: schema.schema_id(),
            });
        }
        if schema.id.version == 0 {
            return Err(SpiiceRegistryError::InvalidSchemaEvolution {
                spiice_id: contract.spiice_id,
                evolution_id: evolution.id,
                reason: "schema versions must be positive",
            });
        }
    }
    Ok(())
}

type SchemaKey = (PayloadKind, SchemaRef);

fn schema_key(schema: &SchemaContract) -> SchemaKey {
    (schema.kind, schema.id)
}

fn visit(
    id: &'static str,
    graph: &HashMap<&'static str, Vec<&'static str>>,
    visiting: &mut HashSet<&'static str>,
    visited: &mut HashSet<&'static str>,
) -> Result<(), SpiiceRegistryError> {
    if visited.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(SpiiceRegistryError::SchemaEvolutionCycle { evolution_id: id });
    }
    if let Some(children) = graph.get(id) {
        for child in children {
            visit(child, graph, visiting, visited)?;
        }
    }
    visiting.remove(id);
    visited.insert(id);
    Ok(())
}

fn has_initial_path(
    key: SchemaKey,
    producers: &HashMap<SchemaKey, &'static str>,
    steps: &HashMap<&'static str, Step<'_>>,
    seen: &mut HashSet<&'static str>,
) -> bool {
    if key.1.version == 1 {
        return true;
    }
    let Some(id) = producers.get(&key) else {
        return false;
    };
    if !seen.insert(id) {
        // `seen` is the current recursion stack, not a global visit mark.
        // A split may reach the same producer through two sibling sources;
        // that is valid after the first branch removes it, while a producer
        // still on the stack is a real cycle.
        return false;
    }
    let step = steps.get(id).expect("producer indexed with its step");
    let reaches_initial = step
        .evolution
        .sources
        .iter()
        .all(|source| has_initial_path(schema_key(source), producers, steps, seen));
    seen.remove(id);
    reaches_initial
}

#[must_use]
pub(crate) fn evolution_for_schema(
    evolutions: &[&'static SchemaEvolution],
    schema_id: &crate::SchemaId,
    schema_version: crate::SchemaVersion,
    kind: PayloadKind,
) -> Option<(&'static str, &'static str)> {
    evolutions.iter().find_map(|evolution| {
        evolution
            .targets
            .iter()
            .any(|target| {
                target.schema_id() == *schema_id
                    && target.schema_version() == schema_version
                    && target.kind == kind
            })
            .then_some((evolution.change, evolution.consumer_action))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spiice::contract::{
        EmbeddingRecipe, ProjectionDecl, Provenance, SearchProjectionDecl, TransferRule,
    };

    const fn schema(kind: PayloadKind, name: &'static str, version: u32) -> SchemaContract {
        SchemaContract {
            id: SchemaRef::new("test-evolution", name, version),
            kind,
            sidecar_table: None,
            search: SearchProjectionDecl::None {
                why: "test",
                tag_column: None,
            },
            embedding: EmbeddingRecipe::Never { why: "test" },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[],
            natural_key_columns: crate::NaturalKeyDeclaration::EMPTY,
        }
    }

    const FACT_V1: SchemaContract = schema(PayloadKind::Fact, "record", 1);
    const FACT_V2: SchemaContract = schema(PayloadKind::Fact, "record", 2);
    const FACT_V3: SchemaContract = schema(PayloadKind::Fact, "record", 3);
    const ABS_V1: SchemaContract = schema(PayloadKind::Abstraction, "record", 1);
    const ABS_V2: SchemaContract = schema(PayloadKind::Abstraction, "record", 2);

    const fn step(
        id: &'static str,
        sources: &'static [SchemaContract],
        targets: &'static [SchemaContract],
        requires: &'static [&'static str],
    ) -> SchemaEvolution {
        SchemaEvolution::new(id, sources, targets, 1, requires, "changed", "upgrade")
    }

    #[test]
    fn reversed_multi_step_declarations_follow_implicit_producer_edges() {
        static FIRST: SchemaEvolution = step("first", &[FACT_V1], &[FACT_V2], &[]);
        static SECOND: SchemaEvolution = step("second", &[FACT_V2], &[FACT_V3], &[]);
        static CONTRACT: SpiiceContract = SpiiceContract {
            spiice_id: "test-evolution",
            ordinal: 7,
            schemas: &[FACT_V3],
            evolutions: &[SECOND, FIRST],
            retired_facts: &[],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: ProjectionDecl::None { why: "test" },
        };
        validate_evolutions(&[&CONTRACT]).expect("implicit chain reaches v1");
    }

    #[test]
    fn kind_sharing_has_independent_paths() {
        static FACT: SchemaEvolution = step("fact", &[FACT_V1], &[FACT_V2], &[]);
        static ABS: SchemaEvolution = step("abs", &[ABS_V1], &[ABS_V2], &[]);
        static CONTRACT: SpiiceContract = SpiiceContract {
            spiice_id: "test-evolution",
            ordinal: 7,
            schemas: &[FACT_V2, ABS_V2],
            evolutions: &[FACT, ABS],
            retired_facts: &[],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: ProjectionDecl::None { why: "test" },
        };
        validate_evolutions(&[&CONTRACT]).expect("F/A paths do not collide");
    }

    #[test]
    fn split_then_merge_reuses_the_split_producer() {
        static SPLIT: SchemaEvolution = step("split", &[FACT_V1], &[FACT_V2, ABS_V2], &[]);
        static MERGE: SchemaEvolution = step("merge", &[FACT_V2, ABS_V2], &[FACT_V3], &[]);
        static CONTRACT: SpiiceContract = SpiiceContract {
            spiice_id: "test-evolution",
            ordinal: 7,
            schemas: &[FACT_V3],
            evolutions: &[MERGE, SPLIT],
            retired_facts: &[],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: ProjectionDecl::None { why: "test" },
        };
        validate_evolutions(&[&CONTRACT]).expect("split-then-merge reaches v1");
    }

    #[test]
    fn a_source_gap_is_not_repaired_by_an_unrelated_requires_edge() {
        static UNRELATED: SchemaEvolution = step("unrelated", &[FACT_V1], &[FACT_V2], &[]);
        static GAP: SchemaEvolution = step("gap", &[FACT_V3], &[ABS_V2], &["unrelated"]);
        static CONTRACT: SpiiceContract = SpiiceContract {
            spiice_id: "test-evolution",
            ordinal: 7,
            schemas: &[ABS_V2],
            evolutions: &[UNRELATED, GAP],
            retired_facts: &[],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: ProjectionDecl::None { why: "test" },
        };
        assert!(matches!(
            validate_evolutions(&[&CONTRACT]),
            Err(SpiiceRegistryError::SchemaEvolutionMissingPath { .. })
        ));
    }
}
