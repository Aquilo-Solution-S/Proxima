/-
Causa — Compliance

Hard delete is abandonment-only, plus the v0.0.8 cold path:

  wipeable := abandoned ∨ (cold ∧ unreferenced ∧ policy)

World is never abandoned. Forget cools; erase wipes abandoned owners.
There is no edge cascade — pins live on the declaring row.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Memory
import Causa.Authorization
import Causa.EdgeAuthorization

namespace Causa

def abandoned (o : Owner) : Prop := ∀ u : User, o u = none

theorem drop_personal_abandoned (u : User) :
    abandoned ((Owner.ofUser u).drop u) := by
  intro v
  by_cases h : v = u <;> simp [Group.drop, Owner.ofUser, h]

/-- No hot row still pins this `t`. -/
def unreferenced (memories : Set Memory) (id : MemoryId) : Prop :=
  ∀ m : Memory, m ∈ memories →
    id ∉ memory_origins m ∧ id ∉ memory_refs m

/-- No remaining admission names this Content. Cooled stubs do not keep
    payload identity; Content GC is admission-set emptiness. -/
def contentUnreferenced (memories : Set Memory) (id : ContentId) : Prop :=
  ∀ m : Memory, m ∈ memories → memory_content_id m ≠ some id

def contentWipeable (o : Owner) (memories : Set Memory) (id : ContentId) : Prop :=
  abandoned o ∨ contentUnreferenced memories id

theorem content_wipeable_when_abandoned
    (o : Owner) (memories : Set Memory) (id : ContentId) (h : abandoned o) :
    contentWipeable o memories id :=
  Or.inl h

theorem content_wipeable_when_unreferenced
    (o : Owner) (memories : Set Memory) (id : ContentId)
    (h : contentUnreferenced memories id) :
    contentWipeable o memories id :=
  Or.inr h

def cold (stubs : Set Cooled) (id : MemoryId) : Prop :=
  ∃ c : Cooled, c ∈ stubs ∧ cooled_t c = id

/-- CO-7 / ST-13 rebase: abandonment OR (cold ∧ unreferenced ∧ policy). -/
def wipeable (o : Owner) (memories : Set Memory) (stubs : Set Cooled)
    (id : MemoryId) (policy : Prop) : Prop :=
  abandoned o ∨ (cold stubs id ∧ unreferenced memories id ∧ policy)

theorem wipeable_when_abandoned
    (o : Owner) (memories : Set Memory) (stubs : Set Cooled)
    (id : MemoryId) (policy : Prop) (h : abandoned o) :
    wipeable o memories stubs id policy :=
  Or.inl h

theorem wipeable_when_cold_unreferenced_policy
    (o : Owner) (memories : Set Memory) (stubs : Set Cooled)
    (id : MemoryId) (policy : Prop)
    (hc : cold stubs id) (hu : unreferenced memories id) (hp : policy) :
    wipeable o memories stubs id policy :=
  Or.inr ⟨hc, hu, hp⟩

/-- A pin to a cooled `t` renders Cold, not a missing origin. -/
def pin_target_cold (stubs : Set Cooled) (id : MemoryId) : Prop :=
  cold stubs id

/-- Target render: hot / Cold / Unavailable (UML §5c). -/
def pin_render_hot (memories : Set Memory) (id : MemoryId) : Prop :=
  ∃ m : Memory, m ∈ memories ∧ memory_t m = id

def pin_render_cold (memories : Set Memory) (stubs : Set Cooled) (id : MemoryId) : Prop :=
  ¬ pin_render_hot memories id ∧ cold stubs id

def pin_render_unavailable (memories : Set Memory) (stubs : Set Cooled) (id : MemoryId) : Prop :=
  ¬ pin_render_hot memories id ∧ ¬ cold stubs id

/-- Forget of a pinned `t` does not delete the pinning row. -/
theorem pin_survives_target_cool
    (memories : Set Memory) (stubs : Set Cooled) (m : Memory) (id : MemoryId)
    (hm : m ∈ memories)
    (_hpin : id ∈ memory_origins m ∨ id ∈ memory_refs m)
    (hc : cold stubs id) :
    pinExists memories stubs id ∧ m ∈ memories :=
  ⟨Or.inr hc, hm⟩

theorem world_never_abandoned (u : User) : ¬ abandoned world := by
  intro h
  have hu := h u
  simp [world] at hu

end Causa
