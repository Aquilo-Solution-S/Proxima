# 13 — Flavor Marketplace

> **Status: concept doc.** Formalizes the evolution path for flavor
> distribution and composition. v1 ships none of the marketplace
> mechanics described here — only the architectural primitives in 08
> (composite crate, `proxima_flavor!` macro, build-time registration)
> that make this evolution possible. This document exists to
> demonstrate that the v1 cut scales; it is not a commitment to
> implement.

## What this is

The marketplace for **T2** (full flavor crates: schemas + sources +
tools + prompts; see [08](docs/08-core-and-flavors.md) / [12](docs/12-tool-manifest.md)). The T1 tool marketplace lives in [12](docs/12-tool-manifest.md).
Different tier, different distribution, different trust shape — each
formalized in its own doc.

A flavor marketplace turns "flavors are independently authorable"
into "third parties and customers actually publish, discover, vet,
and compose them." The strategic positioning (Aquilo ships substrate
plus reference flavors; ontology is not Aquilo's moat) is the causa
proxima thesis operationalized at the supply-chain level.

## Five layers

The marketplace decomposes into five orthogonal layers. Each is
replaceable; v1 implements none.

### 1. Distribution

Flavor crates distribute via **standard Cargo registries**.

- Public OSS flavors: crates.io.
- Paid / customer-internal flavors: private registries (Cloudsmith,
  Shipyard, self-hosted).
- Versioning: semver. Composite crates pin versions per standard
  Cargo mechanics.

No Aquilo-specific distribution infra. The boring-on-purpose layer.

### 2. Authoring contract

A flavor author produces a Cargo crate that depends on `proxima-core`
and exposes its surface via the `proxima_flavor!` macro (08
§Registration mechanism). The Code reference flavor is the contract
demonstrator — its dual purpose (real product + exemplar) is
intentional.

A flavor's surface is **partial**. A well-formed flavor crate
provides any subset of the following — there is no flavor kind
discriminator (08 §Registration mechanism):

- `FactPayload` / `AbstractionPayload` / `PerspectivePayload` /
  `CitedObjectPayload` / `CitationMappingPayload` / `GoalPayload` impls
  with sidecar SQL migrations (03, 06, 11).
- `EventSource` impls for its Reality slice.
- `RelationDescriptor` registrations.
- T2 tool registrations (12).
- **Operators** with their prompts and cadence policies:
  - F→A operator per `(Fact schema, Abstraction schema)` pair — at
    most one per pair across the binary; multiple F→A operators may
    coexist over the same Fact schema, each producing a distinct
    typed Abstraction (composite-time enforced).
  - A→P operator(s), intra-flavor or cross-flavor, with declared
    flavor scope and personality scope (own-personality vs
    all-personalities). Multiple A→P operators across the binary are
    allowed and run in parallel.
  - A→Goal operator(s), agent-discovered-goal synthesis under Π.
  - Edge operator(s), authoring `Causal` / `Interpretive` edges
    over the existing A/P set without producing new memories.
- **`PersonalityFlavor` impls** — one or many per crate.
  Each declares a stable `PERSONALITY_ID`, the snapshot rule
  (which P/G enter `P_active` / `G_active`, top-K caps, fusion,
  identity weighting), and the `project_self` query. Personality
  flavors compose with everything else: a binary that links Code +
  Stoic-Visionary projects a stoic Self over code Reality; swap to
  Workhorse-Programmer and the same memory graph projects a
  task-focused Self.

A pure-flavor brings schemas + sources + intra-flavor operators.
A pure-cognition flavor brings cross-flavor operators only. A
**pure-personality flavor** brings only `PersonalityFlavor` impls —
no schemas, sources, or operators of its own. Most useful flavors mix.

**Flavors that depend on other flavors are a first-class marketplace
shape, not an edge case.** A flavor C with Cargo deps on flavors A
and B may author operators whose inputs and outputs span A and B's
schemas (a `bug-triage` flavor over `code` + `learning`; a
`compliance` flavor over `code` + `jurisdiction`; the
`general-reasoning` cognition flavor in 08 is the canonical example).
The Cargo dep is the explicit-allow signal — declaring the dep is
how flavor C asserts intent to author against another flavor's
schemas. Capability-bounded customer policy enforcement (e.g.
"third-party flavors may not author operators on `code/*` schemas
in this deployment") is the §Trust tier's job, not the macro's.

The `proxima_flavor!` macro is the **authoring contract surface**.
Stability of this macro is therefore a marketplace concern: breaking
it costs the entire ecosystem a recompile.

#### Stability policy

Three rules govern macro evolution:

1. **Additive changes only within a major version.** New optional
   fields, new operator kinds, new payload kinds, new helper macros
   — all minor-version bumps. Existing flavor invocations compile
   unchanged. This is where almost all evolution lands. Required-field
   additions, field renames, and semantic changes to existing fields
   are *not* additive — they are breaking, even when the syntax
   still compiles.

2. **Breaking changes go through a parallel macro, never in-place.**
   When the contract genuinely must break, ship `proxima_flavor_v2!`
   alongside the existing `proxima_flavor!`. Both expand to the
   same Registry calls — `Registry` is the runtime contract; the
   macro is the authoring contract — so a binary may link flavors
   written against v1 and v2 simultaneously. Deprecation cycle: v2
   ships with v1 silent and supported; after two minor releases of
   stable v2 use, v1 starts emitting a `#[deprecated]` compile
   warning; v1 removes on the next major bump after that. No flavor
   is forced to migrate on Aquilo's schedule.

3. **The macro lives in its own semver-pinned crate.**
   `proxima-flavor-macros` is a separate crate from `proxima-core`;
   flavors depend on both and pin both independently. The split
   exists so the *contract surface* evolves by contract rules,
   while `proxima-core`'s runtime internals (Registry struct
   layout, internal traits, executor) can evolve at their own
   cadence without claiming the contract changed. The two crates
   ship from one workspace and bump majors in lockstep, but the
   separate semver is what flavor authors pin against.

Two refinements:

- **`#[experimental]` field gate.** Fields landed but not yet
  promised stable live behind a Cargo feature on the macro crate
  (`proxima-flavor-macros = { version = "1", features = ["experimental"] }`).
  Anyone using them accepts breakage at any minor bump. Lets the
  team experiment in the open without contract weight.
- **`PROXIMA_CONTRACT_VERSION` const re-export.** A `pub const`
  on the macro crate that flavors can `static_assert!` against
  if they want explicit pinning beyond Cargo's `=1.2.3` syntax.
  Belt-and-braces; rarely needed, useful for vetted flavor
  publishers who want to assert the build matches their tested
  contract version.

### 3. Composition

A composite crate is **per-customer engineering work**. It depends on
its constituent flavor crates and assembles them into one binary
(08 §Composite discipline).

Composition is **not** a free union when it comes to cross-flavor
synthesis. Cross-flavor Perspectives require *some* A→P operator
that takes cross-flavor inputs. The standard path is to include a
cognition flavor (e.g., `general-reasoning`) alongside the
schema-owning flavors — its A→P operators consume the union A-graph
and emit cross-flavor Perspectives. The composite crate may also
author its own A→P operators when deployment-specific synthesis is
needed. Without either, the binary runs each flavor's F→A but
produces no cross-flavor Perspectives. The authoring tax is
intentional — synthesis is the value proposition, and the operators
that produce it are where that value is encoded — but it doesn't
have to live in the composite.

**Topology surface.** Beyond the flavor mix, a deployment also
configures the per-Owner read-scope matrix
(02 §Read-scope matrix, 07 §read_scope_matrix) — boolean adjacency
across the binary's linked personalities, with identity diagonal
hardcoded. Two deployments with the same flavors can have radically
different cognitive characters depending on their matrices: full mesh
(every personality reads every other), star (a Synthesist reads all,
none read it back), isolated (each personality strictly its own
subgraph). Asymmetry is a feature, not a workaround.

The substrate's foundational claim — **the agent is not a self; the
agent is a substrate that hosts selves** — is what this layer
operationalises. The agent doesn't *have* a self; the agent *runs* a
topology of voices, each addressable, each connected by a matrix that
can change over time. What "the agent thinks about X" is a query
parametrised by which personality you're asking, and that question
has multiple legitimate simultaneous answers. Selfhood is a
configurable topology over a substrate of memory.

Two composition models:

- **Customer-authored.** A customer engineer writes the composite
  crate. Lowest infra requirement, highest authoring bar.
- **Tool-assisted.** A `proxima compose` CLI or web app takes a
  flavor selection (including personality flavors and any cognition
  flavors that supply cross-flavor A→P), a per-Owner read-scope
  matrix, and any composite-level A→P operators the deployment
  wants to add, and emits a composite crate skeleton. Lower bar,
  same artefact. See §Compose tool.

#### Composite, product, deployment — three repo tiers

The composite crate names *which flavors* compose. A shipping
product adds *brand and clients* on top — Tauri shell, mobile apps,
landing page, signup UX. A running deployment adds *environment* —
secrets backend, IdP issuer URL, k8s manifests. Three concerns,
three repo tiers, no overlap:

| Tier | What it owns | Typical visibility |
|---|---|---|
| Substrate + reference flavors | engine, traits, `AuthResolver` impls (14 §Auth model), `proxima-shell` (09), first-party flavors | public monorepo |
| Product | composite crate, brand assets, mobile/desktop shells, landing page, App Store metadata | public or private per product |
| Deployment | k8s manifests, secrets backend (Vault / OpenBao etc.) refs, env-injected resolver config | private/ops |

The split is what makes "same flavor in N products" and "same
product in N environments" cheap. Multi-product flavors don't fork;
rebrands don't bump flavor versions; deployment changes don't
rebuild binaries. Resolver config (e.g. `OIDC` issuer URL, JWKS)
flows from the deployment tier as env vars, so the composite binary
is portable across environments without recompilation. The trust
tiers in §Trust are orthogonal — they govern who authored each
flavor in the composite, regardless of which product ships it or
where it deploys.

### 4. Discovery

crates.io is a registry, not a marketplace. **Discovery is a content
problem, not an infra problem.**

A curated catalog (e.g. `aquilo.com/flavors`) lists available flavors
with:

- What Reality slice the flavor covers.
- Which `FactPayload` / `AbstractionPayload` schemas it produces.
- Which other flavors it composes well with (cross-flavor relation
  hints).
- Trust tier (see below).
- Authorship and provenance.

The catalog is editorial — Aquilo curates the index, but the
underlying distribution remains open Cargo registries. Listing in the
catalog is reputation; install is via Cargo.

### 5. Trust

Three-tier trust model.

| Tier | Provenance | Validation | Capability bounds |
|---|---|---|---|
| **Aquilo-published** | Aquilo authors and ships. | Internal review + CI. | Full T2 surface. |
| **Aquilo-vetted** | Third-party authored, signed by Aquilo after audit. | Audit + signed crate. | Full T2 surface. |
| **Community** | Third-party authored, self-signed. | cargo-crev or equivalent web-of-trust. | Capability declarations enforced; sandbox where possible. |

Capability bounds parallel the T1 model in 12 (`emit_facts`,
`emit_relations`, `owner_scope`) but at the flavor granularity: a
flavor declares which `EventSource` permissions, network egress, and
credential scopes it requires. The composite-crate build asserts
that capability declarations across constituent flavors don't violate
customer policy.

**Per-flavor pg schema as a permission boundary.** The namespacing
rule from [07 §Storage layout](docs/07-storage.md#storage-layout) —
every flavor's sidecars live under `proxima_<FLAVOR_ID>.*` —
doubles as the database-level trust boundary. A deployment can
`GRANT` write only on a flavor's own pg schema while keeping read
across the binary's full surface, mirroring the composite-discipline
rule (08) at the SQL layer. The Rust capability declaration (which
sources, which network egress) and the Postgres GRANT (which
sidecars are writeable) compose naturally: capability declared in
code, enforced in the database. v1 doesn't ship hosted builds, but
the boundary is in place when it does — and remains useful for
Community-tier flavors running in shared-database deployments where
SQL-level isolation is the second line of defence behind
capability enforcement in code.

Signing: ed25519 signed crates per the existing `[tool.signature]`
shape from 12, lifted to the crate artefact. Same algorithm, same
allowlist mechanism, different granularity.

## Compose tool

The compose tool is what turns composite authorship from a
Rust-engineering task into a structured workflow.

Sketch:

```
$ proxima compose new my-binary
$ proxima compose add-flavor code@1.4
$ proxima compose add-flavor learning@0.7
$ proxima compose add-flavor general-reasoning@0.3   # cross-flavor A→P
$ proxima compose add-a2p prompts/my_a2p.md          # optional: composite-level A→P
$ proxima compose build
```

What the tool does:

1. Generates a Cargo crate skeleton with `proxima_composite!`
   listing the chosen flavors.
2. Validates capability-declaration compatibility (no flavor
   exceeds the binary's policy).
3. Wraps any composite-level A→P operators the customer supplies
   into the composite's `register` flow (cross-flavor synthesis
   from cognition flavors works without this step).
4. Emits a buildable Cargo workspace; `cargo build` produces the
   binary.

This is a code generator, not a runtime. The output is plain Rust;
the build remains transparent.

## Open strategic fork: hosted builds vs always on-prem

**Always on-prem.** Customer runs `cargo build` themselves; binary is
theirs; on-prem deployment is straightforward; air-gapped customers
supported. Cost: gates on Rust toolchain.

**Hosted composite builds.** Customer picks flavors via a web UI;
Aquilo builds the binary; ships a container image. Lowers floor
dramatically. Cost: SaaS business model, single trust point, harder
air-gap story, source-availability questions.

These are different products, not different deployment modes of the
same product. **This document does not pick.** The fork is flagged
here to make clear that the v1 architecture supports both — flavors
are crates, composites are crates, builds are reproducible — and the
choice is a go-to-market decision separable from the engineering
primitives.

## Relationship to T1 marketplace (12)

The T1 tool marketplace is **already specced and on the v1 path**
(12). It is a runtime, capability-bounded, signed-manifest install of
tool bodies that ride existing flavor schemas.

The flavor (T2) marketplace described here is **build-time and
v2+**. It distributes the schemas, sources, prompts, and tools
themselves.

| | T1 (tools) | T2 (flavors) |
|---|---|---|
| Spec home | 12 | 08, 13 |
| Brings | Manifest + body | Schemas + sources + prompts + tools |
| Distribution | Runtime install via signed manifest | Cargo crate |
| Trust | Per-manifest signature, capability-bounded | Per-crate signature, capability-bounded |
| Composition | None — installs into a running binary | Composite crate; cross-flavor A→P from cognition flavors and/or composite-authored operators |
| v1 status | In scope | Out of scope; primitives in 08 |

Both tiers share the **capability-bounded signed-artefact** trust
shape. The same `[signature]` machinery from 12 lifts cleanly to the
crate level.

## Non-goals for v1

- No flavor catalog.
- No `proxima compose` tool.
- No third-party flavor authorship workflow documented for external
  consumers.
- No hosted-build SaaS.
- No third-party flavors actually distributed — Aquilo ships its own
  reference flavors only.

What v1 **does** ship that enables this evolution:

- The `proxima_flavor!` and `proxima_composite!` macros ([08](docs/08-core-and-flavors.md)).
- Build-time registration with compile-time conflict detection ([08](docs/08-core-and-flavors.md)).
- Per-flavor sidecar discipline ([03](docs/03-schema-registry.md)).
- Code as the reference exemplar.
- Capability-bounded signed artefacts at the T1 tier (12) — the
  trust shape that lifts to T2.

## Why formalize now

The marketplace is not v1 work, but the v1 architecture is built
around its existence. Formalizing the evolution path here:

- Validates that the v1 cut is a foundation, not a corner.
- Makes the strategic positioning legible to investors, partners,
  and prospective flavor authors without conflating it with shipping
  work.
- Documents the open hosted-vs-on-prem strategic fork as a known
  decision, not a discovered one.
- Names the `proxima_flavor!` macro as a stability surface so its
  evolution is treated with the seriousness of an API.

The marketplace is the operational consequence of the causa proxima
thesis. Aquilo's contribution is the engineering invariants; flavors
are the flavor mass; the marketplace is how that mass distributes.

## Cross-references

- **08 §Substrate stance** — architectural cut between core and
  flavors.
- **08 §Composite discipline** — how flavors compose into a binary.
- **08 §Registration mechanism** — `proxima_flavor!` and
  `proxima_composite!` macros.
- **12** — T1 tool marketplace; shared signed-artefact trust shape.

## Anchors

- `what-this-is`
- `five-layers`
- `distribution`
- `authoring-contract`
- `composition`
- `discovery`
- `trust`
- `compose-tool`
- `open-strategic-fork-hosted-builds-vs-always-on-prem`
- `relationship-to-t1-marketplace-12`
- `non-goals-for-v1`
- `why-formalize-now`
- `cross-references`
