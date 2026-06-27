/-
Causa — Prelude

Kernel-wide primitives with no dependencies of their own:
- The minimal `Set` definition (avoids Mathlib).
- A logical insertion-time tick (`Instant := Nat`).
- Kernel value aliases (`Instant`, `Text`) backed by Lean core types.
- Forward-referenced Types needed across domain files.

Every file in `Causa/` imports this module. Nothing else
imports anything heavier here — keep the prelude clean.

This kernel is the source of truth for Proxima's domainless
invariants. The prose docs (`docs/*.md`) are rationale and
commentary; where they disagree with the kernel, the kernel wins
until renegotiated in writing.
-/

namespace Causa

-- ============================================================
-- Minimal Set primitive (avoids a Mathlib dependency).
-- Equivalent to Mathlib's `Set α := α → Prop`.
-- ============================================================

def Set (α : Type) : Type := α → Prop

instance {α : Type} : Membership α (Set α) := ⟨fun s a => s a⟩

-- ============================================================
-- Kernel value Types
-- ============================================================

/-- Logical insertion-time tick. Runtime wall-clock/storage timestamps
    may be richer; the kernel only needs ordered row time, so `Nat`'s
    ordinary `≤` supplies the ordering without an extra trusted axiom. -/
abbrev Instant : Type := Nat

/-- Free text attached to memory and goal rows. The kernel text type is
    ordinary Lean `String`; storage encoding, normalization, rendering,
    and flavor sidecar payloads remain engine/flavor concerns. -/
abbrev Text : Type := String

end Causa
