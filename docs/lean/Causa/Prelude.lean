/-
Causa — Prelude

Kernel-wide primitives with no dependencies of their own:
- The minimal `Set` definition (avoids Mathlib).
- A logical insertion-time tick (`Instant := Nat`).
- Opaque value Types whose representation is deferred but whose
  identity earns a kernel slot (`Text`).
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

/-- Free text attached to a memory row. Opaque: the kernel commits
    to the type slot, not its encoding or kind-based presence. Facts,
    Abstractions, and Perspectives may all carry optional text;
    flavor sidecars may carry additional opaque typed payload. -/
axiom Text : Type

end Causa
