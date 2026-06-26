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

/-- A principal can access an Owner-space. Mirrors the engine
    `Identity::can_access_principal`: a user reaches their own space and any
    group they belong to (`visible`); a group subject reaches its own space.
    Host membership resolution may widen the group case app-side; the kernel
    commits to the self-access floor. -/
def principal_can_access (subject : Principal) (o : Owner) : Prop :=
  match subject with
  | .user u  => visible o u
  | .group g => owner_principal o = .group g

/-- A grant — for ANY subject principal, user or group — can only be minted
    for an Owner that subject can access. This keeps RBAC above, not instead
    of, the Owner visibility rule. The engine enforces it for both principal
    kinds via `can_access_principal`; the kernel must too, so a maintainer
    cannot read AUTH-2 as user-only and drop the group-subject check. -/
axiom owner_space_grant_owner_visible :
  ∀ (subject : Principal) (o : Owner) (a : MemoryAction),
    owner_space_grant subject o a → principal_can_access subject o

/-- Admin is not a read/write shortcut in the kernel; engines may map admin
    to administrative operations only unless a concrete verb also checks read
    or write. -/
def admin_is_separate_action : String :=
  "memory.admin does not imply memory.read or memory.write by definition"

end Causa
