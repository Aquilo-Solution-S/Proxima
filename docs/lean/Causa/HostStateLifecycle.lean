import Causa.HostStateMaintenance

/-
Host-state lifecycle callbacks are an inverse inside an already selected core
owner/source/exact-Fact transaction. This module adds no erase authority and leaves
Compliance.wipeable and its hard-erase transitions unchanged. It models exact
coverage, receipt shape, atomic commit choice, and one owner-bound export
snapshot. Selection, authorization, SQL isolation, callback SQL fidelity, and
the truth of reported row counts remain runtime obligations.
-/

namespace Causa.HostStateLifecycle

open Causa.HostStateMaintenance

structure FactCopyLocator where
  originalOwner : OwnerRef
  fact : MemoryId

/-- The two disjoint identity domains supplied to a host lifecycle inverse. -/
structure CopyEraseSelection where
  physicalFacts : Set MemoryId
  originalCopies : Set FactCopyLocator

def noFacts : Set MemoryId := fun _ => False
def noCopies : Set FactCopyLocator := fun _ => False

def emptyCopyEraseSelection : CopyEraseSelection := ⟨noFacts, noCopies⟩

def physicalFactsSelection (facts : Set MemoryId) : CopyEraseSelection :=
  ⟨facts, noCopies⟩

def selectionMatches (expected actual : CopyEraseSelection) : Prop :=
  (∀ fact, fact ∈ expected.physicalFacts ↔ fact ∈ actual.physicalFacts) ∧
    (∀ locator, locator ∈ expected.originalCopies ↔ locator ∈ actual.originalCopies)

structure SourceScopeId where
  private mk ::
  token : String
  deriving DecidableEq, Repr

def SourceScopeId.ofToken (token : String) : SourceScopeId := ⟨token⟩

theorem SourceScopeId.ext_of_token {left right : SourceScopeId}
    (same : left.token = right.token) : left = right := by
  cases left
  cases right
  cases same
  rfl

inductive Scope where
  | wholeOwner
  | source (source : SourceScopeId)
  | exactFacts
  deriving DecidableEq, Repr

structure LifecycleRequest where
  owner : OwnerRef
  scope : Scope
  selection : CopyEraseSelection

inductive EraseDisposition where
  | erase
  | retain
  deriving DecidableEq, Repr

inductive ExportDisposition where
  | include
  | exclude
  deriving DecidableEq, Repr

structure SurfacePolicy where
  surface : StateSurface
  wholeOwnerErase : EraseDisposition
  sourceErase : EraseDisposition
  exactFactErase : EraseDisposition
  ownerExport : ExportDisposition
  deriving DecidableEq

structure LifecycleRegistration where
  participant : ParticipantId
  /-- Frozen host-state tables declared by this participant. -/
  tables : List StateSurface
  /-- Tables already handled by Proxima's generic erase/export paths. -/
  genericTables : List StateSurface
  /-- One explicit policy row per host-state table. -/
  policies : List SurfacePolicy

structure ExactTableSet (declared actual : List StateSurface) : Prop where
  actualUnique : actual.Nodup
  actualWithinDeclared : ∀ surface, surface ∈ actual → surface ∈ declared
  declaredCovered : ∀ surface, surface ∈ declared → surface ∈ actual

def policyTables (registration : LifecycleRegistration) : List StateSurface :=
  registration.policies.map SurfacePolicy.surface

structure CoverageComplete (registration : LifecycleRegistration) : Prop where
  declaredUnique : registration.tables.Nodup
  genericUnique : registration.genericTables.Nodup
  disjointFromGeneric :
    ∀ surface, surface ∈ registration.tables → surface ∉ registration.genericTables
  everyDeclaredTableHasPolicy :
    ExactTableSet registration.tables (policyTables registration)

theorem coverage_is_complete_and_disjoint {registration : LifecycleRegistration}
    (valid : CoverageComplete registration) :
    registration.tables.Nodup ∧ registration.genericTables.Nodup ∧
      (∀ surface, surface ∈ registration.tables → surface ∉ registration.genericTables) ∧
      ExactTableSet registration.tables (policyTables registration) :=
  ⟨valid.declaredUnique, valid.genericUnique, valid.disjointFromGeneric,
    valid.everyDeclaredTableHasPolicy⟩

theorem exact_facts_scope_is_distinct :
    Scope.exactFacts ≠ Scope.wholeOwner ∧
      ∀ source, Scope.exactFacts ≠ Scope.source source := by
  constructor
  · intro impossible
    cases impossible
  · intro source impossible
    cases impossible

def includedExportTables (registration : LifecycleRegistration) : List StateSurface :=
  (registration.policies.filter
    (fun policy => decide (policy.ownerExport = .include))).map SurfacePolicy.surface

def eraseDispositionFor (policy : SurfacePolicy) (scope : Scope) : EraseDisposition :=
  match scope with
  | .wholeOwner => policy.wholeOwnerErase
  | .source _ => policy.sourceErase
  | .exactFacts => policy.exactFactErase

structure EraseCount where
  surface : StateSurface
  deleted : Nat
  scrubbed : Nat
  deriving DecidableEq

structure EraseReceipt where
  participant : ParticipantId
  owner : OwnerRef
  scope : Scope
  selection : CopyEraseSelection
  counts : List EraseCount

def eraseCountTables (receipt : EraseReceipt) : List StateSurface :=
  receipt.counts.map EraseCount.surface

structure EraseReceiptValid (registration : LifecycleRegistration)
    (request : LifecycleRequest) (receipt : EraseReceipt) : Prop where
  participantMatches : receipt.participant = registration.participant
  ownerMatches : receipt.owner = request.owner
  scopeMatches : receipt.scope = request.scope
  selectionMatchesRequest : selectionMatches request.selection receipt.selection
  exactFactsHasNoOriginalCopySelection :
    request.scope = .exactFacts → ∀ locator, locator ∉ request.selection.originalCopies
  exactTables : ExactTableSet registration.tables (eraseCountTables receipt)
  retainedTablesHaveZeroCounts :
    ∀ count, count ∈ receipt.counts →
      ∀ policy, policy ∈ registration.policies →
        policy.surface = count.surface →
        eraseDispositionFor policy request.scope = .retain →
        count.deleted = 0 ∧ count.scrubbed = 0

theorem receipt_binds_participant_owner_scope_and_tables
    {registration : LifecycleRegistration} {request : LifecycleRequest}
    {receipt : EraseReceipt} (valid : EraseReceiptValid registration request receipt) :
    receipt.participant = registration.participant ∧
      receipt.owner = request.owner ∧ receipt.scope = request.scope ∧
      ExactTableSet registration.tables (eraseCountTables receipt) :=
  ⟨valid.participantMatches, valid.ownerMatches, valid.scopeMatches, valid.exactTables⟩

theorem receipt_binds_typed_selection
    {registration : LifecycleRegistration} {request : LifecycleRequest}
    {receipt : EraseReceipt} (valid : EraseReceiptValid registration request receipt) :
    selectionMatches request.selection receipt.selection :=
  valid.selectionMatchesRequest

theorem selection_mismatch_invalidates_receipt
    {registration : LifecycleRegistration} {request : LifecycleRequest}
    {receipt : EraseReceipt}
    (mismatch : ¬ selectionMatches request.selection receipt.selection) :
    ¬ EraseReceiptValid registration request receipt := by
  intro valid
  exact mismatch valid.selectionMatchesRequest

theorem exact_facts_receipt_has_no_original_copy_selection
    {registration : LifecycleRegistration} {request : LifecycleRequest}
    {receipt : EraseReceipt} (valid : EraseReceiptValid registration request receipt)
    (exactScope : request.scope = .exactFacts) :
    ∀ locator, locator ∉ receipt.selection.originalCopies := by
  intro locator receiptSelected
  have requestSelected := (valid.selectionMatchesRequest.2 locator).mpr receiptSelected
  exact valid.exactFactsHasNoOriginalCopySelection exactScope locator requestSelected

theorem exact_facts_request_and_receipt_have_no_original_copy_selection
    {registration : LifecycleRegistration} {request : LifecycleRequest}
    {receipt : EraseReceipt} (valid : EraseReceiptValid registration request receipt)
    (exactScope : request.scope = .exactFacts) :
    (∀ locator, locator ∉ request.selection.originalCopies) ∧
      (∀ locator, locator ∉ receipt.selection.originalCopies) := by
  exact ⟨valid.exactFactsHasNoOriginalCopySelection exactScope,
    exact_facts_receipt_has_no_original_copy_selection valid exactScope⟩

theorem retained_source_surface_has_zero_counts
    {registration : LifecycleRegistration} {request : LifecycleRequest}
    {receipt : EraseReceipt} {count : EraseCount} {policy : SurfacePolicy}
    {source : SourceScopeId}
    (valid : EraseReceiptValid registration request receipt)
    (countPresent : count ∈ receipt.counts)
    (policyPresent : policy ∈ registration.policies)
    (sameSurface : policy.surface = count.surface)
    (sourceScope : request.scope = .source source)
    (retained : policy.sourceErase = .retain) :
    count.deleted = 0 ∧ count.scrubbed = 0 := by
  apply valid.retainedTablesHaveZeroCounts count countPresent policy policyPresent sameSurface
  rw [sourceScope]
  simp [eraseDispositionFor, retained]

theorem retained_exact_fact_surface_has_zero_counts
    {registration : LifecycleRegistration} {request : LifecycleRequest}
    {receipt : EraseReceipt} {count : EraseCount} {policy : SurfacePolicy}
    (valid : EraseReceiptValid registration request receipt)
    (countPresent : count ∈ receipt.counts)
    (policyPresent : policy ∈ registration.policies)
    (sameSurface : policy.surface = count.surface)
    (exactScope : request.scope = .exactFacts)
    (retained : policy.exactFactErase = .retain) :
    count.deleted = 0 ∧ count.scrubbed = 0 := by
  apply valid.retainedTablesHaveZeroCounts count countPresent policy policyPresent sameSurface
  rw [exactScope]
  simp [eraseDispositionFor, retained]

structure LifecycleState (Core Host : Type) where
  core : Core
  host : Host

inductive CallbackResult (Host : Type) where
  | failed
  | completed (hostAfter : Host) (receipt : EraseReceipt)

inductive CommitStatus where
  | failed
  | committed
  deriving DecidableEq, Repr

inductive EraseResult (Core Host : Type) where
  | rejected
  | committed (state : LifecycleState Core Host) (receipt : EraseReceipt)

def stateAfterErase {Core Host : Type} (before : LifecycleState Core Host) :
    EraseResult Core Host → LifecycleState Core Host
  | .rejected => before
  | .committed state _ => state

inductive EraseTransaction {Core Host : Type} :
    LifecycleRequest → Option LifecycleRegistration → Core →
      Option (CallbackResult Host) → CommitStatus → LifecycleState Core Host →
      EraseResult Core Host → Prop where
  | missingRegistration :
      EraseTransaction request none coreAfter callback commit before .rejected
  | incompleteCoverage {frozen : LifecycleRegistration}
      (invalid : ¬ CoverageComplete frozen) :
      EraseTransaction request (some frozen) coreAfter callback commit before .rejected
  | missingCallback {frozen : LifecycleRegistration}
      (coverage : CoverageComplete frozen) :
      EraseTransaction request (some frozen) coreAfter none commit before .rejected
  | callbackFailed {frozen : LifecycleRegistration}
      (coverage : CoverageComplete frozen) :
      EraseTransaction request (some frozen) coreAfter (some .failed) commit before .rejected
  | invalidReceipt {frozen : LifecycleRegistration} {hostAfter : Host}
      {receipt : EraseReceipt}
      (coverage : CoverageComplete frozen)
      (invalid : ¬ EraseReceiptValid frozen request receipt) :
      EraseTransaction request (some frozen) coreAfter
        (some (.completed hostAfter receipt)) commit before .rejected
  | commitFailed {frozen : LifecycleRegistration} {hostAfter : Host}
      {receipt : EraseReceipt}
      (coverage : CoverageComplete frozen)
      (valid : EraseReceiptValid frozen request receipt) :
      EraseTransaction request (some frozen) coreAfter
        (some (.completed hostAfter receipt)) .failed before .rejected
  | committed {frozen : LifecycleRegistration} {hostAfter : Host}
      {receipt : EraseReceipt}
      (coverage : CoverageComplete frozen)
      (valid : EraseReceiptValid frozen request receipt) :
      EraseTransaction request (some frozen) coreAfter
        (some (.completed hostAfter receipt)) .committed before
        (.committed ⟨coreAfter, hostAfter⟩ receipt)

theorem rejected_erase_preserves_core_and_host
    {Core Host : Type} {request : LifecycleRequest}
    {registration : Option LifecycleRegistration} {coreAfter : Core}
    {callback : Option (CallbackResult Host)} {commit : CommitStatus}
    {before : LifecycleState Core Host}
    (_attempt : EraseTransaction request registration coreAfter callback commit before .rejected) :
    stateAfterErase before .rejected = before := rfl

theorem committed_erase_binds_owner_and_scope
    {Core Host : Type} {request : LifecycleRequest}
    {frozen : LifecycleRegistration} {coreAfter : Core} {hostAfter : Host}
    {receipt : EraseReceipt} {before : LifecycleState Core Host}
    (attempt : EraseTransaction request (some frozen) coreAfter
      (some (.completed hostAfter receipt)) .committed before
      (.committed ⟨coreAfter, hostAfter⟩ receipt)) :
    receipt.participant = frozen.participant ∧ receipt.owner = request.owner ∧
      receipt.scope = request.scope ∧
      selectionMatches request.selection receipt.selection ∧
      (∀ surface, surface ∈ eraseCountTables receipt → surface ∈ frozen.tables) := by
  cases attempt with
  | committed coverage valid =>
      exact ⟨valid.participantMatches, valid.ownerMatches, valid.scopeMatches,
        valid.selectionMatchesRequest, valid.exactTables.actualWithinDeclared⟩

structure ExportItem (Payload : Type) where
  surface : StateSurface
  payload : Payload

structure OwnerExportSnapshot (Payload : Type) where
  participant : ParticipantId
  owner : OwnerRef
  surfaces : List (ExportItem Payload)

def exportItemTables {Payload : Type} (snapshot : OwnerExportSnapshot Payload) :
    List StateSurface := snapshot.surfaces.map ExportItem.surface

structure OwnerExportRequest where
  owner : OwnerRef

structure OwnerExportValid {Payload : Type} (registration : LifecycleRegistration)
    (request : OwnerExportRequest) (snapshot : OwnerExportSnapshot Payload) : Prop where
  participantMatches : snapshot.participant = registration.participant
  ownerMatches : snapshot.owner = request.owner
  exactIncludedTables :
    ExactTableSet (includedExportTables registration) (exportItemTables snapshot)

inductive ExportCallbackResult (Payload : Type) where
  | failed
  | completed (snapshot : OwnerExportSnapshot Payload)

inductive ExportResult (Payload : Type) where
  | rejected
  | complete (snapshot : OwnerExportSnapshot Payload)

inductive ExportTransaction {Payload : Type} :
    OwnerExportRequest → Option LifecycleRegistration →
      Option (ExportCallbackResult Payload) → CommitStatus →
      ExportResult Payload → Prop where
  | missingRegistration :
      ExportTransaction request none callback commit .rejected
  | incompleteCoverage {frozen : LifecycleRegistration}
      (invalid : ¬ CoverageComplete frozen) :
      ExportTransaction request (some frozen) callback commit .rejected
  | missingCallback {frozen : LifecycleRegistration}
      (coverage : CoverageComplete frozen) :
      ExportTransaction request (some frozen) none commit .rejected
  | callbackFailed {frozen : LifecycleRegistration}
      (coverage : CoverageComplete frozen) :
      ExportTransaction request (some frozen) (some .failed) commit .rejected
  | invalidSnapshot {frozen : LifecycleRegistration} {snapshot : OwnerExportSnapshot Payload}
      (coverage : CoverageComplete frozen)
      (invalid : ¬ OwnerExportValid frozen request snapshot) :
      ExportTransaction request (some frozen) (some (.completed snapshot)) commit .rejected
  | commitFailed {frozen : LifecycleRegistration} {snapshot : OwnerExportSnapshot Payload}
      (coverage : CoverageComplete frozen)
      (valid : OwnerExportValid frozen request snapshot) :
      ExportTransaction request (some frozen) (some (.completed snapshot)) .failed .rejected
  | completed {frozen : LifecycleRegistration} {snapshot : OwnerExportSnapshot Payload}
      (coverage : CoverageComplete frozen)
      (valid : OwnerExportValid frozen request snapshot) :
      ExportTransaction request (some frozen) (some (.completed snapshot)) .committed
        (.complete snapshot)

theorem completed_export_is_owner_bound
    {Payload : Type} {request : OwnerExportRequest} {frozen : LifecycleRegistration}
    {snapshot : OwnerExportSnapshot Payload}
    (attempt : ExportTransaction request (some frozen) (some (.completed snapshot))
      .committed (.complete snapshot)) :
    snapshot.participant = frozen.participant ∧ snapshot.owner = request.owner ∧
      (∀ surface, surface ∈ exportItemTables snapshot →
        surface ∈ includedExportTables frozen) := by
  cases attempt with
  | completed coverage valid =>
      exact ⟨valid.participantMatches, valid.ownerMatches,
        valid.exactIncludedTables.actualWithinDeclared⟩

theorem completed_export_contains_only_declared_surfaces
    {Payload : Type} {request : OwnerExportRequest} {frozen : LifecycleRegistration}
    {snapshot : OwnerExportSnapshot Payload}
    (coverage : CoverageComplete frozen)
    (attempt : ExportTransaction request (some frozen) (some (.completed snapshot))
      .committed (.complete snapshot)) :
    ∀ surface, surface ∈ exportItemTables snapshot → surface ∈ frozen.tables := by
  intro surface present
  have included : surface ∈ includedExportTables frozen :=
    (completed_export_is_owner_bound attempt).2.2 surface present
  have includedPolicy : surface ∈ policyTables frozen := by
    rcases List.mem_map.mp included with ⟨policy, filtered, policySurfaceEq⟩
    rcases List.mem_filter.mp filtered with ⟨policyPresent, _included⟩
    have policyListed : policy.surface ∈ policyTables frozen :=
      List.mem_map.mpr ⟨policy, policyPresent, rfl⟩
    rw [← policySurfaceEq]
    exact policyListed
  exact coverage.everyDeclaredTableHasPolicy.actualWithinDeclared surface includedPolicy

def exampleExecutionSurface : StateSurface := ⟨"host.execution"⟩
def exampleConfigurationSurface : StateSurface := ⟨"host.configuration"⟩
def exampleGenericSurface : StateSurface := ⟨"core.memory"⟩

def exampleRegistration : LifecycleRegistration := {
  participant := ⟨"goal_trigger"⟩
  tables := [exampleExecutionSurface, exampleConfigurationSurface]
  genericTables := [exampleGenericSurface]
  policies := [
    ⟨exampleExecutionSurface, .erase, .erase, .erase, .include⟩,
    ⟨exampleConfigurationSurface, .erase, .retain, .retain, .exclude⟩
  ]
}

theorem example_registration_has_complete_disjoint_coverage :
    CoverageComplete exampleRegistration := by
  constructor
  · decide
  · decide
  · intro surface present
    simp [exampleRegistration, exampleExecutionSurface, exampleConfigurationSurface,
      exampleGenericSurface] at present ⊢
    rcases present with present | present <;>
      simp [exampleExecutionSurface, exampleConfigurationSurface, present]
  · constructor
    · decide
    · intro surface present
      simp [policyTables, exampleRegistration, exampleExecutionSurface,
        exampleConfigurationSurface] at present ⊢
      rcases present with present | present <;>
        simp [exampleRegistration, exampleExecutionSurface, exampleConfigurationSurface, present]
    · intro surface present
      simp [policyTables, exampleRegistration, exampleExecutionSurface,
        exampleConfigurationSurface] at present ⊢
      simpa using present

theorem example_configuration_has_explicit_source_retention_and_export_exclusion :
    ∃ policy, policy ∈ exampleRegistration.policies ∧
      policy.surface = exampleConfigurationSurface ∧
      policy.sourceErase = .retain ∧ policy.exactFactErase = .retain ∧
      policy.ownerExport = .exclude := by
  refine ⟨⟨exampleConfigurationSurface, .erase, .retain, .retain, .exclude⟩,
    ?_, rfl, rfl, rfl, rfl⟩
  simp [exampleRegistration]

end Causa.HostStateLifecycle
