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

**There is no causal chain without an observer's perspective.** Facts alone
yield events correlated in time; the *why* requires an interpretive frame.
The lineage runs through Hume (causation as inferred, not observed), Peirce
(abductive inference), van Fraassen (constructive empiricism), Pearl (causal
DAGs are always *someone's* model), and Cartwright (causal pluralism). The
neuroscience version is predictive processing / active inference (Friston,
Clark, Hohwy): the brain does not observe causes, it infers them under priors.

The load-bearing architectural consequence: **Perspective is the locus of
causal claims**, never Facts. Edges encoding causation must be P-authored
or operator-justified; cosine similarity is observer-independent and so
cannot encode an observer-relative relation. This grounds invariant 6 and
the directionality rule of `02-memory.md`.

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

The core question is - what drives our learning? What makes a human mind a human mind?
The above principle describes a word view which is memory centric. The abstraction holds.
Lets map some cases against it:

### Code World

Reality: The reality are the registered sources - for example repositories from different providers.
Memories: State of the Code at time point t and the connecting edges between them which follow the causa proxima principle
Perspective: Extraction out of the code - meta principles accross repositories, shared architectures,
shared code segments - meta level of the concepts which are recognizable. Here the drift inside multiple repositories is detected
Goals: Matched with the goals of the repository, reduce drift, increase output

### Learning World

Reality: The reality are proviced documents - scripts from university, books, research papers,
conversations with the user about the topics, created exams, interaction events
Memories: State of the documents at time point t and connecting edges between the memorable events
like conversations, tests, exams and so on.
Perspectie: Extraction out of the user sessions with a user centric observation, what does the user
need to understand about topic X to be prepared for the challanges of the exam
Goals: Understand X

### Juristication World

Reality: Documents of cases, Mandates and Mandanten, notes, emails and so on.
Memories: Extraction and facts of documents, interactions, applied laws and interconnections between them
Perspective: Extraction of patterns accross cases to reduce friction from case to case. Meta insights into all data.
Goals: Improve output per time while reducing cognitive load and errors

## Conclusions

As we see we need different kind of memories - fact baseds are extracted from real documents which
are exactly existing at a time point t and can be retrieved and associated. One Document can
produce a meaningfull amount of factual memories. Abstracted memories are such which are coming from
non factual things like aggregations from factual memories. Perspective creating memories are then
memories above them all which create a reasoning.