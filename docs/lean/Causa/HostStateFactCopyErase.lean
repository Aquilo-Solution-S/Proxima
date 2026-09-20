import Causa.HostStateLifecycle
import Causa.PublicationOrigin

/-
This is the domainless selection law for copied Fact payload rows. A physical
Fact selection and an original-publication owner/source selection are separate
typed sets shared with HostStateLifecycle. The inverse removes a row once by set
membership when either domain selects it; the input selections are premises from
the existing authorized core erase and retained origin index. SQL callback
fidelity, selector correctness, and truthful exactly-once row counts are not
proved here.
-/

namespace Causa.HostStateFactCopyErase

open Causa
open Causa.PublicationOrigin
open Causa.HostStateLifecycle

/-- The lifecycle source token and publication source token are distinct
typed wrappers over Proxima's one native SourceId namespace. This bridge is
an identity projection; CloudEvents' configured producer URI is a different
field and is never used to derive lifecycle source identity. -/
def publicationSourceForScope (source : SourceScopeId) : SourceId :=
  SourceId.ofToken source.token

theorem publication_source_scope_preserves_token (source : SourceScopeId) :
    (publicationSourceForScope source).nativeToken = source.token := by
  change (SourceId.ofToken source.token).nativeToken = source.token
  exact SourceId.ofToken_preserves_native_token _

theorem publication_source_scope_injective {left right : SourceScopeId}
    (same : publicationSourceForScope left = publicationSourceForScope right) :
    left = right := by
  change SourceId.ofToken left.token = SourceId.ofToken right.token at same
  apply SourceScopeId.ext_of_token
  exact SourceId.ofToken_injective same

theorem publication_source_scope_equal_iff_tokens_equal
    (left right : SourceScopeId) :
    publicationSourceForScope left = publicationSourceForScope right ↔
      left.token = right.token := by
  constructor
  · intro same
    exact congrArg SourceId.nativeToken same
  · intro same
    change SourceId.ofToken left.token = SourceId.ofToken right.token
    rw [same]

structure CopiedPayload (Payload : Type) where
  locator : FactCopyLocator
  payload : Payload

inductive OriginalPublicationScope where
  | owner (owner : OwnerRef)
  | source (owner : OwnerRef) (source : SourceId)

structure HostState (Payload Configuration : Type) where
  copiedPayloads : Set (CopiedPayload Payload)
  authoredConfiguration : Configuration

def byPhysicalFacts (facts : Set MemoryId) : CopyEraseSelection :=
  ⟨facts, noCopies⟩

def exactPhysicalFact (fact : MemoryId) : CopyEraseSelection :=
  byPhysicalFacts (fun selected => selected = fact)

def originalCopyLocators (origins : Set OriginRecord)
    (scope : OriginalPublicationScope) : Set FactCopyLocator :=
  fun locator =>
    match scope with
    | .owner owner =>
        locator.originalOwner = owner ∧
          hasOriginalOwnerOrigin origins owner locator.fact
    | .source owner source =>
        locator.originalOwner = owner ∧
          hasOriginalSourceOrigin origins owner source locator.fact

def byOriginalScope (origins : Set OriginRecord)
    (scope : OriginalPublicationScope) : CopyEraseSelection :=
  ⟨noFacts, originalCopyLocators origins scope⟩

def union {α : Type} (left right : Set α) : Set α :=
  fun value => value ∈ left ∨ value ∈ right

def combine (left right : CopyEraseSelection) : CopyEraseSelection :=
  ⟨union left.physicalFacts right.physicalFacts,
    union left.originalCopies right.originalCopies⟩

def lifecycleSelection (origins : Set OriginRecord) (physicalFacts : Set MemoryId)
    (scope : OriginalPublicationScope) : CopyEraseSelection :=
  combine (byPhysicalFacts physicalFacts) (byOriginalScope origins scope)

def eraseCopiedPayloads {Payload Configuration : Type}
    (state : HostState Payload Configuration) (selection : CopyEraseSelection) :
    HostState Payload Configuration :=
  { state with copiedPayloads := fun row =>
      row ∈ state.copiedPayloads ∧
        row.locator.fact ∉ selection.physicalFacts ∧
        row.locator ∉ selection.originalCopies }

theorem physical_selection_crosses_original_owners
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (row : CopiedPayload Payload) (physicalFacts : Set MemoryId)
    (selected : row.locator.fact ∈ physicalFacts) :
    row ∉ (eraseCopiedPayloads state (byPhysicalFacts physicalFacts)).copiedPayloads := by
  intro remains
  exact remains.2.1 selected

theorem exact_physical_fact_removes_its_copies_globally
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (row : CopiedPayload Payload) (fact : MemoryId)
    (selected : row.locator.fact = fact) :
    row ∉ (eraseCopiedPayloads state (exactPhysicalFact fact)).copiedPayloads := by
  apply physical_selection_crosses_original_owners
  exact selected

theorem exact_physical_fact_preserves_other_fact
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (row : CopiedPayload Payload) (fact : MemoryId)
    (present : row ∈ state.copiedPayloads)
    (unrelated : row.locator.fact ≠ fact) :
    row ∈ (eraseCopiedPayloads state (exactPhysicalFact fact)).copiedPayloads := by
  refine ⟨present, ?_, ?_⟩
  · exact unrelated
  · exact fun impossible => impossible

theorem exact_fact_selection_has_no_original_scope
    (fact : MemoryId) : (exactPhysicalFact fact).originalCopies = noCopies := rfl

theorem source_copy_locators_are_exact
    (origins : Set OriginRecord) (owner : OwnerRef) (source : SourceId)
    (locator : FactCopyLocator) :
    locator ∈ originalCopyLocators origins (.source owner source) ↔
      locator.originalOwner = owner ∧
        hasOriginalSourceOrigin origins owner source locator.fact := Iff.rfl

theorem mapped_source_copy_locators_are_exact
    (origins : Set OriginRecord) (owner : OwnerRef) (source : SourceScopeId)
    (locator : FactCopyLocator) :
    locator ∈ originalCopyLocators origins
        (.source owner (publicationSourceForScope source)) ↔
      locator.originalOwner = owner ∧
        hasOriginalSourceOrigin origins owner (SourceId.ofToken source.token)
          locator.fact := by
  rfl

theorem owner_copy_locators_are_exact
    (origins : Set OriginRecord) (owner : OwnerRef) (locator : FactCopyLocator) :
    locator ∈ originalCopyLocators origins (.owner owner) ↔
      locator.originalOwner = owner ∧ hasOriginalOwnerOrigin origins owner locator.fact :=
  Iff.rfl

theorem selected_source_copy_is_removed
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (origins : Set OriginRecord) (row : CopiedPayload Payload)
    (owner : OwnerRef) (source : SourceId)
    (selected : row.locator ∈ originalCopyLocators origins (.source owner source)) :
    row ∉ (eraseCopiedPayloads state
      (byOriginalScope origins (.source owner source))).copiedPayloads := by
  intro remains
  exact remains.2.2 selected

theorem selected_owner_copy_is_removed
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (origins : Set OriginRecord) (row : CopiedPayload Payload)
    (owner : OwnerRef)
    (selected : row.locator ∈ originalCopyLocators origins (.owner owner)) :
    row ∉ (eraseCopiedPayloads state
      (byOriginalScope origins (.owner owner))).copiedPayloads := by
  intro remains
  exact remains.2.2 selected

theorem lifecycle_selection_removes_physical_copy
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (origins : Set OriginRecord) (physicalFacts : Set MemoryId)
    (scope : OriginalPublicationScope) (row : CopiedPayload Payload)
    (selected : row.locator.fact ∈ physicalFacts) :
    row ∉ (eraseCopiedPayloads state
      (lifecycleSelection origins physicalFacts scope)).copiedPayloads := by
  intro remains
  have notSelected := remains.2.1
  change ¬ (row.locator.fact ∈ physicalFacts ∨ row.locator.fact ∈ noFacts) at notSelected
  exact notSelected (Or.inl selected)

theorem lifecycle_selection_removes_original_scope_copy
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (origins : Set OriginRecord) (physicalFacts : Set MemoryId)
    (scope : OriginalPublicationScope) (row : CopiedPayload Payload)
    (selected : row.locator ∈ originalCopyLocators origins scope) :
    row ∉ (eraseCopiedPayloads state
      (lifecycleSelection origins physicalFacts scope)).copiedPayloads := by
  intro remains
  have notSelected := remains.2.2
  change ¬ (row.locator ∈ noCopies ∨
    row.locator ∈ originalCopyLocators origins scope) at notSelected
  exact notSelected (Or.inr selected)

theorem lifecycle_selection_contains_intersection
    {Payload : Type} (origins : Set OriginRecord) (physicalFacts : Set MemoryId)
    (scope : OriginalPublicationScope) (row : CopiedPayload Payload)
    (physicalSelected : row.locator.fact ∈ physicalFacts)
    (originalSelected : row.locator ∈ originalCopyLocators origins scope) :
    row.locator.fact ∈ (lifecycleSelection origins physicalFacts scope).physicalFacts ∧
      row.locator ∈ (lifecycleSelection origins physicalFacts scope).originalCopies := by
  constructor
  · change row.locator.fact ∈ physicalFacts ∨ row.locator.fact ∈ noFacts
    exact Or.inl physicalSelected
  · change row.locator ∈ noCopies ∨ row.locator ∈ originalCopyLocators origins scope
    exact Or.inr originalSelected

theorem lifecycle_selection_removes_intersection_copy
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (origins : Set OriginRecord) (physicalFacts : Set MemoryId)
    (scope : OriginalPublicationScope) (row : CopiedPayload Payload)
    (physicalSelected : row.locator.fact ∈ physicalFacts)
    (originalSelected : row.locator ∈ originalCopyLocators origins scope) :
    row ∉ (eraseCopiedPayloads state
      (lifecycleSelection origins physicalFacts scope)).copiedPayloads := by
  have selectedByBoth := lifecycle_selection_contains_intersection origins physicalFacts
    scope row physicalSelected originalSelected
  intro remains
  have notPhysicalSelected := remains.2.1
  change ¬ (row.locator.fact ∈ physicalFacts ∨ row.locator.fact ∈ noFacts) at notPhysicalSelected
  exact notPhysicalSelected selectedByBoth.1

theorem combined_selection_preserves_unselected_copy
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (origins : Set OriginRecord) (physicalFacts : Set MemoryId)
    (scope : OriginalPublicationScope) (row : CopiedPayload Payload)
    (present : row ∈ state.copiedPayloads)
    (notPhysicalSelected : row.locator.fact ∉ physicalFacts)
    (notOriginalSelected : row.locator ∉ originalCopyLocators origins scope) :
    row ∈ (eraseCopiedPayloads state
      (lifecycleSelection origins physicalFacts scope)).copiedPayloads := by
  refine ⟨present, ?_, ?_⟩
  · change ¬ (row.locator.fact ∈ physicalFacts ∨ row.locator.fact ∈ noFacts)
    intro selected
    rcases selected with selected | selected
    · exact notPhysicalSelected selected
    · exact selected
  · change ¬ (row.locator ∈ noCopies ∨
      row.locator ∈ originalCopyLocators origins scope)
    intro selected
    rcases selected with selected | selected
    · exact selected
    · exact notOriginalSelected selected

theorem owner_scope_selects_source_free_origin
    (origins : Set OriginRecord) (owner : OwnerRef)
    (record : OriginRecord) (present : record ∈ origins)
    (sameOwner : record.metadata.originalOwner = owner)
    (sourceAbsent : record.metadata.source = none)
    (locator : FactCopyLocator) (sameFact : locator.fact = record.fact)
    (locatorOwner : locator.originalOwner = owner) :
    record.metadata.source = none ∧ locator ∈ originalCopyLocators origins (.owner owner) := by
  constructor
  · exact sourceAbsent
  · apply (owner_copy_locators_are_exact origins owner locator).2
    refine ⟨locatorOwner, ?_⟩
    exact ⟨record, present, sameFact.symm, sameOwner⟩

theorem original_owner_source_scope_preserves_unselected_copy
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (origins : Set OriginRecord) (row : CopiedPayload Payload)
    (owner : OwnerRef) (source : SourceId)
    (present : row ∈ state.copiedPayloads)
    (unselected : row.locator ∉ originalCopyLocators origins (.source owner source)) :
    row ∈ (eraseCopiedPayloads state
      (byOriginalScope origins (.source owner source))).copiedPayloads := by
  refine ⟨present, ?_, unselected⟩
  intro impossible
  exact impossible

theorem source_scope_does_not_select_known_absent_source
    (origins : Set OriginRecord) (owner : OwnerRef) (source : SourceId)
    (record : OriginRecord) (present : record ∈ origins)
    (unique : OriginKeyUnique origins)
    (sourceAbsent : record.metadata.source = none)
    (locator : FactCopyLocator) (sameFact : locator.fact = record.fact) :
    locator ∉ originalCopyLocators origins (.source owner source) := by
  intro selected
  have matching := selected.2
  rcases matching with ⟨sourceRecord, sourcePresent, sourceFact, sourceOwner, sourceSome⟩
  have sameRecord := unique sourceRecord record sourcePresent present
    (sourceFact.trans sameFact)
  subst sourceRecord
  rw [sourceAbsent] at sourceSome
  cases sourceSome

theorem inverse_preserves_authored_configuration
    {Payload Configuration : Type} (state : HostState Payload Configuration)
    (selection : CopyEraseSelection) :
    (eraseCopiedPayloads state selection).authoredConfiguration =
      state.authoredConfiguration := rfl

end Causa.HostStateFactCopyErase
