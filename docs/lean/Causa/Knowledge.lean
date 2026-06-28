/-
Causa — Knowledge artifacts

Model-independent semantic uptake for text-bearing Memory rows.

This file deliberately does NOT define factive philosophical knowledge:
there is no `Truth`, `Believes`, `Justified`, `Knows`, probability, or modal
operator here. It only captures the weaker bridge Heinrich approved:
thought can be expressed as information, text is information, and a durable
text-bearing Proxima Memory can carry recoverable knowledge content independent
of one particular model instance.

The interpreter is a CLASS (human, LLM, or other), not a named runtime model or
person. A concrete LLM or human is an implementation-side instance of such a
class; the kernel only needs the class-level recoverability witness.
-/

import Causa.Memory

namespace Causa

-- ============================================================
-- Model-independent semantic uptake
-- ============================================================

/-- The propositional/informational content recoverable from text. Kept as
    ordinary `Text`: the kernel does not parse proposition syntax or provide a
    truth predicate. -/
abbrev KnowledgeContent : Type := Text

/-- Interpreter classes, not interpreter instances. This keeps knowledge
    artifacts independent from a particular LLM model, process, or human. -/
inductive InterpreterKind where
  | human
  | llm
  | other
  deriving DecidableEq, Repr

/-- A class of interpreters able to recover content from text. `canRecover` is a
    semantic-uptake boundary supplied by host/flavor/language practice, not by
    the core kernel. -/
structure InterpreterClass where
  kind : InterpreterKind
  canRecover : Text → KnowledgeContent → Prop

/-- A Memory row as a model-independent knowledge artifact. The row must carry
    text, and at least one interpreter class must be able to recover the stated
    content from that text. This is long-term representational knowledge, not
    factive `K p`. -/
structure KnowledgeArtifact where
  carrier : Memory
  content : KnowledgeContent
  text : Text
  textStored : memory_text carrier = some text
  interpreterClasses : Set InterpreterClass
  recoverable : ∃ c : InterpreterClass,
    c ∈ interpreterClasses ∧ c.canRecover text content

/-- A knowledge artifact admitted to a durable memory table. Physical retention,
    indexing, and query APIs are storage concerns; table membership is the kernel
    face of long-term persistence. -/
def KnowledgeArtifactIn (memories : Set Memory) (a : KnowledgeArtifact) : Prop :=
  a.carrier ∈ memories

/-- Recoverability by an interpreter class kind, e.g. by some human class or some
    LLM class. -/
def KnowledgeArtifact.recoverableByKind
    (a : KnowledgeArtifact) (kind : InterpreterKind) : Prop :=
  ∃ c : InterpreterClass,
    c ∈ a.interpreterClasses ∧ c.kind = kind ∧ c.canRecover a.text a.content

/-- A knowledge artifact always has text on its carrier Memory row. -/
theorem knowledge_artifact_has_text :
    ∀ a : KnowledgeArtifact,
      ∃ text : Text, memory_text a.carrier = some text := by
  intro a
  exact ⟨a.text, a.textStored⟩

/-- A knowledge artifact is model-independent in the intended weak sense: it is
    recoverable by at least one interpreter class, not by one named model/person. -/
theorem knowledge_artifact_model_independent :
    ∀ a : KnowledgeArtifact,
      ∃ c : InterpreterClass,
        c ∈ a.interpreterClasses ∧ c.canRecover a.text a.content := by
  intro a
  exact a.recoverable

/-- The artifact is recoverable by the kind of some registered interpreter class
    that witnesses semantic uptake. -/
theorem knowledge_artifact_recoverable_by_its_kind :
    ∀ a : KnowledgeArtifact,
      ∃ kind : InterpreterKind, a.recoverableByKind kind := by
  intro a
  rcases a.recoverable with ⟨c, hc, hrecovers⟩
  exact ⟨c.kind, c, hc, rfl, hrecovers⟩

/-- Long-term knowledge artifacts are admitted Memory rows that carry text. -/
theorem long_term_knowledge_artifact_has_text_memory :
    ∀ memories (a : KnowledgeArtifact),
      KnowledgeArtifactIn memories a →
        a.carrier ∈ memories ∧ ∃ text : Text, memory_text a.carrier = some text := by
  intro memories a hin
  exact ⟨hin, knowledge_artifact_has_text a⟩

end Causa
