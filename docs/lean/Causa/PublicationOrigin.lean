import Causa.Publication
import Causa.Compliance

/-
Publication origin is a payload-free positive allowlist keyed by Fact `t`.
It preserves the immutable typed publication owner and optional typed source
through transfer and outbox pruning. Fresh capture is a distinct admission
operation; replay never writes origin metadata. Intake and backlog re-offer
share the same origin-plus-no-hard-delete-witness gate.

Backfill binds to the stable `OwnerRef` captured on a surviving outbox row;
the existing `Publication.Captured.owner` is a resolved `Owner` and cannot
recover that stable reference. The capture projection, source evidence, and
its SQL availability remain runtime obligations. Exact-`t` outbox/origin
removal below is an operation law only. It does not claim that every Rust
hard-erase entry point already invokes it. Source selection, transaction/lock
realization, migration provenance, and all SQL behavior remain runtime
obligations. Existing Compliance hard-delete authority is unchanged.
-/

namespace Causa.PublicationOrigin

open Causa

structure SourceId where
  private mk ::
  private token : String

def SourceId.ofToken (token : String) : SourceId := ⟨token⟩

/-- The native Proxima source token carried by this publication-index value. -/
def SourceId.nativeToken (source : SourceId) : String := source.token

theorem SourceId.ofToken_preserves_native_token (token : String) :
    (SourceId.ofToken token).nativeToken = token := rfl

theorem SourceId.ofToken_injective {left right : String}
    (same : SourceId.ofToken left = SourceId.ofToken right) : left = right := by
  have sameToken := congrArg SourceId.nativeToken same
  simpa [SourceId.nativeToken] using sameToken

/-- No event payload or payload digest is retained in this positive allowlist. -/
structure Metadata where
  originalOwner : OwnerRef
  source : Option SourceId

/-- The Fact ID is part of each key; `OriginKeyUnique` makes it single-valued. -/
structure OriginRecord where
  fact : MemoryId
  metadata : Metadata

/-- The published event's `Captured.owner` is a resolved `Owner`; backfill needs
    the stable owner-table reference already persisted by the outbox. This typed
    projection records that immutable row field separately. -/
structure CapturedOutboxOwner where
  fact : MemoryId
  originalOwner : OwnerRef

structure State where
  liveFacts : Set MemoryId
  outboxFacts : Set MemoryId
  capturedOutboxOwners : Set CapturedOutboxOwner
  origins : Set OriginRecord
  currentOwners : Set (MemoryId × OwnerRef)

def emptyState : State := {
  liveFacts := fun _ => False
  outboxFacts := fun _ => False
  capturedOutboxOwners := fun _ => False
  origins := fun _ => False
  currentOwners := fun _ => False
}

def OriginKeyUnique (origins : Set OriginRecord) : Prop :=
  ∀ first second, first ∈ origins → second ∈ origins →
    first.fact = second.fact → first = second

def originExists (origins : Set OriginRecord) (id : MemoryId) : Prop :=
  ∃ record, record ∈ origins ∧ record.fact = id

def capturedOutboxOwnerExists (captures : Set CapturedOutboxOwner) (id : MemoryId) : Prop :=
  ∃ capture, capture ∈ captures ∧ capture.fact = id

def CapturedOutboxOwnerUnique (captures : Set CapturedOutboxOwner) : Prop :=
  ∀ first second, first ∈ captures → second ∈ captures →
    first.fact = second.fact → first = second

def hasCapturedOriginalOwner (captures : Set CapturedOutboxOwner)
    (id : MemoryId) (owner : OwnerRef) : Prop :=
  ∃ capture, capture ∈ captures ∧ capture.fact = id ∧ capture.originalOwner = owner

def addId (ids : Set MemoryId) (id : MemoryId) : Set MemoryId :=
  fun candidate => candidate ∈ ids ∨ candidate = id

def removeId (ids : Set MemoryId) (id : MemoryId) : Set MemoryId :=
  fun candidate => candidate ∈ ids ∧ candidate ≠ id

def FreshCaptureAllowed (state : State) (id : MemoryId)
    (witnesses : Set ErasedPinTarget) : Prop :=
  id ∉ state.liveFacts ∧ id ∉ state.outboxFacts ∧
    ¬ originExists state.origins id ∧
    ¬ erasedPinTargetExists witnesses id ∧
    ¬ capturedOutboxOwnerExists state.capturedOutboxOwners id

/-- This single result inserts the Fact, outbox key, typed owner binding and
    payload-free origin. Only the guarded fresh-admission rule may use it. -/
def captureFresh (state : State) (id : MemoryId) (metadata : Metadata) : State :=
  { liveFacts := addId state.liveFacts id
    outboxFacts := addId state.outboxFacts id
    capturedOutboxOwners := fun capture => capture ∈ state.capturedOutboxOwners ∨
      capture = ⟨id, metadata.originalOwner⟩
    origins := fun record => record ∈ state.origins ∨
      record = ⟨id, metadata⟩
    currentOwners := fun binding => binding ∈ state.currentOwners ∨
      binding = (id, metadata.originalOwner) }

inductive PublicationStep (witnesses : Set ErasedPinTarget) : State → State → Prop where
  | freshAdmission (state : State) (id : MemoryId) (metadata : Metadata)
      (fresh : FreshCaptureAllowed state id witnesses) :
      PublicationStep witnesses state (captureFresh state id metadata)
  | replay (state : State) (id : MemoryId) (metadata : Metadata) :
      PublicationStep witnesses state state

theorem fresh_admission_captures_atomically
    (state : State) (id : MemoryId) (metadata : Metadata)
    (witnesses : Set ErasedPinTarget)
    (fresh : FreshCaptureAllowed state id witnesses) :
    PublicationStep witnesses state (captureFresh state id metadata) ∧
      id ∈ (captureFresh state id metadata).liveFacts ∧
      id ∈ (captureFresh state id metadata).outboxFacts ∧
      (⟨id, metadata.originalOwner⟩ : CapturedOutboxOwner) ∈
        (captureFresh state id metadata).capturedOutboxOwners ∧
      (⟨id, metadata⟩ : OriginRecord) ∈ (captureFresh state id metadata).origins := by
  refine ⟨.freshAdmission state id metadata fresh, ?_, ?_, ?_, ?_⟩
  · exact Or.inr rfl
  · exact Or.inr rfl
  · exact Or.inr rfl
  · exact Or.inr rfl

theorem live_fact_cannot_be_freshly_recaptured
    (state : State) (id : MemoryId) (witnesses : Set ErasedPinTarget)
    (alreadyLive : id ∈ state.liveFacts) :
    ¬ FreshCaptureAllowed state id witnesses := by
  intro fresh
  exact fresh.1 alreadyLive

theorem hard_deleted_fact_cannot_be_freshly_recaptured
    (state : State) (id : MemoryId) (witnesses : Set ErasedPinTarget)
    (deleted : erasedPinTargetExists witnesses id) :
    ¬ FreshCaptureAllowed state id witnesses := by
  intro fresh
  exact fresh.2.2.2.1 deleted

theorem fresh_capture_preserves_key_uniqueness
    (state : State) (id : MemoryId) (metadata : Metadata)
    (unique : OriginKeyUnique state.origins)
    (fresh : ¬ originExists state.origins id) :
    OriginKeyUnique (captureFresh state id metadata).origins := by
  intro first second firstPresent secondPresent sameFact
  change first ∈ state.origins ∨ first = ⟨id, metadata⟩ at firstPresent
  change second ∈ state.origins ∨ second = ⟨id, metadata⟩ at secondPresent
  cases firstPresent with
  | inl firstOld =>
      cases secondPresent with
      | inl secondOld => exact unique first second firstOld secondOld sameFact
      | inr secondNew =>
          subst second
          have firstAtNew : first.fact = id := by simpa using sameFact
          exact False.elim (fresh ⟨first, firstOld, firstAtNew⟩)
  | inr firstNew =>
      cases secondPresent with
      | inl secondOld =>
          subst first
          have secondAtNew : second.fact = id := by simpa using sameFact.symm
          exact False.elim (fresh ⟨second, secondOld, secondAtNew⟩)
      | inr secondNew =>
          subst first
          subst second
          rfl

theorem fresh_capture_preserves_captured_owner_uniqueness
    (state : State) (id : MemoryId) (metadata : Metadata)
    (witnesses : Set ErasedPinTarget)
    (unique : CapturedOutboxOwnerUnique state.capturedOutboxOwners)
    (fresh : FreshCaptureAllowed state id witnesses) :
    CapturedOutboxOwnerUnique (captureFresh state id metadata).capturedOutboxOwners := by
  intro first second firstPresent secondPresent sameFact
  change first ∈ state.capturedOutboxOwners ∨ first = ⟨id, metadata.originalOwner⟩ at firstPresent
  change second ∈ state.capturedOutboxOwners ∨ second = ⟨id, metadata.originalOwner⟩ at secondPresent
  cases firstPresent with
  | inl firstOld =>
      cases secondPresent with
      | inl secondOld => exact unique first second firstOld secondOld sameFact
      | inr secondNew =>
          subst second
          have firstAtNew : first.fact = id := by simpa using sameFact
          exact False.elim (fresh.2.2.2.2 ⟨first, firstOld, firstAtNew⟩)
  | inr firstNew =>
      cases secondPresent with
      | inl secondOld =>
          subst first
          have secondAtNew : second.fact = id := by simpa using sameFact.symm
          exact False.elim (fresh.2.2.2.2 ⟨second, secondOld, secondAtNew⟩)
      | inr secondNew =>
          subst first
          subst second
          rfl

def transfer (state : State) (id : MemoryId) (destination : OwnerRef) : State :=
  { state with currentOwners := fun binding =>
      (binding ∈ state.currentOwners ∧ binding.1 ≠ id) ∨
        binding = (id, destination) }

def pruneOutbox (state : State) (retained : Set MemoryId) : State :=
  { state with
    outboxFacts := fun id => id ∈ state.outboxFacts ∧ id ∈ retained
    capturedOutboxOwners := fun capture => capture ∈ state.capturedOutboxOwners ∧
      capture.fact ∈ retained }

theorem fresh_capture_stamps_outbox_original_owner
    (state : State) (id : MemoryId) (metadata : Metadata) :
    (⟨id, metadata.originalOwner⟩ : CapturedOutboxOwner) ∈
      (captureFresh state id metadata).capturedOutboxOwners := Or.inr rfl

theorem transfer_preserves_captured_outbox_owner
    (state : State) (id : MemoryId) (destination : OwnerRef) :
    (transfer state id destination).capturedOutboxOwners = state.capturedOutboxOwners := rfl

theorem transfer_preserves_original_owner_and_source
    (state : State) (id : MemoryId) (destination : OwnerRef) :
    (transfer state id destination).origins = state.origins := rfl

theorem outbox_pruning_preserves_original_owner_and_source
    (state : State) (retained : Set MemoryId) :
    (pruneOutbox state retained).origins = state.origins := rfl

def originMatchesSource (record : OriginRecord) (owner : OwnerRef) (source : SourceId) : Prop :=
  record.metadata.originalOwner = owner ∧ record.metadata.source = some source

def hasOriginalSourceOrigin (origins : Set OriginRecord)
    (owner : OwnerRef) (source : SourceId) (id : MemoryId) : Prop :=
  ∃ record, record ∈ origins ∧ record.fact = id ∧ originMatchesSource record owner source

def originMatchesOwner (record : OriginRecord) (owner : OwnerRef) : Prop :=
  record.metadata.originalOwner = owner

def hasOriginalOwnerOrigin (origins : Set OriginRecord)
    (owner : OwnerRef) (id : MemoryId) : Prop :=
  ∃ record, record ∈ origins ∧ record.fact = id ∧ originMatchesOwner record owner

def revokeOriginalSource (state : State) (owner : OwnerRef) (source : SourceId) : State :=
  { state with
    outboxFacts := fun id => id ∈ state.outboxFacts ∧
      ¬ hasOriginalSourceOrigin state.origins owner source id
    capturedOutboxOwners := fun capture => capture ∈ state.capturedOutboxOwners ∧
      ¬ hasOriginalSourceOrigin state.origins owner source capture.fact
    origins := fun record => record ∈ state.origins ∧
      ¬ originMatchesSource record owner source }

theorem source_revoke_removes_matching_origin
    (state : State) (owner : OwnerRef) (source : SourceId)
    (record : OriginRecord)
    (matching : record.metadata.originalOwner = owner ∧
      record.metadata.source = some source) :
    record ∉ (revokeOriginalSource state owner source).origins := by
  intro retained
  exact retained.2 matching

theorem source_revoke_preserves_nonmatching_origin
    (state : State) (owner : OwnerRef) (source : SourceId)
    (record : OriginRecord) (present : record ∈ state.origins)
    (unmatched : ¬ (record.metadata.originalOwner = owner ∧
      record.metadata.source = some source)) :
    record ∈ (revokeOriginalSource state owner source).origins :=
  ⟨present, unmatched⟩

theorem source_revoke_removes_matching_outbox_copy
    (state : State) (owner : OwnerRef) (source : SourceId)
    (record : OriginRecord) (outboxPresent : record.fact ∈ state.outboxFacts)
    (originPresent : record ∈ state.origins)
    (matching : originMatchesSource record owner source) :
    record.fact ∈ state.outboxFacts ∧
      record.fact ∉ (revokeOriginalSource state owner source).outboxFacts := by
  constructor
  · exact outboxPresent
  · intro retained
    exact retained.2 ⟨record, originPresent, rfl, matching⟩

theorem source_revoke_removes_captured_outbox_owner
    (state : State) (owner : OwnerRef) (source : SourceId)
    (capture : CapturedOutboxOwner)
    (matching : hasOriginalSourceOrigin state.origins owner source capture.fact) :
    capture ∉ (revokeOriginalSource state owner source).capturedOutboxOwners := by
  intro retained
  exact retained.2 matching

theorem source_revoke_preserves_unmatched_outbox_id
    (state : State) (owner : OwnerRef) (source : SourceId) (id : MemoryId)
    (outboxPresent : id ∈ state.outboxFacts)
    (unmatched : ¬ hasOriginalSourceOrigin state.origins owner source id) :
    id ∈ (revokeOriginalSource state owner source).outboxFacts :=
  ⟨outboxPresent, unmatched⟩

theorem source_revoke_retains_live_facts_and_current_owners
    (state : State) (owner : OwnerRef) (source : SourceId) :
    (revokeOriginalSource state owner source).liveFacts = state.liveFacts ∧
      (revokeOriginalSource state owner source).currentOwners = state.currentOwners :=
  ⟨rfl, rfl⟩

def revokeOriginalOwner (state : State) (owner : OwnerRef) : State :=
  { state with
    outboxFacts := fun id => id ∈ state.outboxFacts ∧
      ¬ hasOriginalOwnerOrigin state.origins owner id
    capturedOutboxOwners := fun capture => capture ∈ state.capturedOutboxOwners ∧
      ¬ hasOriginalOwnerOrigin state.origins owner capture.fact
    origins := fun record => record ∈ state.origins ∧
      ¬ originMatchesOwner record owner }

theorem owner_revoke_removes_matching_origin
    (state : State) (owner : OwnerRef) (record : OriginRecord)
    (matching : originMatchesOwner record owner) :
    record ∉ (revokeOriginalOwner state owner).origins := by
  intro retained
  exact retained.2 matching

theorem owner_revoke_preserves_other_owner_origin
    (state : State) (owner : OwnerRef) (record : OriginRecord)
    (present : record ∈ state.origins)
    (otherOwner : record.metadata.originalOwner ≠ owner) :
    record ∈ (revokeOriginalOwner state owner).origins :=
  ⟨present, otherOwner⟩

theorem owner_revoke_removes_matching_outbox_copy
    (state : State) (owner : OwnerRef) (record : OriginRecord)
    (outboxPresent : record.fact ∈ state.outboxFacts)
    (originPresent : record ∈ state.origins)
    (matching : originMatchesOwner record owner) :
    record.fact ∈ state.outboxFacts ∧
      record.fact ∉ (revokeOriginalOwner state owner).outboxFacts := by
  constructor
  · exact outboxPresent
  · intro retained
    exact retained.2 ⟨record, originPresent, rfl, matching⟩

theorem owner_revoke_removes_captured_outbox_owner
    (state : State) (owner : OwnerRef) (capture : CapturedOutboxOwner)
    (matching : hasOriginalOwnerOrigin state.origins owner capture.fact) :
    capture ∉ (revokeOriginalOwner state owner).capturedOutboxOwners := by
  intro retained
  exact retained.2 matching

theorem owner_revoke_removes_source_free_outbox_copy
    (state : State) (owner : OwnerRef) (record : OriginRecord)
    (outboxPresent : record.fact ∈ state.outboxFacts)
    (originPresent : record ∈ state.origins)
    (ownerMatches : record.metadata.originalOwner = owner)
    (sourceFree : record.metadata.source = none) :
    record.metadata.source = none ∧ record.fact ∈ state.outboxFacts ∧
      record.fact ∉ (revokeOriginalOwner state owner).outboxFacts ∧
      record ∉ (revokeOriginalOwner state owner).origins := by
  refine ⟨sourceFree, outboxPresent, ?_, ?_⟩
  · intro retained
    exact retained.2 ⟨record, originPresent, rfl, ownerMatches⟩
  · intro retained
    exact retained.2 ownerMatches

theorem owner_revoke_preserves_unmatched_outbox_id
    (state : State) (owner : OwnerRef) (id : MemoryId)
    (outboxPresent : id ∈ state.outboxFacts)
    (unmatched : ¬ hasOriginalOwnerOrigin state.origins owner id) :
    id ∈ (revokeOriginalOwner state owner).outboxFacts :=
  ⟨outboxPresent, unmatched⟩

theorem owner_revoke_retains_live_facts_and_current_owners
    (state : State) (owner : OwnerRef) :
    (revokeOriginalOwner state owner).liveFacts = state.liveFacts ∧
      (revokeOriginalOwner state owner).currentOwners = state.currentOwners :=
  ⟨rfl, rfl⟩

def exactFactRevoke (state : State) (id : MemoryId) : State :=
  { state with
    liveFacts := removeId state.liveFacts id
    outboxFacts := removeId state.outboxFacts id
    capturedOutboxOwners := fun capture => capture ∈ state.capturedOutboxOwners ∧
      capture.fact ≠ id
    origins := fun record => record ∈ state.origins ∧ record.fact ≠ id
    currentOwners := fun binding => binding ∈ state.currentOwners ∧ binding.1 ≠ id }

theorem exact_fact_revoke_is_global
    (state : State) (id : MemoryId) (record : OriginRecord)
    (sameFact : record.fact = id) :
    record ∉ (exactFactRevoke state id).origins ∧
      id ∉ (exactFactRevoke state id).outboxFacts := by
  constructor
  · intro retained
    exact retained.2 sameFact
  · intro retained
    exact retained.2 rfl

theorem exact_fact_revoke_preserves_other_fact
    (state : State) (erased : MemoryId) (record : OriginRecord)
    (different : record.fact ≠ erased) (present : record ∈ state.origins) :
    record ∈ (exactFactRevoke state erased).origins :=
  ⟨present, different⟩

/-- This gate runs before intake performs payload deduplication or conflict work. -/
def eligibleForPayload (origins : Set OriginRecord) (witnesses : Set ErasedPinTarget)
    (requestedOwner : OwnerRef) (id : MemoryId) : Prop :=
  ∃ record, record ∈ origins ∧ record.fact = id ∧
    record.metadata.originalOwner = requestedOwner ∧
    ¬ erasedPinTargetExists witnesses id

def intakeMayHandlePayload := eligibleForPayload
def backlogMayReoffer := eligibleForPayload

theorem intake_eligibility_requires_matching_origin_and_no_witness
    (origins : Set OriginRecord) (witnesses : Set ErasedPinTarget)
    (owner : OwnerRef) (id : MemoryId)
    (eligible : intakeMayHandlePayload origins witnesses owner id) :
    (∃ record, record ∈ origins ∧ record.fact = id ∧
      record.metadata.originalOwner = owner) ∧
      ¬ erasedPinTargetExists witnesses id := by
  rcases eligible with ⟨record, present, factMatches, ownerMatches, noWitness⟩
  exact ⟨⟨record, present, factMatches, ownerMatches⟩, noWitness⟩

theorem missing_origin_fails_closed
    (origins : Set OriginRecord) (witnesses : Set ErasedPinTarget)
    (owner : OwnerRef) (id : MemoryId)
    (missing : ¬ originExists origins id) :
    ¬ intakeMayHandlePayload origins witnesses owner id := by
  rintro ⟨record, present, factMatches, _, _⟩
  exact missing ⟨record, present, factMatches⟩

theorem hard_delete_witness_fails_closed
    (origins : Set OriginRecord) (witnesses : Set ErasedPinTarget)
    (owner : OwnerRef) (id : MemoryId)
    (deleted : erasedPinTargetExists witnesses id) :
    ¬ intakeMayHandlePayload origins witnesses owner id := by
  rintro ⟨_, _, _, _, noWitness⟩
  exact noWitness deleted

theorem backlog_reoffer_uses_intake_eligibility
    (origins : Set OriginRecord) (witnesses : Set ErasedPinTarget)
    (owner : OwnerRef) (id : MemoryId) :
    backlogMayReoffer origins witnesses owner id ↔
      intakeMayHandlePayload origins witnesses owner id := Iff.rfl

theorem replay_does_not_restore_revoked_origin
    (witnesses : Set ErasedPinTarget) (state : State) (id : MemoryId)
    (metadata : Metadata) (revoked : ¬ originExists state.origins id) :
    ∃ after, PublicationStep witnesses state after ∧ after = state ∧
      ¬ originExists after.origins id :=
  ⟨state, .replay state id metadata, rfl, revoked⟩

theorem source_revocation_does_not_enable_same_fact_recapture
    (state : State) (owner : OwnerRef) (source : SourceId)
    (id : MemoryId) (witnesses : Set ErasedPinTarget)
    (live : id ∈ state.liveFacts) :
    ¬ FreshCaptureAllowed (revokeOriginalSource state owner source) id witnesses := by
  intro fresh
  exact fresh.1 live

theorem fresh_fact_can_reuse_revoked_source
    (state : State) (owner : OwnerRef) (source : SourceId)
    (oldFact newFact : MemoryId)
    (oldLive : oldFact ∈ state.liveFacts)
    (oldRecord : ({ fact := oldFact, metadata := ⟨owner, some source⟩ } : OriginRecord) ∈
      state.origins)
    (witnesses : Set ErasedPinTarget)
    (fresh : FreshCaptureAllowed (revokeOriginalSource state owner source)
      newFact witnesses) :
    ({ fact := oldFact, metadata := (⟨owner, some source⟩ : Metadata) } : OriginRecord) ∉
      (revokeOriginalSource state owner source).origins ∧
    (⟨newFact, (⟨owner, some source⟩ : Metadata)⟩ : OriginRecord) ∈
      (captureFresh (revokeOriginalSource state owner source) newFact
        ⟨owner, some source⟩).origins ∧
    intakeMayHandlePayload
      (captureFresh (revokeOriginalSource state owner source) newFact
        ⟨owner, some source⟩).origins witnesses owner newFact ∧
    newFact ≠ oldFact := by
  have oldRevoked := source_revoke_removes_matching_origin state owner source
    ⟨oldFact, ⟨owner, some source⟩⟩ ⟨rfl, rfl⟩
  have distinct : newFact ≠ oldFact := by
    intro same
    subst oldFact
    exact fresh.1 oldLive
  have newOrigin :
      (⟨newFact, (⟨owner, some source⟩ : Metadata)⟩ : OriginRecord) ∈
        (captureFresh (revokeOriginalSource state owner source) newFact
          ⟨owner, some source⟩).origins := Or.inr rfl
  have noWitness := fresh.2.2.2.1
  have canHandle : intakeMayHandlePayload
      (captureFresh (revokeOriginalSource state owner source) newFact
        ⟨owner, some source⟩).origins witnesses owner newFact :=
    ⟨⟨newFact, ⟨owner, some source⟩⟩, newOrigin, rfl, rfl, noWitness⟩
  exact ⟨oldRevoked, newOrigin, canHandle, distinct⟩

def BackfillEvidence (outbox retainedFacts : Set MemoryId)
    (captures : Set CapturedOutboxOwner) (witnesses : Set ErasedPinTarget)
    (sourceAt : MemoryId → Option (Option SourceId)) (record : OriginRecord) : Prop :=
  CapturedOutboxOwnerUnique captures ∧ record.fact ∈ outbox ∧ record.fact ∈ retainedFacts ∧
    ¬ erasedPinTargetExists witnesses record.fact ∧
    hasCapturedOriginalOwner captures record.fact record.metadata.originalOwner ∧
    sourceAt record.fact = some record.metadata.source

/-- `some none` is a known absent source. `none` is missing source evidence.
    Only surviving outbox + retained hot/cooled Fact evidence is admitted. -/
def backfillOrigins (outbox retainedFacts : Set MemoryId)
    (captures : Set CapturedOutboxOwner) (witnesses : Set ErasedPinTarget)
    (sourceAt : MemoryId → Option (Option SourceId)) : Set OriginRecord :=
  fun record => BackfillEvidence outbox retainedFacts captures witnesses sourceAt record

theorem backfill_requires_surviving_evidence
    (outbox retainedFacts : Set MemoryId) (witnesses : Set ErasedPinTarget)
    (captures : Set CapturedOutboxOwner) (sourceAt : MemoryId → Option (Option SourceId))
    (record : OriginRecord)
    (backfilled : record ∈ backfillOrigins outbox retainedFacts captures witnesses sourceAt) :
    CapturedOutboxOwnerUnique captures ∧ record.fact ∈ outbox ∧ record.fact ∈ retainedFacts ∧
      ¬ erasedPinTargetExists witnesses record.fact ∧
      hasCapturedOriginalOwner captures record.fact record.metadata.originalOwner ∧
      sourceAt record.fact = some record.metadata.source := backfilled

theorem missing_backfill_evidence_fails_closed
    (outbox retainedFacts : Set MemoryId) (witnesses : Set ErasedPinTarget)
    (captures : Set CapturedOutboxOwner) (sourceAt : MemoryId → Option (Option SourceId))
    (record : OriginRecord) (missingOutbox : record.fact ∉ outbox) :
    record ∉ backfillOrigins outbox retainedFacts captures witnesses sourceAt := by
  exact fun evidence => missingOutbox evidence.2.1

theorem missing_owner_or_source_metadata_fails_closed
    (outbox retainedFacts : Set MemoryId) (witnesses : Set ErasedPinTarget)
    (captures : Set CapturedOutboxOwner) (sourceAt : MemoryId → Option (Option SourceId))
    (record : OriginRecord)
  (missingOwner : ¬ hasCapturedOriginalOwner captures record.fact record.metadata.originalOwner ∨
      sourceAt record.fact ≠ some record.metadata.source) :
    record ∉ backfillOrigins outbox retainedFacts captures witnesses sourceAt := by
  intro evidence
  rcases missingOwner with ownerMissing | sourceMissing
  · exact ownerMissing evidence.2.2.2.2.1
  · exact sourceMissing evidence.2.2.2.2.2

theorem backfill_after_transfer_uses_captured_original_owner
    (id : MemoryId) (originalOwner destination : OwnerRef) (source : SourceId)
    (differentOwners : originalOwner ≠ destination)
    (retainedFacts : Set MemoryId) (sourceAt : MemoryId → Option (Option SourceId))
    (retained : id ∈ retainedFacts) (witnesses : Set ErasedPinTarget)
    (noWitness : ¬ erasedPinTargetExists witnesses id)
    (sourceEvidence : sourceAt id = some (some source)) :
    let captured := captureFresh emptyState id ⟨originalOwner, some source⟩
    let transferred := transfer captured id destination
    let original : OriginRecord := ⟨id, ⟨originalOwner, some source⟩⟩
    original ∈ backfillOrigins transferred.outboxFacts retainedFacts
        transferred.capturedOutboxOwners witnesses sourceAt ∧
      (⟨id, ⟨destination, some source⟩⟩ : OriginRecord) ∉
        backfillOrigins transferred.outboxFacts retainedFacts
          transferred.capturedOutboxOwners witnesses sourceAt := by
  dsimp
  constructor
  · change BackfillEvidence _ _ _ _ _ _
    have fresh : FreshCaptureAllowed emptyState id witnesses := by
      refine ⟨?_, ?_, ?_, noWitness, ?_⟩
      · intro present
        change False at present
        exact present
      · intro present
        change False at present
        exact present
      · intro present
        rcases present with ⟨record, belongs, _⟩
        change False at belongs
        exact belongs
      · intro present
        rcases present with ⟨capture, belongs, _⟩
        change False at belongs
        exact belongs
    have initiallyUnique : CapturedOutboxOwnerUnique emptyState.capturedOutboxOwners := by
      intro first second firstPresent _ _
      change False at firstPresent
      exact False.elim firstPresent
    have uniqueCaptured : CapturedOutboxOwnerUnique
        (captureFresh emptyState id ⟨originalOwner, some source⟩).capturedOutboxOwners := by
      exact fresh_capture_preserves_captured_owner_uniqueness emptyState id
        ⟨originalOwner, some source⟩ witnesses initiallyUnique fresh
    refine ⟨uniqueCaptured, ?_, retained, noWitness, ?_, sourceEvidence⟩
    · exact Or.inr rfl
    · exact ⟨⟨id, originalOwner⟩, Or.inr rfl, rfl, rfl⟩
  · intro evidence
    have captureIsDestination := evidence.2.2.2.2.1
    rcases captureIsDestination with ⟨capture, present, sameFact, sameOwner⟩
    change capture ∈ emptyState.capturedOutboxOwners ∨
      capture = ⟨id, originalOwner⟩ at present
    rcases present with impossible | exactCapture
    · exact False.elim (by simpa [emptyState] using impossible)
    · subst capture
      exact differentOwners sameOwner

theorem backfill_accepts_known_source_absence
    (id : MemoryId) (owner : OwnerRef) (retainedFacts : Set MemoryId)
    (sourceAt : MemoryId → Option (Option SourceId))
    (witnesses : Set ErasedPinTarget) (retained : id ∈ retainedFacts)
    (noWitness : ¬ erasedPinTargetExists witnesses id)
    (knownAbsent : sourceAt id = some none) :
    let captured := captureFresh emptyState id ⟨owner, none⟩
    let record : OriginRecord := ⟨id, ⟨owner, none⟩⟩
    record ∈ backfillOrigins captured.outboxFacts retainedFacts
      captured.capturedOutboxOwners witnesses sourceAt := by
  dsimp
  change BackfillEvidence _ _ _ _ _ _
  have fresh : FreshCaptureAllowed emptyState id witnesses := by
    refine ⟨?_, ?_, ?_, noWitness, ?_⟩
    · intro present
      change False at present
      exact present
    · intro present
      change False at present
      exact present
    · intro present
      rcases present with ⟨record, belongs, _⟩
      change False at belongs
      exact belongs
    · intro present
      rcases present with ⟨capture, belongs, _⟩
      change False at belongs
      exact belongs
  have initiallyUnique : CapturedOutboxOwnerUnique emptyState.capturedOutboxOwners := by
    intro first second firstPresent _ _
    change False at firstPresent
    exact False.elim firstPresent
  have uniqueCaptured : CapturedOutboxOwnerUnique
      (captureFresh emptyState id ⟨owner, none⟩).capturedOutboxOwners := by
    exact fresh_capture_preserves_captured_owner_uniqueness emptyState id
      ⟨owner, none⟩ witnesses initiallyUnique fresh
  refine ⟨uniqueCaptured, Or.inr rfl, retained, noWitness, ?_, knownAbsent⟩
  exact ⟨⟨id, owner⟩, Or.inr rfl, rfl, rfl⟩

theorem missing_known_source_evidence_fails_closed
    (outbox retainedFacts : Set MemoryId) (captures : Set CapturedOutboxOwner)
    (witnesses : Set ErasedPinTarget) (sourceAt : MemoryId → Option (Option SourceId))
    (record : OriginRecord) (missing : sourceAt record.fact = none) :
    record ∉ backfillOrigins outbox retainedFacts captures witnesses sourceAt := by
  intro evidence
  have sourceBackfill := evidence.2.2.2.2.2
  rw [missing] at sourceBackfill
  cases sourceBackfill

end Causa.PublicationOrigin
