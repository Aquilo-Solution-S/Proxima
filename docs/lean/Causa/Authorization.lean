/-
Causa — Authorization

Owner-space grants layer policy above Owner without changing Owner.
Core stores and compares Owner as Principal. Hosts resolve group membership
and role assignment; the kernel names only the granted action predicate the
engine must enforce.
-/

import Causa.Prelude
import Causa.Owner

namespace Causa

/-- Memory-space action vocabulary. Resource granularity is Owner. -/
inductive MemoryAction where
  | search
  | read
  | write
  | publish
  | admin
  deriving DecidableEq, Repr

/-- Host-resolved grant predicate. The kernel does not model group membership
    assignment or role bundles; those live before AuthzContext construction. -/
axiom owner_space_grant : Principal → Owner → MemoryAction → Prop

/-- Authorization gate for memory-space actions. -/
def may_memory_action (subject : Principal) (owner : Owner) (action : MemoryAction) : Prop :=
  owner_space_grant subject owner action

/-- A user grant can only be minted for an Owner visible to that user. This
    keeps RBAC above, not instead of, the Owner visibility rule. -/
axiom owner_space_grant_owner_visible :
  ∀ (u : UserId) (o : Owner) (a : MemoryAction),
    owner_space_grant (.user u) o a → visible o u

/-- Admin is not a read/write shortcut in the kernel; engines may map admin
    to administrative operations only unless a concrete verb also checks read
    or write. -/
def admin_is_separate_action : String :=
  "memory.admin does not imply memory.read or memory.write by definition"

end Causa
