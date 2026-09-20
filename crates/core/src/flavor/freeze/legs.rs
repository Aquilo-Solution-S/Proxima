//! Leg coverage: for each lifecycle verb, that every surface a flavor
//! declares is reached by something.
//!
//! The three checks share one shape. A surface the generic loop skips and
//! no bespoke leg claims is not a loud failure — erase reports `Completed`,
//! transfer reports success, forget leaves the row — so what an uncovered
//! surface produces is rows that outlive the operation that was supposed to
//! reach them. Refusing the freeze is the only point at which that is still
//! visible.

use super::{FlavorRegistry, FlavorRegistryError};

impl FlavorRegistry {
    /// Every surface must have a leg that destroys it, and the check is at
    /// boot because a missing one is discovered nowhere else.
    ///
    /// The erase partitions every declared surface into exactly one of five
    /// answers — a generated keyed leg, a generated owned leg, a named
    /// hand-written leg, a constraint, or a declared non-erase with a
    /// reason. There is no sixth answer; the sixth shape is silence —
    /// `ByKey` on a key the erase builds no selection set for falls through
    /// both generic loops, leaving only an exemption list between that and a
    /// table nothing ever deletes.
    ///
    /// The partition lives here rather than in `proxima-storage-pg` because
    /// that crate depends on no flavor and can only ever see flavor #0. An
    /// out-of-tree flavor declaring `ByKey` on a `Custom` key would freeze
    /// cleanly, boot cleanly, and report `Completed` over rows that outlived
    /// their owner. Under a model where the host owns every promise about
    /// erasure, a substrate that quietly keeps rows is the one failure it
    /// must not have, so the check sits where every flavor passes through,
    /// in-tree or not.
    ///
    /// [`FlavorContract::erase_leg`] is the classifier both this and the
    /// erase itself call, so this is a check on the code that runs rather
    /// than on a second description of it.
    pub(super) fn validate_erase_legs(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::{EraseLeg, EraseRule};

        for surface in contract.all_surfaces() {
            if contract.erase_leg(&surface) == EraseLeg::Unreachable {
                return Err(FlavorRegistryError::UndeletableSurface {
                    flavor_id: contract.flavor_id,
                    table: surface.table,
                });
            }
        }

        // A stale name is what rots an exemption list, and an exemption that
        // claims a `Cascade` or `Never` surface is a flavor arguing with
        // itself about whether a statement runs.
        for table in contract.bespoke_erase_legs {
            let why = match contract
                .all_surfaces()
                .find(|surface| surface.table == *table)
            {
                None => "this flavor does not declare",
                Some(surface) => match surface.erase {
                    EraseRule::ByKey | EraseRule::ByOwner => continue,
                    EraseRule::Cascade { .. } => {
                        "a constraint removes, so no hand-written statement should touch it"
                    }
                    EraseRule::HostState { .. } => {
                        "is handled by the registered host lifecycle callback, not a bespoke SQL leg"
                    }
                    EraseRule::Never { .. } => {
                        "is a declared non-erase, so no statement should touch it"
                    }
                },
            };
            return Err(FlavorRegistryError::BespokeEraseLegMismatch {
                flavor_id: contract.flavor_id,
                table,
                why,
            });
        }
        Ok(())
    }

    /// Every surface must have a leg that MOVES it, or a declaration
    /// saying it deliberately does not move, and the check is at boot for
    /// the same reason the erase's is: a missing one is discovered nowhere
    /// else.
    ///
    /// The transfer partitions every declared surface into exactly one of
    /// seven answers — a generated re-home, a generated drop, a generated
    /// dedupe, a named hand-written leg, a key-owned non-move, a deliberate
    /// retention at the source, or a refusal. There is no eighth answer; the
    /// eighth shape is silence — a table list of string literals naming no
    /// contract type would give a flavor adding a `Follow` surface no
    /// statement, no error, and no way to find out.
    ///
    /// That silence is worse here than on the erase side, which is what the
    /// partition is worth its lines for. An unerased row outlives its owner
    /// and is found by reconcile. An unmoved row is readable by the SOURCE
    /// owner after the memory became the destination's — a cross-tenant
    /// read under the multi-owner design centre, produced by nobody
    /// deciding anything.
    ///
    /// [`FlavorContract::transfer_leg`] is the classifier both this and the
    /// transfer itself call, so this is a check on the code that runs
    /// rather than on a second description of it.
    pub(super) fn validate_transfer_legs(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::TransferLeg;

        for surface in contract.all_surfaces() {
            if contract.transfer_leg(&surface) == TransferLeg::Unreachable {
                return Err(FlavorRegistryError::UnmovableSurface {
                    flavor_id: contract.flavor_id,
                    table: surface.table,
                });
            }
        }

        // A stale name is what rots an exemption list, and an exemption
        // claiming a surface whose rule says NO statement runs is a flavor
        // arguing with itself about whether one does.
        for table in contract.bespoke_transfer_legs {
            let why = match contract
                .all_surfaces()
                .find(|surface| surface.table == *table)
            {
                None => "this flavor does not declare",
                Some(surface) => {
                    if contract.transfer_leg(&surface).moves_rows() {
                        continue;
                    }
                    "declares a transfer that moves no rows, so no hand-written statement \
                     should touch it"
                }
            };
            return Err(FlavorRegistryError::BespokeTransferLegMismatch {
                flavor_id: contract.flavor_id,
                table,
                why,
            });
        }
        Ok(())
    }

    /// Every surface says what forget does to it, and every answer is one
    /// the forget can carry out.
    ///
    /// The arms this closes are `DeleteWithMemory` over a key shaped like
    /// nothing the forget builds a `t` for, or `DumpThenCascade` without both
    /// a `MemoryT` key and a completeness proof. Such a surface declared that
    /// forgetting a memory destroys (or preserves) rows, and no valid leg
    /// anywhere would have. Same shape as `UndeletableSurface` and
    /// `UnmovableSurface`, one verb over.
    pub(super) fn validate_forget_legs(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::ForgetLeg;

        for surface in contract.all_surfaces() {
            if ForgetLeg::derive(&surface) == ForgetLeg::Unreachable {
                return Err(FlavorRegistryError::UnforgettableSurface {
                    flavor_id: contract.flavor_id,
                    table: surface.table,
                });
            }
        }
        Ok(())
    }
}
