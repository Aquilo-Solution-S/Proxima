/-
Causa — Pin authorization

Writer can read each pinned `t` (UML §2 `read`). No edge row. Source-owned
means the pin lives on the declaring Memory/Goal. Target render is a
separate gate (hot / Cold / Unavailable).
-/

import Causa.Edges
import Causa.Authorization

namespace Causa

def MemoryKind.accessKind : MemoryKind → AccessKind
  | .Fact => .fact
  | .Abstraction => .abstraction
  | .Perspective => .perspective

/-- Seeing the declaring row is source-local. -/
def pin_source_read_admitted (requester : User) (source : Memory) : Prop :=
  may_read requester (memory_owner source) (memory_kind source).accessKind

/-- Target projection: the pinned row may be shown only if readable. -/
def pin_target_readable (requester : User) (target : Memory) : Prop :=
  may_read requester (memory_owner target) (memory_kind target).accessKind

/-- Uniform write admission: source write + target read. -/
def pin_write_admitted (requester : User) (source target : Memory) : Prop :=
  may_write requester (memory_owner source) (memory_kind source).accessKind ∧
  may_read requester (memory_owner target) (memory_kind target).accessKind

theorem pin_write_admitted_source_write :
    ∀ requester source target,
      pin_write_admitted requester source target →
      may_write requester (memory_owner source) (memory_kind source).accessKind := by
  intro _ _ _ h
  exact h.1

theorem pin_write_admitted_target_read :
    ∀ requester source target,
      pin_write_admitted requester source target →
      may_read requester (memory_owner target) (memory_kind target).accessKind := by
  intro _ _ _ h
  exact h.2

/-- Cross-owner targets are admitted when readable. -/
theorem cross_owner_target_admitted :
    ∀ requester source target,
      pin_write_admitted requester source target →
      may_read requester (memory_owner target) (memory_kind target).accessKind :=
  pin_write_admitted_target_read

/-- Goal pin: writer writes the Goal owner, reads the named Memory. -/
def goal_pin_write_admitted (requester : User) (g : Goal) (target : Memory) : Prop :=
  may_write requester (goal_owner g) .goal ∧
  may_read requester (memory_owner target) (memory_kind target).accessKind

end Causa
