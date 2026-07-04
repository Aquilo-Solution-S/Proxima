/-
Causa — axiom-surface printer.

Prints every SAFE `axiom` declared under the `Causa` namespace, one fully
qualified name per line, sorted. This is the kernel's primitive surface: the
complete, exhaustive set of domain axioms the proofs in `docs/lean/Causa`
assume, independent of which theorems happen to reference them today.

Two things are deliberately excluded so the pinned set stays meaningful:

* Lean's own built-in axioms (`propext`, `Classical.choice`, `Quot.sound`) —
  those are the trusted base of Lean itself, not something this kernel can
  add or remove.
* `unsafe` compiler-internal stand-ins (e.g. `_elambda_1` closure-extraction
  artifacts Lean's equation compiler sometimes synthesizes for `def ... where`
  structure literals). These show up as `axiomInfo` in the environment but are
  code-generation plumbing for the *compiled/interpreted* value, never part of
  a proof term — `#print axioms <theorem>` never lists them as a dependency,
  and neither does this scan (filtered via `isUnsafe`).

Regenerate the checked-in allowlist after an intentional kernel change with:

    cd docs/lean && lake build && lake env lean scripts/PrintAxioms.lean \
      > ../../scripts/lean-axioms.allowlist.txt

(`scripts/check-lean-axioms.py` runs the equivalent invocation and diffs
against that file; see its --help / header for details.) An empty output is a
valid result: the script first asserts that a known Causa declaration loaded, so
zero lines means zero Causa axioms, not a silent import miss.
-/
import Lean
import Causa

open Lean

#eval show Lean.Elab.Command.CommandElabM Unit from do
  let env ← getEnv
  unless env.contains `Causa.Flavor.wipeable_when_abandoned do
    throwError "Causa import did not expose Causa.Flavor.wipeable_when_abandoned; refusing to print an axiom surface"
  let mut names : Array String := #[]
  for (n, ci) in env.constants.toList do
    match ci with
    | .axiomInfo v =>
      if n.getRoot == `Causa && !v.isUnsafe then
        names := names.push n.toString
    | _ => pure ()
  let sorted := names.qsort (· < ·)
  for n in sorted do
    logInfo n
