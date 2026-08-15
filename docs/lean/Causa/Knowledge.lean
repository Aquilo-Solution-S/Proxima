/-
Causa — Knowledge artifacts

Model-independent semantic uptake. Text lives on the sidecar, not on Memory
(UML §4). The artifact still names an admitted Memory carrier.
-/

import Causa.Memory

namespace Causa

abbrev KnowledgeContent : Type := Text

inductive InterpreterKind where
  | human
  | llm
  | other
  deriving DecidableEq, Repr

structure InterpreterClass where
  kind : InterpreterKind
  canRecover : Text → KnowledgeContent → Prop

/-- A Memory row plus sidecar text. Text is NOT a Memory field. -/
structure KnowledgeArtifact where
  carrier : Memory
  content : KnowledgeContent
  text : Text
  interpreterClasses : Set InterpreterClass
  recoverable : ∃ c : InterpreterClass,
    c ∈ interpreterClasses ∧ c.canRecover text content

def KnowledgeArtifactIn (memories : Set Memory) (a : KnowledgeArtifact) : Prop :=
  a.carrier ∈ memories

def KnowledgeArtifact.recoverableByKind
    (a : KnowledgeArtifact) (kind : InterpreterKind) : Prop :=
  ∃ c : InterpreterClass,
    c ∈ a.interpreterClasses ∧ c.kind = kind ∧ c.canRecover a.text a.content

theorem knowledge_artifact_has_text :
    ∀ a : KnowledgeArtifact, ∃ text : Text, text = a.text := by
  intro a
  exact ⟨a.text, rfl⟩

theorem knowledge_artifact_model_independent :
    ∀ a : KnowledgeArtifact,
      ∃ c : InterpreterClass,
        c ∈ a.interpreterClasses ∧ c.canRecover a.text a.content := by
  intro a
  exact a.recoverable

theorem knowledge_artifact_recoverable_by_its_kind :
    ∀ a : KnowledgeArtifact,
      ∃ kind : InterpreterKind, a.recoverableByKind kind := by
  intro a
  rcases a.recoverable with ⟨c, hc, hrecovers⟩
  exact ⟨c.kind, c, hc, rfl, hrecovers⟩

theorem long_term_knowledge_artifact_has_text_memory :
    ∀ memories (a : KnowledgeArtifact),
      KnowledgeArtifactIn memories a →
        a.carrier ∈ memories ∧ ∃ text : Text, text = a.text := by
  intro memories a hin
  exact ⟨hin, knowledge_artifact_has_text a⟩

end Causa
