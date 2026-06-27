/-
Causa — Prelude

Kernel-wide primitives with no dependencies of their own:
- The minimal `Set` definition (avoids Mathlib).
- Opaque value Types whose representation is deferred but whose
  identity earns a kernel slot (`Instant`, `Text`).
- Forward-referenced Types needed across domain files.

Every file in `Causa/` imports this module. Nothing else
imports anything heavier here — keep the prelude clean.

This kernel is the source of truth for Proxima's domainless
invariants. The prose docs (`docs/*.md`) are rationale and
commentary; where they disagree with the kernel, the kernel wins
until renegotiated in writing. Spec-mode Lean: every primitive is
an `axiom` or closed `inductive`; there are no proof obligations.
-/

namespace Causa

-- ============================================================
-- Minimal Set primitive (avoids a Mathlib dependency).
-- Equivalent to Mathlib's `Set α := α → Prop`.
-- ============================================================

def Set (α : Type) : Type := α → Prop

instance {α : Type} : Membership α (Set α) := ⟨fun s a => s a⟩

-- ============================================================
-- Opaque value Types
-- ============================================================

/-- Time-point identity. Kept opaque; only `≤` is exposed. Source,
    memory, goal, audit, and runtime layers may attach distinct time
    meanings; the kernel commits to time comparability, not clock
    structure. -/
axiom Instant : Type
axiom Instant.le : Instant → Instant → Prop
instance : LE Instant := ⟨Instant.le⟩

/-- Free text attached to a memory row. Opaque: the kernel commits
    to the type slot, not its encoding or kind-based presence. Facts,
    Abstractions, and Perspectives may all carry optional text;
    flavor sidecars may carry additional opaque typed payload. -/
axiom Text : Type

end Causa
