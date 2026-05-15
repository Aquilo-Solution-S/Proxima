# 15 — Compliance primitives

Proxima is an open-source substrate. Compliance with subject-rights
regimes (GDPR, UK GDPR, CCPA, LGPD, …) is the *controller's*
obligation — i.e. the natural or legal person who deploys a
Proxima-based product and decides the purposes and means of
processing personal data. The substrate cannot deliver compliance;
it can only provide the primitives that make compliance achievable.
Whether those primitives are invoked, how, by whom, and under which
regime is a deployment decision.

This doc defines those primitives:

- **Compliance operations** the substrate exposes (delete, pause,
  restrict, export) and their semantics.
- **Suppression and audit mechanics** that make erasure provable
  and re-ingest-safe.
- **Metadata** the substrate requires flavors to declare at
  registration (lawful basis, residency, retention, special-category
  flags), so the operations have something to act on.
- **Explicit limits** of what the substrate can and cannot do —
  particularly around external side effects already executed by
  tool calls.

The cognitive layer is structurally non-forgetting (Facts immutable;
A/P/Goals revise via supersession, not deletion — see
[02 §Re-derivation and supersession](02-memory.md#re-derivation-and-supersession)).
Subject-rights regimes require operations that *do* forget — a
deliberate, audited, out-of-band break of the cognitive invariant,
scoped to a specific Owner. The substrate ships those operations;
the controller decides when to run them.

A worked mapping to GDPR articles appears throughout this doc as
inline citations (Art. 5, 6, 9, 13, 15, 17, 18, 19, 20, 22, 32) —
GDPR is the strictest common case, so a substrate sufficient for
GDPR is generally sufficient for the others. The mapping is
illustrative, not exhaustive; controllers under other regimes
(CCPA's "right to know" / "right to delete", LGPD's Art. 18, …)
will find the same primitives apply with different naming.

What is out of scope by design: lawful-basis *selection* per
collection event, DPIA documentation, breach detection and 72-hour
notification, DPO appointment, SCCs for cross-border transfers,
privacy-policy text and consent UX. These are controller and Ops
concerns; the substrate provides hooks (residency allowlists,
recipient declarations, special-category flags) but does not
legislate the policy.

## Cognitive vs compliance

Two distinct lifecycle layers, never confused:

| Layer | Mode | Authorship | Audit |
|---|---|---|---|
| Cognitive | Supersession only; Facts immutable; A/P/Goals revise via new memory + `supersedes` edge | `EventSource`, `Operator*`, `PerspectiveLink`, `Core(Engine)`, `Core(User)` — see [02 §The core entity](02-memory.md#the-core-entity) | The supersession chain itself is the audit |
| Compliance | Hard delete, pause, restriction, export | Admin-invoked; no Authorship variant; flavor selectors may calculate scope | Separate `compliance.*` schema, never visible to operators |

Operators and deciders observe the diminished graph as if deleted
entries had never existed. They cannot read the compliance audit
log; they cannot author compliance operations; they cannot
overload them as cognitive lifecycle transitions. The compliance
API is owned by the engine and surfaced through
[14](14-protocol-surface.md). Flavors may provide typed selectors
for scope calculation; the resulting hard delete is still a
compliance operation, not a flavor-authored Memory mutation.

## Operations

All compliance operations are bounded by one Owner. The target may
be the whole Owner or an Owner-scoped source object. Cross-owner
edges are not expressible in v1 (see [06 §Scoping](06-goals-and-self.md#scoping)),
so a single-Owner operation is structurally complete — no
distributed coordination, no cross-shard cleanup.

### `delete_owner(owner_id, reason)` — v1 primary primitive

Atomic erasure of all memories, goals, edges, edge sidecars,
embeddings, source-batch payloads, and operator-invocation cache
entries for one Owner, in a single transaction. Idempotency-key
entries are *retained but flagged* — see §Suppression list.

```rust
fn delete_owner(
    owner_id:  OwnerId,
    requester: PrincipalId,        // controller-side admin, see 14
    reason:    DeletionReason,
) -> Result<DeletionReceipt, DeletionRefusal>;

enum DeletionReason {
    GdprArt17,                     // explicit subject erasure request
    ConsentWithdrawn,              // Art. 7(3)
    RetentionExpired,              // policy-driven, per source/Owner retention
    AdminInitiated { description: String }, // operational; recorded verbatim
}

enum DeletionRefusal {
    LegalRetention { basis: String, until: Option<Date> },  // Art. 17(3)(b), (e)
    PendingLitigation { case_ref: String },                 // Art. 17(3)(e)
    OwnerNotFound,
    Unauthorized,
}

struct DeletionReceipt {
    deletion_id:            DeletionId,    // uuidv7
    owner_id:               OwnerId,
    completed_at:           Timestamp,
    memory_count:           u64,
    goal_count:             u64,
    edge_count:             u64,
    embedding_count:        u64,
    suppression_keys_added: u64,
}
```

`DeletionRefusal` is load-bearing for legal flavors. A regulated
domain (Anwaltskanzlei under BORA / AO §147 / GoBD, healthcare
under HIPAA-equivalent retention, finance under MaR) *must* be
able to refuse a subject deletion request with a recorded basis;
the refusal is itself an auditable event and surfaceable to the
data subject as a structured response. Refusal is not failure —
it is one of the two valid outcomes.

### `delete_source_scope(owner_id, source_scope, reason)` — v1 scoped primitive

Atomic erasure of all substrate rows attributable to one
Owner-scoped source object, plus that source object's flavor-owned
index rows. Example: Code flavor repo erasure deletes the repo's
Facts, derived Abstractions, incident edges, edge sidecars,
embeddings, events, citation mappings, cited objects with no
remaining mappings, source batches with no remaining events, and
the `proxima_code.repos` row.

```
SourceScope =
  { flavor_id: "proxima-code", object_kind: "repo", object_id: RepoId }
| { flavor_id: "...", object_kind: "...", object_id: ... }
```

Rules:

- Scope resolution is flavor-typed; deletion execution is
  compliance-mode.
- The scope must resolve entirely inside one Owner.
- The receipt records counts and the opaque source object id, never
  deleted payloads.
- Suppression keys are retained for every deleted source batch /
  source object natural key needed to prevent silent re-ingest.
- If retention policy protects any member row, the operation returns
  `DeletionRefusal` unless controller policy permits partial erasure.

### `cascade_delete(memory_id, reason)` — deferred

Per-memory erasure with provenance backtracking: walks
`core/derived-from` edges, collects the transitive closure of
A/P that derive from the targeted Memory, deletes the entire
set in one transaction. Substrate ships this **when the first
case-by-case request lands**, not in v1. Reasons:

- `delete_owner` covers the dominant subject-request shape
  ("delete me") at a fraction of the implementation cost.
- Per-memory cascade requires partial-graph repair semantics
  that interact subtly with operator-invocation idempotency
  (cached invocations referencing partially-deleted input sets
  must be invalidated, not silently re-run with diminished input).
- Re-derivation as an alternative to cascade is non-deterministic
  (LLM operators may reproduce deleted PII from training context
  or from siblings); cascade-only is the safer v1 default.

Until shipped, partial-erasure requests route through Ops:
either re-scope the affected data into a sub-Owner that can be
`delete_owner`'d, or refuse with `LegalRetention`.

### `pause_owner(owner_id)` / `resume_owner(owner_id)`

Art. 18 restriction of processing. While paused, operators skip
the Owner — no F→A, no A→P, no decider runs — but data remains
intact and reads remain available (`export_owner` still works,
UI reads still work). Implementation: a `paused` flag on the
Owner row; every operator's `availability` predicate AND'd with
`!owner.paused` at engine dispatch level, not flavor-overridable.

### `export_owner(owner_id, format)`

Art. 15 (subject access) and Art. 20 (portability). Walks every
table where `owner_id` appears, serialises to one of `{ json,
ndjson, csv-bundle }`, returns a manifest plus content. Includes:

- All memories with payload sidecar joined
- All goals with payload sidecar joined
- All edges with edge sidecar joined (when EdgePayload ships)
- All embeddings (vectors as float arrays or base64)
- Source-batch records (provenance metadata)
- Compliance audit entries *involving this Owner*

Excludes: other Owners' data, internal engine state (operator
scheduler queues, LLM-tier-router caches, suppression-list keys
for *other* Owners).

## Suppression list — re-ingest rejection

The hardest substrate-level concern after deletion. If `F1` is
deleted under Art. 17 and the upstream source (GitHub webhook,
email sync, file watcher) re-emits the same payload tomorrow,
naive idempotency dedup *re-ingests* `F1`. PII is back. Compliance
violated through the regular ingest path.

Resolution: the source-batch idempotency-key table retains entries
for deleted batches with a `deleted_at: Timestamp` column. Source
ingest checks this flag *before* dedup and rejects the batch with
`SourceRejection::Suppressed`; the source observes the rejection
the same way it would observe a duplicate batch — no-op, no error,
no retry.

The suppression entry contains *only* the opaque idempotency key
and deletion timestamp — no payload, no PII. The key schema
([01](01-event-source.md)) is required to be content-derived
(hash) or otherwise opaque, never a verbatim natural identifier
of a person.

Suppression entries are themselves indefinite — they cannot be
erased on subject request, because their erasure would re-open
the re-ingest hole. This is justified under Art. 17(3)(b)
(controller's legal obligation to comply with Art. 5(2)
accountability).

## Audit log

Separate Postgres schema `compliance` outside `proxima_core`.
Not readable by operators; not exposed via the cognitive read
APIs; surfaced only through 14's admin protocol.

Two tables minimum:

- `compliance.deletions` — one row per `delete_owner`,
  `delete_source_scope`, or `cascade_delete` invocation (when the
  latter ships).
- `compliance.actions` — one row per `pause_owner`,
  `resume_owner`, `restrict_processing`, `export_owner`
  invocation.

Both record:

- `id: uuidv7` of the operation
- `owner_id: OwnerId` (opaque — not the natural-person identifier)
- `requested_at`, `completed_at`, `requester: PrincipalId`
- `reason` (typed enum, varies by operation)
- `outcome: Outcome { Completed { counts } | Refused { refusal_kind } }`
- For Delete: counts of affected rows (no content)
- For Refusal: structured refusal reason + retention citation

Never recorded: deleted content, the natural-person identifier
mapped to suppressed idempotency keys, payload diffs, decision
trees that led to the operation.

Retention: indefinite. The audit log is the controller's evidence
that erasure happened (or was correctly refused) and itself falls
under Art. 5(2) — it cannot be erased on subject request, because
the controller has a legal obligation to retain it. Audit entries
are the one substrate construct that survives `delete_owner` for
the same Owner, by design.

## External side effects

Compliance operations cannot reach beyond the substrate. A decider
that, prior to `delete_owner`, fired an external tool call (sent
an email, posted a Slack message, opened a PR, transferred funds,
filed a legal notice) *has done that*; the resulting state in the
third-party system persists, and the substrate cannot undo it.

Implications:

- The `DeletionReceipt` does not promise external state was rolled
  back. It promises substrate-internal erasure only.
- Controllers responsible for downstream cleanup must do so out of
  band, using the *exported* tool-call records (run `export_owner`
  before `delete_owner`) as a reference list of where data went.
- Tool manifests ([12](12-tool-manifest.md)) must declare
  `recipients: Vec<RecipientId>` so Art. 19 notification
  obligations (informing each recipient the data was shared with)
  are at least mechanically discharge-able by Ops.
- Tools that perform legally-significant external actions (sending
  legal notices, transferring funds, modifying public records,
  contacting third parties on the subject's behalf) must be wired
  under human-in-the-loop deciders — the pattern is the
  proposal-Fact-plus-approval-Source flow already documented in
  [05 §Deciders](05-actions.md#deciders--flavor-supplied), gated
  by the `legal_consequence: bool` flag on the tool manifest
  ([12](12-tool-manifest.md)). Engine refuses to wire a
  `legal_consequence = true` tool to a fully-automatic decider
  without an explicit override.

## Compliance vocabulary

Three enums and one type alias define the shared compliance
vocabulary. They live in `proxima_core::compliance`; 01, 03, and
12 reference them when they declare their per-source / per-schema
/ per-tool fields. Each enum has a value that gracefully expresses
"no applicable regime" — substrate does not impose GDPR-shaped
declarations on deployers under more permissive jurisdictions.

```rust
enum LawfulBasis {
    NotApplicable,                                        // no GDPR-style regime applies
    Consent,                                              // Art. 6(1)(a)
    Contract,                                             // Art. 6(1)(b)
    LegitimateInterest { description: String },           // Art. 6(1)(f)
    LegalObligation    { citation: String },              // Art. 6(1)(c)
    VitalInterest,                                        // Art. 6(1)(d)
    PublicTask,                                           // Art. 6(1)(e)
}

enum RetentionPolicy {
    Indefinite { reason: String },                        // "no rule applies" / "legal hold"
    RetainFor(Duration),                                  // policy-driven cleanup target
}

enum Region {
    Eu, Uk, Us, Ch, Br, In, Cn, /* extensible */
    Unrestricted,                                         // explicit opt-out, not absence
}

type RecipientId = String;                                // opaque identifier of an
                                                          // external data recipient
                                                          // (third-party API, SaaS,
                                                          // analytics processor)
```

The trivial values (`NotApplicable`, `Indefinite { reason: "..." }`,
`Unrestricted`) make substrate compliance enforcement a no-op for
deployers who don't need it. A US-only deployment with no
GDPR-equivalent state regulation declares all-trivial values; a
mixed EU/US deployment declares real values and substrate enforces
residency allowlists, suppression-list semantics, and audit-log
discipline against them. Same code path, different values.

## Required metadata

Compliance operations rely on the vocabulary above being present
on every source, schema, and tool. Each field is declared at its
home doc's registration call; substrate startup fails if a
required field is absent.

| Metadata | Doc | Purpose |
|---|---|---|
| `lawful_basis: LawfulBasis` per source | [01](01-event-source.md) | Art. 6 — proves processing legitimacy |
| `collection_purpose: String` per source | [01](01-event-source.md) | Art. 13 — purpose limitation |
| `retention_policy: RetentionPolicy` per source or Owner | [01](01-event-source.md) | Art. 5(1)(e) — storage limitation |
| `data_residency: Region` per source | [01](01-event-source.md) | Chapter V — cross-border transfers |
| `special_category: bool` per payload schema | [03](03-schema-registry.md) | Art. 9 — special-category enforcement |
| `data_residency: Region` per tool | [12](12-tool-manifest.md) | Chapter V — third-country tool calls |
| `recipients: Vec<RecipientId>` per tool | [12](12-tool-manifest.md) | Art. 19 — recipient notification |
| `legal_consequence: bool` per tool | [12](12-tool-manifest.md) | Art. 22 — automated decision-making |

Per-Owner consent state, residency allowlists (`allowed_residencies:
Set<Region>`), and the pause flag live as `compliance.owner_policy`
rows in the audit-side schema, not in `proxima_core`. Engine
consults them on every operator dispatch and tool invocation;
violations are runtime errors, not silent skips.

## Owner policy

`compliance.owner_policy` is the per-Owner runtime overlay that
the substrate's enforcement code reads on every dispatch. One row
per Owner (lazily created on first compliance-relevant write or
operation):

```sql
CREATE TABLE compliance.owner_policy (
    owner_principal_kind   text NOT NULL,
    owner_principal_id     text NOT NULL,
    owner_org_id           text NOT NULL,

    -- Restriction (Art. 18). Operators skip the Owner; reads remain.
    paused                 boolean    NOT NULL DEFAULT false,
    paused_at              timestamptz,
    paused_reason          text,

    -- Residency allowlist (Chapter V). Empty set = no restriction;
    -- otherwise the engine refuses tool calls whose
    -- manifest.compliance.data_residency is not in the set.
    allowed_residencies    text[]     NOT NULL DEFAULT '{}',

    -- Per-Owner override of source-default retention.
    -- NULL = inherit from source declaration; otherwise overrides.
    retention_override     interval,
    retention_override_reason text,

    -- Per-Owner consent state — opaque, controller-managed.
    -- Substrate stores; controller interprets per regime.
    consent_state          jsonb      NOT NULL DEFAULT '{}',

    -- Per-Owner override of the Art. 22 wiring constraint.
    -- See 12 §Compliance metadata; controllers with a documented
    -- DPIA may flip this for specific Owners after consent capture.
    allow_unmediated_legal_consequence boolean NOT NULL DEFAULT false,

    updated_at             timestamptz NOT NULL DEFAULT now(),
    updated_by             text       NOT NULL,

    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id)
);
```

**Read pattern.** Engine reads `owner_policy` once per dispatch
(operator run, tool invocation, source ingest) and caches the row
for the duration of the call. Updates are immediate but the
caller-scope cache means a mid-call mutation is observed at the
next dispatch, not retroactively. Pause / restriction is therefore
not "stop in flight"; it is "stop the next one." For genuine
in-flight termination, the controller invokes `delete_owner` (which
preempts) or waits.

**Write pattern.** Updates go through admin-only RPC surfaced via
[14](14-protocol-surface.md), every write produces an entry in
`compliance.actions` with the diff (old → new) and the requesting
principal. No operator authorship; no flavor extension; no API
exposure to deciders or the cognitive read surface.

**Default-empty semantics.** A fresh Owner with no
`owner_policy` row is treated as the all-permissive default:
not paused, no residency restriction, no retention override, no
consent state, no Art. 22 override. The substrate writes the
default row lazily when the first restrictive update arrives; a
controller running in a non-regulated jurisdiction may never have
any rows in this table at all. The compliance.* schema's tables
exist; their contents are deployment-specific.

The four trivial values from §Compliance vocabulary
(`NotApplicable`, `Indefinite { reason }`, `Unrestricted`,
`SPECIAL_CATEGORY = false`) plus an empty `owner_policy` together
form the substrate's no-op compliance posture: every primitive is
present and typed, but every enforcement path is a structural
no-op until the controller declares otherwise.

## Out of scope

Out-of-substrate by deliberate design — fall to controller, Ops,
or product layer:

- Privacy policy text, consent UX, cookie banners, lawful-basis
  *selection* per data-collection event (substrate enforces the
  declared basis; choosing it is the controller's call)
- DPIA (Art. 35) documentation; processing register (Art. 30)
- Breach detection and 72-hour notification (Art. 33)
- DPO appointment (Art. 37–39)
- SCCs / Adequacy-decision sourcing for cross-border transfers
  (substrate enforces `data_residency` allowlists; the controller
  files the SCCs)
- Children's-data verification (Art. 8) — auth-layer concern
- Anonymisation pipelines (substrate ships pseudonymisation via
  opaque `OwnerId`; full anonymisation of historical exports is
  a downstream pipeline, not a substrate operation)
- Backup, read-replica, and embedding-service-cache cleanup —
  Ops discipline against the metadata the substrate exposes; the
  substrate itself manages only the live store

## Anchors

- `cognitive-vs-compliance`
- `operations`
- `suppression-list--re-ingest-rejection`
- `audit-log`
- `external-side-effects`
- `compliance-vocabulary`
- `required-metadata`
- `owner-policy`
- `out-of-scope`
