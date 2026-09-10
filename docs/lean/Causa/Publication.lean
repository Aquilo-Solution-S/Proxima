/-
Causa — Publication (the Fact outbox, doc 18)

A listenable Fact's admission yields ONE publication record keyed by the Fact's
own `t`. The CAPTURED half never changes; only DELIVERY moves. No step removes
a record — removal is owner erasure (doc 13).

What this module carries:

  G1  set-shape ASSERTED `OutboxValid` (+ `RecordIdUnique`), with the
      projections `capture_iff_listenable` and `replay_no_second_record`.
  G2  the captured event is immutable: `step_preserves_capture`.
  G3  a closed delivery vocabulary (`Delivery`) and a closed admitted
      transition relation (`Step`), with `no_step_deletes`,
      `expiry_preserves_record`, `erasure_is_the_only_removal`.
  G4  a record carries the Fact's owner and no second read path:
      `owner_follows_fact`.
  G5  at-least-once SAFETY only: `duplicates_share_identity`.

DELIBERATELY EXCLUDED — do not read an absence here as a claim:

  * TRANSACTIONALITY. "The Fact and its record commit together or not at all"
    is a storage-layer contract. The kernel models a set of rows: no partial
    state, no write ordering, no transaction. Same stance as CN-9 / CI-18 /
    CI-20 / ST-SCOPE. Carried by the storage-pg fault-injection tests.
  * LIVENESS. "Every pending record is eventually published" needs a run, a
    schedule, or a temporal operator. The kernel has no trace vocabulary at
    all, so there is nothing to state it in. Carried by the publisher-recovery
    tests. Only the safety half of at-least-once is proved here.
-/

import Causa.Flavor

namespace Causa.Publication

open Causa

/-- Build-time listen registration. The kernel never inspects a schema; a
    flavor's listenable vocabulary is a PARAMETER, never a kernel table
    (same stance as `Causa.Flavor`). -/
structure ListenRegistry where
  listenable : SchemaRef → Prop

/-- Listenability is a SERIES contract, read off `MemoryHead` — `schema` lives
    only on the head, never on the version row. -/
def factListenable (heads : Set MemoryHead) (reg : ListenRegistry) (m : Memory) : Prop :=
  ∃ h : MemoryHead, h ∈ heads ∧ memory_head_handle h = memory_handle m ∧
    reg.listenable (memory_head_schema h)

/-- G2 — the captured event. The publisher transmits THIS, never a
    re-derivation from today's Fact state or today's flavor version. -/
structure Captured where
  t        : MemoryId
  handle   : Handle
  schema   : SchemaRef
  producer : User
  owner    : Owner
  content  : ContentHash
  tick     : Instant

instance : Immutable Captured := ⟨⟩

/-- G3 — closed delivery vocabulary. A claim carries its lease deadline INSIDE
    the constructor, so `Record` needs no `claimed_has_lease` proof field and
    `{ r with delivery := d }` stays a plain record update. -/
inductive Delivery where
  | pending
  | claimed (lease : Instant)
  | published
  deriving Repr

structure Record where
  captured : Captured
  delivery : Delivery

def record_t (r : Record) : MemoryId := r.captured.t
def record_owner (r : Record) : Owner := r.captured.owner
def record_delivery (r : Record) : Delivery := r.delivery

/-- Record identity IS the Fact's `t` — not the series handle, not a receipt,
    not a content hash. -/
def RecordIdUnique (rs : Set Record) : Prop :=
  ∀ r1 r2 : Record, r1 ∈ rs → r2 ∈ rs → record_t r1 = record_t r2 → r1 = r2

/-- Capture: identity is the Fact's `t`, owner is the Fact's owner, tick is the
    Fact's original recording time. -/
def capture (f : Fact) (schema : SchemaRef) (producer : User) (content : ContentHash) :
    Record where
  captured :=
    { t := memory_t f.memory, handle := memory_handle f.memory, schema := schema
      producer := producer, owner := memory_owner f.memory
      content := content, tick := memory_tick f.memory }
  delivery := .pending

/-- G3 — the admitted steps. Nothing leaves `.published`; nothing deletes.
    `Step` relates `Delivery`, not `Record`, so `advance` is the only way state
    moves: "no transition deletes" holds by construction. -/
inductive Step : Delivery → Delivery → Prop where
  | claim   (lease : Instant) : Step .pending (.claimed lease)
  | ack     (lease : Instant) : Step (.claimed lease) .published
  | release (lease : Instant) : Step (.claimed lease) .pending
  | expire  (lease : Instant) : Step (.claimed lease) .pending

def advance (r : Record) (d : Delivery) : Record := { r with delivery := d }

def outboxStep (rs : Set Record) (r : Record) (d : Delivery) : Set Record :=
  fun x => (x ∈ rs ∧ record_t x ≠ record_t r) ∨ x = advance r d

/-- The ONLY removal: compliance erasure of an abandoned owner (doc 13). Not
    `wipeable` — a publication record has no cooled/unreferenced analogue, so
    that disjunct has nothing to bind. -/
def outboxErase (rs : Set Record) (o : Owner) : Set Record :=
  fun r => r ∈ rs ∧ ¬ (record_owner r = o ∧ abandoned o)

/-- G1 — ASSERTED table validity, in the style of `MemoryGraphValid` /
    `ContentAligned`. The kernel cannot observe a write, so capture totality,
    soundness and faithfulness are stated as fields, not proved. -/
structure OutboxValid
    (memories : Set Memory) (heads : Set MemoryHead)
    (reg : ListenRegistry) (rs : Set Record) : Prop where
  idsUnique : RecordIdUnique rs
  captureTotal : ∀ m : Memory, m ∈ memories → memory_kind m = .Fact →
    factListenable heads reg m → ∃ r : Record, r ∈ rs ∧ record_t r = memory_t m
  captureSound : ∀ r : Record, r ∈ rs → ∃ m : Memory, m ∈ memories ∧
    memory_kind m = .Fact ∧ factListenable heads reg m ∧ memory_t m = record_t r
  captureFaithful : ∀ (r : Record) (m : Memory), r ∈ rs → m ∈ memories →
    memory_t m = record_t r →
      record_owner r = memory_owner m ∧ r.captured.tick = memory_tick m

theorem record_id_is_fact_t (f : Fact) (s : SchemaRef) (p : User) (c : ContentHash) :
    record_t (capture f s p c) = memory_t f.memory := rfl

/-- G4 — reading a record is exactly reading its Fact. The outbox opens no
    second, cross-owner read path. -/
theorem owner_follows_fact
    (u : User) (f : Fact) (s : SchemaRef) (p : User) (c : ContentHash) :
    record_owner (capture f s p c) = memory_owner f.memory ∧
      (may_read u (record_owner (capture f s p c)) .fact ↔
        may_read u (memory_owner f.memory) .fact) := ⟨rfl, Iff.rfl⟩

/-- G2 — every delivery step preserves the captured event exactly. -/
theorem step_preserves_capture (r : Record) (d : Delivery) :
    (advance r d).captured = r.captured := rfl

/-- G3 — no admitted step removes the record: same `t`, same bytes. -/
theorem no_step_deletes (rs : Set Record) (r : Record) (d : Delivery) :
    ∃ x : Record, x ∈ outboxStep rs r d ∧ record_t x = record_t r ∧
      x.captured = r.captured :=
  ⟨advance r d, Or.inr rfl, rfl, rfl⟩

/-- G3 — lease expiry returns to `pending` with bytes intact: expiry permits a
    retry, never a loss. -/
theorem expiry_preserves_record (r : Record) (lease : Instant)
    (h : record_delivery r = .claimed lease) :
    Step (record_delivery r) .pending ∧ (advance r .pending).captured = r.captured := by
  rw [h]; exact ⟨Step.expire lease, rfl⟩

/-- G1 — an admitted Fact has a record iff it is listenable. -/
theorem capture_iff_listenable
    (memories : Set Memory) (heads : Set MemoryHead)
    (reg : ListenRegistry) (rs : Set Record)
    (hvalid : OutboxValid memories heads reg rs) (huniq : MemoryIdUnique memories)
    (m : Memory) (hm : m ∈ memories) (hk : memory_kind m = .Fact) :
    (∃ r : Record, r ∈ rs ∧ record_t r = memory_t m) ↔ factListenable heads reg m := by
  refine ⟨fun ⟨r, hr, hrt⟩ => ?_, hvalid.captureTotal m hm hk⟩
  obtain ⟨m', hm', _, hlisten, ht⟩ := hvalid.captureSound r hr
  exact (huniq m' m hm' hm (by rw [ht, hrt])) ▸ hlisten

/-- G1 — replay keeps the same `t`, so it names the SAME record; a genuinely
    new Fact gets its own. -/
theorem replay_no_second_record
    (memories : Set Memory) (heads : Set MemoryHead)
    (reg : ListenRegistry) (rs : Set Record)
    (hvalid : OutboxValid memories heads reg rs)
    (r1 r2 : Record) (h1 : r1 ∈ rs) (h2 : r2 ∈ rs)
    (hsame : record_t r1 = record_t r2) : r1 = r2 :=
  hvalid.idsUnique r1 r2 h1 h2 hsame

/-- G5 (safety half only) — a redelivery is keyed by `t` and identical in
    bytes, so a consumer can deduplicate on durable identity. Liveness is NOT
    claimed: see the exclusion in the header. -/
theorem duplicates_share_identity (r : Record) (d₁ d₂ : Delivery) :
    record_t (advance r d₁) = record_t (advance r d₂) ∧
      (advance r d₁).captured = (advance r d₂).captured := ⟨rfl, rfl⟩

/-- G3 — a record survives an erase sweep unless its OWN owner is abandoned. -/
theorem erasure_is_the_only_removal
    (rs : Set Record) (o : Owner) (r : Record) (hr : r ∈ rs)
    (hkeep : ¬ (record_owner r = o ∧ abandoned o)) : r ∈ outboxErase rs o :=
  ⟨hr, hkeep⟩

#print axioms record_id_is_fact_t
#print axioms owner_follows_fact
#print axioms step_preserves_capture
#print axioms no_step_deletes
#print axioms expiry_preserves_record
#print axioms capture_iff_listenable
#print axioms replay_no_second_record
#print axioms duplicates_share_identity
#print axioms erasure_is_the_only_removal

end Causa.Publication
