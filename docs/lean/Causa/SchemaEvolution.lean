/-
Causa — schema evolution

A spiice supplies the relation between old and new typed records. Relations,
rather than functions, permit one admission to split into several admissions
and several admissions to merge into one. The kernel only completes the
shared retained-store obligations; conversion meaning stays with the spiice.
-/

import Causa.Provenance

namespace Causa

structure SchemaMigrationWitness where
  oldMemories : Set Memory
  newMemories : Set Memory
  oldGoals : Set Goal
  newGoals : Set Goal
  oldHeads : Set MemoryHead
  newHeads : Set MemoryHead
  /-- Existing erase witnesses are carried through, never created by a schema
      migration. Erasure remains the separate authorized inverse operation. -/
  retainedTargets : Set ErasedPinTarget
  memoryMap : Memory → Memory → Prop
  goalMap : Goal → Goal → Prop
  memoryPinMap : MemoryId → MemoryId → Prop
  goalPinMap : GoalId → GoalId → Prop
  /-- Every old live row is explicitly mapped to at least one new live row.
      The relation permits split, merge, and replacement IDs; it cannot use a
      retained erase witness as a substitute for migration coverage. -/
  sourceMemoryCoverage : ∀ old, old ∈ oldMemories →
    ∃ new, new ∈ newMemories ∧ memoryMap old new
  sourceGoalCoverage : ∀ old, old ∈ oldGoals →
    ∃ new, new ∈ newGoals ∧ goalMap old new
  targetMemoryDeclaration : ∀ new, new ∈ newMemories →
    ∃ old, old ∈ oldMemories ∧ memoryMap old new
  targetGoalDeclaration : ∀ new, new ∈ newGoals →
    ∃ old, old ∈ oldGoals ∧ goalMap old new
  mappedMemoryOwner : ∀ old new, memoryMap old new →
    memory_owner old = memory_owner new
  mappedGoalOwner : ∀ old new, goalMap old new →
    goal_owner old = goal_owner new
  mappedMemoryPins : ∀ old new, memoryMap old new → ∀ id,
    (id ∈ memory_origins old ∨ id ∈ memory_refs old ∨
      memory_forked_from old = some id) →
      (∃ oldTarget newTarget,
        oldTarget ∈ oldMemories ∧ newTarget ∈ newMemories ∧
        memory_t oldTarget = id ∧ memoryPinMap id (memory_t newTarget) ∧
        pinFrom new newTarget) ∨
      (∃ e, e ∈ retainedTargets ∧ erased_pin_target_t e = id)
  mappedGoalPins : ∀ old new, goalMap old new → ∀ id,
    id ∈ goalDeclaredTargetIds old →
      (∃ newId, goalPinMap id newId ∧ newId ∈ goalDeclaredTargetIds new) ∨
      (∃ e, e ∈ retainedTargets ∧ erased_pin_target_t e = id)
  newMemoryIdUnique : MemoryIdUnique newMemories
  newGoalIdUnique : GoalIdUnique newGoals
  retainedTargetIdUnique : ErasedPinTargetIdUnique retainedTargets
  newHeadAligned : MemoryHeadAligned newMemories newHeads
  newForkAnchorAligned : ForkAnchorAligned newMemories
  retainedPinTargetsExist : ∀ m, m ∈ newMemories →
    (∀ id, id ∈ memory_origins m →
      retainedOriginTargetExists newMemories retainedTargets id) ∧
    (∀ id, id ∈ memory_refs m →
      retainedMemoryReferenceTargetExists newMemories retainedTargets id) ∧
    (∀ id, memory_forked_from m = some id →
      retainedMemoryReferenceTargetExists newMemories retainedTargets id) ∧
    (∀ id, id ∈ memory_goal_refs m →
      retainedGoalReferenceTargetExists newGoals retainedTargets id)
  retainedMotivationsValid : ∀ g, g ∈ newGoals →
    goal_motivations_retained_valid newMemories newGoals retainedTargets g
  retainedGuidanceValid : ∀ m, m ∈ newMemories →
    perspective_guidance_retained_valid newMemories newGoals retainedTargets m
  retainedOriginKind : ∀ m, m ∈ newMemories →
    RetainedOriginKindValid newMemories retainedTargets m
  memoryGoalIdsDisjoint : ∀ m g, m ∈ newMemories → g ∈ newGoals →
    memory_t m ≠ goal_t g
  erasedMemoryDisjoint : ErasedTargetsDisjointMemories retainedTargets newMemories
  erasedGoalDisjoint : ErasedTargetsDisjointGoals retainedTargets newGoals
  abstractionHasOrigins : ∀ m, m ∈ newMemories →
    memory_kind m = .Abstraction → memory_origins m ≠ []
  derivedProvenance : ∀ m, m ∈ newMemories → memory_kind m ≠ .Fact →
    memory_origins m ≠ [] ∨ memory_refs m ≠ []
  pinTimeStrict : ∀ m target, m ∈ newMemories → target ∈ newMemories →
    pinFrom m target → memory_tick target < memory_tick m
  assignmentsValid : RetainedGoalAssignmentValid newGoals newMemories retainedTargets
  evidenceValid : RetainedGoalEvidenceValid newGoals newMemories retainedTargets
  terminalCloseValid :
    RetainedGoalTerminalCloseFactValid newGoals newMemories retainedTargets

theorem schema_migration_source_memory_covered
    (migration : SchemaMigrationWitness) (old : Memory)
    (h : old ∈ migration.oldMemories) :
    ∃ new, new ∈ migration.newMemories ∧ migration.memoryMap old new :=
  migration.sourceMemoryCoverage old h

theorem schema_migration_source_goal_covered
    (migration : SchemaMigrationWitness) (old : Goal)
    (h : old ∈ migration.oldGoals) :
    ∃ new, new ∈ migration.newGoals ∧ migration.goalMap old new :=
  migration.sourceGoalCoverage old h

theorem schema_migration_mapped_memory_preserves_owner
    (migration : SchemaMigrationWitness) (old new : Memory)
    (h : migration.memoryMap old new) :
    memory_owner old = memory_owner new :=
  migration.mappedMemoryOwner old new h

theorem schema_migration_mapped_goal_preserves_owner
    (migration : SchemaMigrationWitness) (old new : Goal)
    (h : migration.goalMap old new) :
    goal_owner old = goal_owner new :=
  migration.mappedGoalOwner old new h

theorem schema_migration_validates_retained_store
    (migration : SchemaMigrationWitness) :
    RetainedCommittedStoreValid migration.newMemories migration.newGoals
      migration.newHeads migration.retainedTargets := by
  exact {
    graph := {
      memoryIdUnique := migration.newMemoryIdUnique
      goalIdUnique := migration.newGoalIdUnique
      erasedPinIdUnique := migration.retainedTargetIdUnique
      headAligned := migration.newHeadAligned
      forkAnchorAligned := migration.newForkAnchorAligned
      pinTargetsExist := migration.retainedPinTargetsExist
      motivationsValid := migration.retainedMotivationsValid
      guidanceValid := migration.retainedGuidanceValid
      originKind := migration.retainedOriginKind
      memoryGoalIdsDisjoint := migration.memoryGoalIdsDisjoint
      erasedMemoryDisjoint := migration.erasedMemoryDisjoint
      erasedGoalDisjoint := migration.erasedGoalDisjoint
      abstractionHasOrigins := migration.abstractionHasOrigins
      derivedProvenance := migration.derivedProvenance
      pinTimeStrict := migration.pinTimeStrict
    }
    assignments := migration.assignmentsValid
    evidence := migration.evidenceValid
    terminalClose := migration.terminalCloseValid
  }

end Causa
