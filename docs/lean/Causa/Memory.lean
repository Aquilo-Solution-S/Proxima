/-
Causa — Memory

The F/A/P cognitive graph as a timeseries (UML §0–§2).

  series  = handle
  version = t            -- MemoryId; globally UNIQUE
  row     = (handle, t)

No `supersedes` / `text` / `authoring_*` / schema-on-version / `created_at`.
Pins (`origins`, `refs`) are target `t`, frozen at write. Schema and series
contract live on `MemoryHead`. There is no `FactEntity`.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity

namespace Causa

-- ============================================================
-- Kinds and layers (doc 02 §The Layering Principle)
-- ============================================================

inductive MemoryKind where
  | Fact
  | Abstraction
  | Perspective
  deriving DecidableEq, Repr

/-- ℓ(F)=0, ℓ(A)=1, ℓ(P)=2 — doc 02, verbatim. -/
def MemoryKind.layer : MemoryKind → Nat
  | .Fact        => 0
  | .Abstraction => 1
  | .Perspective => 2

-- ============================================================
-- The core entity — one version of a series
-- ============================================================

/-- One Memory version. `t` is the row id. `tick` is the kernel face of
    uuidv7 `t` order (not a storage `created_at` column). Schema is not here. -/
structure Memory where
  handle  : Handle
  t       : MemoryId
  kind    : MemoryKind
  owner   : Owner
  origins : List MemoryId
  refs    : List MemoryId
  blob_id : Option BlobId
  tick    : Instant
  /-- Fact origins are empty (UML §2). -/
  fact_origins_empty : kind = .Fact → origins = []
  /-- A Perspective never cites (UML §4). -/
  perspective_never_cites : kind = .Perspective → blob_id = none
  /-- `blob_id` is F/A only. -/
  blob_fa_only : blob_id ≠ none → kind = .Fact ∨ kind = .Abstraction

def memory_handle : Memory → Handle := Memory.handle
def memory_t : Memory → MemoryId := Memory.t
/-- Compatibility: the row id is `t`. -/
def memory_id : Memory → MemoryId := Memory.t
def memory_kind : Memory → MemoryKind := Memory.kind
def memory_owner : Memory → Owner := Memory.owner
def memory_origins : Memory → List MemoryId := Memory.origins
def memory_refs : Memory → List MemoryId := Memory.refs
def memory_blob_id : Memory → Option BlobId := Memory.blob_id
def memory_tick : Memory → Instant := Memory.tick

theorem memory_field_projection
    (handle : Handle) (t : MemoryId) (kind : MemoryKind) (owner : Owner)
    (origins refs : List MemoryId) (blob_id : Option BlobId) (tick : Instant)
    (fact_origins_empty : kind = .Fact → origins = [])
    (perspective_never_cites : kind = .Perspective → blob_id = none)
    (blob_fa_only : blob_id ≠ none → kind = .Fact ∨ kind = .Abstraction) :
    memory_t
      (Memory.mk handle t kind owner origins refs blob_id tick
        fact_origins_empty perspective_never_cites blob_fa_only) = t := rfl

/-- `t` uniqueness is a table/store invariant. -/
def MemoryIdUnique (memories : Set Memory) : Prop :=
  ∀ m1 m2 : Memory,
    m1 ∈ memories →
    m2 ∈ memories →
    memory_t m1 = memory_t m2 →
    m1 = m2

instance : AppendOnly Memory := ⟨⟩

-- ============================================================
-- Facts
-- ============================================================

def Fact : Type := { m : Memory // memory_kind m = .Fact }

def Fact.memory (f : Fact) : Memory := f.val

theorem fact_memory_kind (f : Fact) : memory_kind f.memory = .Fact := f.property

/-- ME-4 replacement — a Fact declares no origins. -/
theorem fact_origins_nothing (f : Fact) : memory_origins f.memory = [] :=
  f.val.fact_origins_empty f.property

-- ============================================================
-- Series catalog (UML §2b)
-- ============================================================

/-- Required catalog. `(kind, schema, owner)` are the series contract and
    live only here. `t` is the current head (display / search). -/
structure MemoryHead where
  handle : Handle
  kind   : MemoryKind
  schema : SchemaRef
  owner  : Owner
  t      : MemoryId

def memory_head_handle : MemoryHead → Handle := MemoryHead.handle
def memory_head_kind : MemoryHead → MemoryKind := MemoryHead.kind
def memory_head_schema : MemoryHead → SchemaRef := MemoryHead.schema
def memory_head_owner : MemoryHead → Owner := MemoryHead.owner
def memory_head_t : MemoryHead → MemoryId := MemoryHead.t

def MemoryHeadHandleUnique (heads : Set MemoryHead) : Prop :=
  ∀ h1 h2 : MemoryHead,
    h1 ∈ heads →
    h2 ∈ heads →
    memory_head_handle h1 = memory_head_handle h2 →
    h1 = h2

/-- Every version of a handle agrees with the catalog on kind and owner. -/
def MemoryHeadAligned (memories : Set Memory) (heads : Set MemoryHead) : Prop :=
  ∀ (m : Memory) (h : MemoryHead),
    m ∈ memories →
    h ∈ heads →
    memory_handle m = memory_head_handle h →
      memory_kind m = memory_head_kind h ∧
      memory_owner m = memory_head_owner h

/-- A Memory lifecycle head: the catalog names this `t` for the handle. -/
def memoryIsHead (memories : Set Memory) (heads : Set MemoryHead) (m : Memory) : Prop :=
  m ∈ memories ∧
  ∃ h : MemoryHead, h ∈ heads ∧
    memory_head_handle h = memory_handle m ∧
    memory_head_t h = memory_t m

def memoryHeads (memories : Set Memory) (heads : Set MemoryHead) : Set Memory :=
  fun m => memoryIsHead memories heads m

def perspectiveHeads (memories : Set Memory) (heads : Set MemoryHead) : Set Memory :=
  fun m => memory_kind m = .Perspective ∧ memoryIsHead memories heads m

theorem perspective_head_is_perspective :
    ∀ (memories : Set Memory) (heads : Set MemoryHead) (m : Memory),
      m ∈ perspectiveHeads memories heads → memory_kind m = .Perspective := by
  intro _ _ _ h
  exact h.1

theorem perspective_head_is_memory_head :
    ∀ (memories : Set Memory) (heads : Set MemoryHead) (m : Memory),
      m ∈ perspectiveHeads memories heads → memoryIsHead memories heads m := by
  intro _ _ _ h
  exact h.2

-- ============================================================
-- Cold stub (UML §5c) — forget leaves this, not a tombstone flag
-- ============================================================

structure Cooled where
  t      : MemoryId
  handle : Handle
  owner  : Owner
  kind   : MemoryKind

def cooled_t : Cooled → MemoryId := Cooled.t
def cooled_handle : Cooled → Handle := Cooled.handle
def cooled_owner : Cooled → Owner := Cooled.owner
def cooled_kind : Cooled → MemoryKind := Cooled.kind

def CooledIdUnique (cooled : Set Cooled) : Prop :=
  ∀ c1 c2 : Cooled,
    c1 ∈ cooled →
    c2 ∈ cooled →
    cooled_t c1 = cooled_t c2 →
    c1 = c2

def pinExists (memories : Set Memory) (cooled : Set Cooled) (id : MemoryId) : Prop :=
  (∃ m : Memory, m ∈ memories ∧ memory_t m = id) ∨
  (∃ c : Cooled, c ∈ cooled ∧ cooled_t c = id)

/-- Kind of a pinned `t`: hot row or cooled stub. Forget must not break layering. -/
def pinKindIs
    (memories : Set Memory) (cooled : Set Cooled)
    (id : MemoryId) (k : MemoryKind) : Prop :=
  (∃ tgt : Memory, tgt ∈ memories ∧ memory_t tgt = id ∧ memory_kind tgt = k) ∨
  (∃ c : Cooled, c ∈ cooled ∧ cooled_t c = id ∧ cooled_kind c = k)

-- ============================================================
-- Origin kind CHECKs (UML §2) — Tesla valve, no extra if
-- ============================================================

structure OriginKindValid
    (memories : Set Memory) (cooled : Set Cooled) (m : Memory) : Prop where
  factEmpty : memory_kind m = .Fact → memory_origins m = []
  absFacts : memory_kind m = .Abstraction →
    ∀ id : MemoryId, id ∈ memory_origins m →
      pinKindIs memories cooled id .Fact
  perspAbsOrEmpty : memory_kind m = .Perspective →
    memory_origins m = [] ∨
    ∀ id : MemoryId, id ∈ memory_origins m →
      pinKindIs memories cooled id .Abstraction

/-- A Fact never originates from anything. THEOREM from the row field. -/
theorem facts_declare_no_origins (m : Memory) (hk : memory_kind m = .Fact) :
    memory_origins m = [] :=
  m.fact_origins_empty hk

-- ============================================================
-- Personality absence (doc 02 §Personality, D4)
-- ============================================================

/- Personality is not a kernel entity. No FactEntity. No supersedes. -/

end Causa
