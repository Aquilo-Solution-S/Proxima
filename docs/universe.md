# Universe

## 1. Ontology — three entities and one center

Most agent frameworks model the world as either a flat event stream (reactive)
or a hand-coded ontology (BDI-style). Neither captures how a mind actually
operates over time. The Spinning Wheel uses three entities and one center.

### Self (S) — *"How do I see myself."*

The inner world. Self-in-itself, independent of the world. Identity, values,
introspective state. Emerges from memories.

Examples:
- "I value Heinrich's trust."
- "I tend to over-explain when I'm uncertain."
- "I believe trust between humans and AI requires observability."

### Reality (R) — *the actual world*

Unknowable in itself. Only accessible through **Events** that get observed.
Reality never enters the agent directly — it is always mediated through an
Event Source.

Examples (the events, not Reality itself):
- A Forgejo webhook fires for a new commit.
- A Telegram message arrives.
- A test suite reports a failure.
- A monitoring alert triggers.

### Perspective (P) — *"How do I see the world."*

The meta-level above memories. The agent's model of Reality's causal chains.
Includes how the agent sees itself situated *in* Reality (Self-in-Reality).
Always a model, never the real chains. Disrupted when Reality contradicts it.

Examples:
- "gRPC streams die when idle because Kestrel's MinRequestBodyDataRate kicks in after 5s."
- "Heinrich pushes back when I scope-cut as avoidance."
- "Customers in finance need on-prem deployment or they walk."

### Goals — *the gravitational center*

The agent's core programm. Influenced by Self, shape Perspective, drive
Actions that attempt to move Reality. The motor of the wheel.

---

## 2. The Spinning Wheel — closed feedback loop

```
                      ┌─────────────┐
                      │   Goals     │  ← influenced by Self
                      └──────┬──────┘
                             │ shape
                             ▼
                      ┌─────────────┐
                      │   Actions   │
                      └──────┬──────┘
                             │ change
                             ▼
                      ┌─────────────┐
                      │  Reality    │
                      └──────┬──────┘
                             │ emits
                             ▼
                      ┌─────────────┐
                      │   Events    │  ← observed via Event Sources
                      └──────┬──────┘
                             │ enter
                             ▼
                      ┌─────────────┐
                      │  Memories   │  ← Self substrate
                      └──────┬──────┘
                             │ consolidate via dream
                             ▼
                      ┌─────────────┐
                      │ Perspective │  ← world model
                      └──────┬──────┘
                             │ + Self update
                             ▼
                      ┌─────────────┐
                      │   Goals     │  (loop closes)
                      └─────────────┘
```

## 3. Philosophical commitments

The Spinning Wheel rests on positions with well-developed lineages in
philosophy of science and neuroscience. Naming them explicitly so readers
can tell what is claimed and what isn't.

### Perspectivist constructivism about causation

**Causal claims are perspective-relative.** Facts alone yield events correlated
in time; the *why* requires an interpretive frame, and any system that asserts a
causal chain thereby commits to one. The position is epistemological, not
ontological: we make no claim that causation does not exist outside observers,
only that any *attribution* of causation is observer-relative. The lineage runs
through Hume (causation as inferred, not observed), Peirce (abductive inference),
van Fraassen (constructive empiricism), Pearl (causal DAGs are always *someone's*
model), and Cartwright (causal pluralism). The neuroscience version is predictive
processing / active inference (Friston, Clark, Hohwy): the brain does not observe
causes, it infers them under priors.

The load-bearing architectural consequence: **Perspective is the locus of
causal claims**, never Facts. Facts from multiple domains may be connected only
by a typed Abstraction over those Facts, optionally framed by a Perspective.
Direct semantic or causal Fact-to-Fact edges are forbidden. Mechanical
Fact-to-Fact edges remain structural/provenance only. Cosine similarity is
observer-independent and so cannot encode an observer-relative relation. This
grounds invariants 6 and 20 and the directionality rule of `02-memory.md`.

### "Causa proxima" as abductive inference, not legal proximity

The name is a redefinition. In Aquinas and 19th-century legal theory,
*causa proxima* is the *immediate* cause, contrasted with *causa remota*
(the further-back cause). Proxima uses it differently: **the nearest
*abductively plausible* cause that meta-reflection can reach** — Peirce's
inference-to-the-best-explanation applied at A/P, with neuroscience grounding
in predictive inference. When multiple perspective-dependent paths exist
the system admits there may be no unique nearest cause; that is a feature
of perspectivism, not a bug.

### What is and isn't claimed

These positions are not original to Proxima. The contribution is **treating
them as load-bearing engineering invariants in a running system**:
append-only storage that cannot overwrite a Perspective, build-time-typed
payloads that force operators to commit to a causal interpretation, schemas
that forbid similarity-wired edges. Cognitive architectures (SOAR, ACT-R,
LIDA) and philosophy of science have articulated the shape for decades;
what was missing was the operational discipline to ship it on top of LLMs
that can finally serve as F→A and A→P operators.

## What do we actually want to achieve?

The core question: what drives learning? What makes a mind a mind? The model
above describes a memory-centric world view, and the abstraction holds across
domains. Three mappings:

### Code World

- **Reality:** registered sources — repositories from different providers.
- **Memories:** state of the code at time point t, with edges between states that follow the causa proxima principle.
- **Perspective:** extraction across repositories — meta principles, shared architectures, shared code segments. Drift across multiple repositories is detected at this layer.
- **Goals:** aligned with repository goals — reduce drift, increase output.

### Learning World

- **Reality:** provided documents — university scripts, books, research papers, conversations with the user about the topics, generated exams, interaction events.
- **Memories:** state of the documents at time point t, with edges between memorable events such as conversations, tests, exams.
- **Perspective:** extraction from user sessions with user-centric observation — what does the user need to understand about topic X to be prepared for the exam's challenges.
- **Goals:** understand X.

### Legal World

- **Reality:** documents of cases, mandates, clients, notes, emails.
- **Memories:** extracted facts from documents, interactions, applied laws, and the interconnections between them.
- **Perspective:** patterns across cases that reduce friction from case to case; meta-insights across all data.
- **Goals:** improve output per time while reducing cognitive load and errors.

## Conclusions

The three domain mappings show that the same three-layer memory shape supports
distinct working contexts:

- **Factual memories** are extracted from real artefacts that exist at a definite
  time point t. They are retrievable, associable, and one source document can
  yield many of them.
- **Abstracted memories** aggregate over factual memories without themselves
  referring to a single source — they hold patterns, not records.
- **Perspective memories** sit above both layers and carry the system's
  reasoning. They may frame cross-domain Abstractions, but do not create direct
  semantic Fact-to-Fact edges.

The architectural commitment is that this layering is *strict and irreversible*:
no operator may produce a lower-layer memory from a higher-layer one. That is
what keeps Facts immutable under Perspective change — and what makes a
Perspective revision a clean operation rather than a rewrite of history.
