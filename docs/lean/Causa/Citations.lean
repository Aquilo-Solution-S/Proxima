/-
Causa — Citations

Citation is `blob_id` 0..1 on the Memory row (UML §4). No mapping table.
Subject is Fact ∪ Abstraction. Perspectives never cite.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory

namespace Causa

structure Blob where
  id     : BlobId
  owner  : Owner
  schema : SchemaRef

def blob_id : Blob → BlobId := Blob.id
def blob_owner : Blob → Owner := Blob.owner
def blob_schema : Blob → SchemaRef := Blob.schema

def BlobIdUnique (objects : Set Blob) : Prop :=
  ∀ b1 b2 : Blob,
    b1 ∈ objects →
    b2 ∈ objects →
    blob_id b1 = blob_id b2 →
    b1 = b2

instance : Immutable Blob := ⟨⟩
instance : AppendOnly Blob := ⟨⟩

def Citable : Type := { m : Memory // memory_kind m = .Fact ∨ memory_kind m = .Abstraction }

def Citable.memory (c : Citable) : Memory := c.val

def Fact.citable (f : Fact) : Citable := ⟨f.val, Or.inl f.property⟩

/-- The Memory cites this blob. Multiplicity 0..1 is the Option itself. -/
def memory_cites (m : Memory) (b : Blob) : Prop :=
  memory_blob_id m = some (blob_id b) ∧ memory_owner m = blob_owner b

theorem citation_subject_is_citable :
    ∀ (m : Memory) (b : Blob),
      memory_cites m b →
      memory_kind m = .Fact ∨ memory_kind m = .Abstraction := by
  intro m b h
  have hsome : memory_blob_id m ≠ none := by
    rw [h.1]
    exact Option.some_ne_none _
  exact m.blob_fa_only hsome

theorem citation_perspective_never_cites :
    ∀ m : Memory, memory_kind m = .Perspective → memory_blob_id m = none := by
  intro m hk
  exact m.perspective_never_cites hk

theorem citation_implies_citable :
    ∀ (m : Memory),
      (memory_blob_id m).isSome = true →
        memory_kind m = .Fact ∨ memory_kind m = .Abstraction := by
  intro m h
  have hne : memory_blob_id m ≠ none := by
    intro hnone
    rw [hnone] at h
    exact Bool.noConfusion h
  exact m.blob_fa_only hne

theorem citation_pointer_never_on_perspective :
    ∀ (m : Memory),
      memory_kind m = .Perspective → (memory_blob_id m).isSome = true → False := by
  intro m hkind hsome
  have hnone : memory_blob_id m = none := m.perspective_never_cites hkind
  rw [hnone] at hsome
  exact Bool.noConfusion hsome

theorem citation_owner_match :
    ∀ (m : Memory) (b : Blob),
      memory_cites m b → memory_owner m = blob_owner b := by
  intro _ _ h
  exact h.2

/-- 0..1 is structural: one Option field, not a mapping table. -/
theorem citation_unique_per_subject (m : Memory) (b1 b2 : Blob) :
    memory_cites m b1 → memory_cites m b2 → blob_id b1 = blob_id b2 := by
  intro h1 h2
  have : some (blob_id b1) = some (blob_id b2) := h1.1.symm.trans h2.1
  injection this

end Causa
