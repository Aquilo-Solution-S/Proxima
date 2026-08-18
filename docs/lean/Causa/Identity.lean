/-
Causa — Identity

ID types and the append-only storage discipline (doc 07; v0.0.8 timeseries).

Identity (UML §0):
  series  = handle
  version = t            uuidv7 UNIQUE     -- this is the row id
  row     = (handle, t)
  head    = memory_head.t / goal_head.t    -- DISPLAY / SEARCH ONLY
  link    = target t, frozen at write      -- NEVER a handle, NEVER follow-at-read

There is no `FactEntity`, no `EdgeId`, no `CitationMappingId`. Citation is
`blob_id` 0..1 on the Memory row. Schema lives on the handle catalog, not on
every version. Typed payload is `Content` (owner-scoped); many admissions
may share one `ContentId`. The kernel's time axis is `tick : Instant`, the
model of uuidv7 `t` order — not a second storage column named `created_at`.
-/

import Causa.Prelude
import Causa.Owner

namespace Causa

-- ============================================================
-- Identity — ONE opaque token (doc 07 §ID Types)
-- ============================================================

/-- The one identity primitive: a fresh, unique, engine-minted token (UUIDv7).
    EQUALITY is all the kernel asks of it. Layout and v7 time-sortability are
    engine commitments. The kernel's time axis is `Instant` (the order of `t`),
    never a second wall-clock field on the row. -/
opaque Id : Type := String

/-- Version id — Memory `t` / Goal `t`. Globally unique. The row id. -/
abbrev MemoryId          : Type := Id
abbrev GoalId            : Type := Id
/-- Series id — one real-world thing / one judgment / one goal. -/
abbrev Handle            : Type := Id
abbrev WakeId            : Type := Id
abbrev BlobId            : Type := Id
/-- Typed sidecar payload. Not an admission. Shareable within one owner. -/
abbrev ContentId         : Type := Id
/-- Engine digest of a Content payload. Opaque equality only. -/
opaque ContentHash : Type := String
/-- Kernel face of a retrieval situation: admitted `t`s in scope. -/
abbrev Cue : Type := Set MemoryId

/-- Stable persisted owner-table reference. This is not the resolved membership
    map (`Owner := User → Option Role`). Entity rows should store this reference;
    the host resolves it through `OwnerState` before authorization. No new axiom:
    group refs reuse the existing engine-minted `Id`. -/
inductive OwnerRef where
  | world
  | personal (u : User)
  | group (id : Id)

/-- The resolved Leopard-style owner map used by pure authorization rules. -/
abbrev ResolvedOwner : Type := Owner

/-- Server/host owner state: the trusted boundary that resolves stable owner refs
    into current role maps. The kernel does not model the Leopard expansion
    algorithm, SQL tables, or caches; it consumes only this resolved function. -/
structure OwnerState where
  resolve : OwnerRef → ResolvedOwner
  world_resolves : resolve .world = world
  personal_resolves : ∀ u : User, resolve (.personal u) = Owner.ofUser u

/-- Which operator produced a derived memory. Reproducibility metadata stays
    engine/sidecar — not a Memory column. -/
opaque OperatorId : Type := String

/-- The F→A input contract token. Opaque equality only. Not a Memory column. -/
opaque InputContract : Type := String

/-- The opaque schema tag. Stored once on `memory_head` / `goal_head` / `blob`,
    never on every version row. New shape → new `SchemaRef` → new handle.
    No `schema_version`. -/
opaque SchemaRef : Type := String

-- ============================================================
-- Lifecycle capability classes (doc 07 §Append-Only)
-- ============================================================

/-- ST-11 — no UPDATE in the cognitive lifecycle (except `wake_config`,
    which is not a cognitive row). Marker class. -/
class AppendOnly (α : Type) : Prop

/-- Never updated in place. Facts, blobs. Forget is cool-to-S3, not UPDATE. -/
class Immutable (α : Type) : Prop

/- ST-15..17 — vector-store independence is carried by ABSENCE: the kernel
   declares no `Embedding` entity and no `Memory → Embedding` accessor. -/

end Causa
