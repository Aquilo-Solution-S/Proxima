# 13 — The Inverses of Storing

> **Status:** current. Deferred rows are design intent, not implementation claims.

Proxima stores, and for everything it stores it can undo. That is the whole
of what this document describes. It used to describe a compliance regime —
retention windows, legal holds, an audit journal, a lawful-basis vocabulary
and a GDPR mapping table — and every one of those was a judgement about
someone's obligations that the substrate had no way to make.

## The position

An application that hosts Proxima as its memory core makes promises to its
users. Which promises, to whom, under which law, on what schedule, with what
record kept — all of that belongs to the application, because the
application is the only party that knows any of it. A right to erasure with
a thirty-day service level, a contractual retention floor, a litigation hold
served on one tenant: these are facts about a business, not about a store.

So the substrate ships the mechanism and none of the policy:

| Core provides | Host owns |
|---|---|
| a COMPLETE inverse for every declared surface | when the inverse is owed |
| a receipt stating exactly what one operation destroyed or exported | whether that receipt must be kept, and for how long |
| a seam that asks "may this caller erase this owner?" | the answer |
| a refusal when nothing answers | the escalation path when it refuses |

Completeness is the part a host cannot write for itself. Every surface every
flavor declares states its own inverse, and the erase and export are
generated from those declarations, so a table cannot be missed by forgetting
to add it to a list. That property is core's contribution; the rest is the
host's.

## Contract boundary

| Area | Contract |
|---|---|
| cognitive lifecycle | append-only; Facts immutable; A/P/Goals supersede (see [02 §Re-derivation and supersession](02-memory.md#re-derivation-and-supersession)) |
| inverse lifecycle | out-of-band host operation; hard-deletes rows |
| scope | one `Owner`, or one source object inside one `Owner` |
| authorship | a host-authorized principal; never operator-authored |
| operator visibility | diminished graph only |
| protocol | host API; see [14](14-protocol-surface.md) |
| record | the returned receipt; core keeps no history of the operation |

## Operations

Two verbs, each in a four-cell grid of owner kind × scope. The names carry no
adjectives, because a name is not the place to promise a check.

| Operation | Scope | Contract |
|---|---|---|
| `erase_group_owner` | one group `Owner` | destroys every row of the owner across every declared surface; refuses inside the transaction if the group still has members |
| `erase_personal_owner` | one personal `Owner` | the same, and requires the host's drop proof for the named drop event before an authorization is minted |
| `erase_group_source_scope` | one source object inside a group `Owner` | destroys the rows attributable to that source only, under the same live-roster refusal |
| `erase_personal_source_scope` | one source object inside a personal `Owner` | the same, under the same drop proof |
| `export_owner_bundle` | one personal or group `Owner` | a deterministic bundle of every surface the contracts declare exportable |

The preconditions are real and they live in the transaction, not the name.
Group scopes take the membership lock and re-check the roster; personal
scopes require `OwnerDropProofPort` to confirm the drop event. A source scope
is a partial owner: rows that carry no source attribution belong to neither
half of one and are therefore untouched by it.

Every owner is personal or group, so every row sits inside some owner's
erase reach. An owner-to-owner transfer moves that reach rather than escaping
it: the destination can erase the transferred rows and the source no longer
can. See [Consumer Projector
Guidance](reference/public-api.md#consumer-projector-guidance).

Hard deletion also appends a permanent, database-only witness for each erased
Memory or Goal target: its `t` and closed kind. The witness has no owner or
payload and is not an additional node or edge. Other rows that point at the
target retain their `origins[]` and `refs[]` byte-for-byte; erase does not
cascade into or null source declarations. New writes must resolve live target
rows and cannot use or reuse witnesses. An exact cooled restoration may use a
correctly kinded witness under the sealed historical path; legacy cooled rows
with `NULL` pin arrays use ordinary live-target admission.

## The authority seam

`OwnerEraseAuthorityPort` is the provider seam, and the only place the
question "who may erase an owner?" is asked.

| Property | Contract |
|---|---|
| shape | one yes/no question about one target; no reason, no deadline, no policy |
| default | absent port refuses every erase and every export — fail-closed |
| bypass | `AuthPath::System`, the in-process operator path |
| never | `AuthPath::Delegated`; a delegated worker holds a user's authority, and erasing an owner is not among a user's powers |
| export | `may_export_owner` defaults to asking the erase question; a host with a looser portability rule overrides it |

It is narrow on purpose. A port that returned a reason would make core
interpret one, and interpreting a reason is exactly the judgement the
substrate must not make.

## Receipts

Every erase returns what it destroyed; every export returns what it carried.

| Property | Contract |
|---|---|
| erase counts | one entry per `counter` the frozen flavor contracts declare, seeded to zero before the first delete, so "counted none" is distinguishable from "does not count this" |
| export counts | one entry per exported table, derived from the rows beside them — a count that disagrees with its rows is not representable |
| identity | the operation's uuidv7 id, its target, the derived requester and auth path, and the timestamp |
| external debt | `cold_object_purge_pending` and `cited_object_purge_pending` independently report destruction still owed outside the database |
| persistence | none. Core writes no journal row for an erase, a refusal, or an export |

A host that must be able to show what it did records the receipt. A host with
no such obligation records nothing, and pays for nothing.

## What an erase destroys

The inventory is not written here, because a written inventory is the defect
this phase removed. Each surface declares its own `EraseRule`, and the verb
is generated from the declarations:

| Declaration | Statement |
|---|---|
| `ByKey` | deleted through the selection set for its key — memory `t`, goal `t`, or blob id, under the column the key names |
| `ByOwner` | deleted by the surface's own `owner_id`; reached by source scope only when it both retains at source and keys on a memory |
| `Cascade { via }` | no statement at all; the named constraint is the proof, and a test asks the `pg_constraint` catalog whether it exists |
| `Never { why }` | never deleted, with the reason in the declaration — `owners` because seventeen FKs point at it, `cold_purge_pending` because it is the erase's own outbox |

Legs whose statement is not the generic shape — those that enqueue before
deleting, span two selection sets, carry a refcount guard, or rewind a head —
are named in one sorted exemption list beside the code, and a test asserts
that every declared surface is reached by a generated leg, named in that
list, removed by a constraint, or declared a non-erase. There is no fifth
answer.

Two legs deserve prose because their correctness is not local:

| Rows | Contract |
|---|---|
| external objects | each exact `cooled.object_key` or erased `blob_uploads.object_key` is enqueued in `cold_purge_pending` inside the erase transaction and the object is destroyed only after commit. Destroying in-transaction loses the bytes outright on rollback. The queue IS the debt: a row means the object still exists and the erase that promised to reclaim it has not. |
| shared objects | an object key two owners' upload rows both name survives one owner's erase. The candidate set is filtered by an anti-join against the other owner's rows, and under source scope against rows outside the selection — refcount by query, never by counter. |

## The export bundle

The bundle is a table name → rows map plus the pins projected from the
exported memory rows, and its shape is derived the same way the erase is:

| Declaration | Bundle |
|---|---|
| `Rows` | the whole row, `to_jsonb(s)` |
| `Allowlist(fields)` | exactly the named fields — the table is an unsupported persistence detail, the bundle is a supported serialized contract, and a storage-only column added later must not leak into it |
| `Excluded { why }` | absent, with the reason in the declaration |

Row order comes off the declared key. Every exported surface is present even
when it returned no rows, because absence would otherwise be
indistinguishable from a surface the export forgot.

| Section | Contract |
|---|---|
| `cooled` | row metadata including the `object_key` that locates the dumped payload — a manifest, not the bytes; the bundle is a database export and hydration recovers the payload |
| `sketch` | the four declared columns; the generated lexical index is not owner data |
| `delegated_authority_grants` | an explicit field allowlist that excludes the redeemable `delegation_id`; a personal export does not pull group-owned grants merely because the same subject issued them |
| `blob` | `blob_id`, `schema_id`, `content_hash` — enough to identify an opaque CitedObject with no sidecar; upload coordinates and object bytes stay out |
| erased-target witness | never exported; the internal `(t, closed kind)` seal has no owner, payload, or public graph projection |
| owner-pinned sidecars | `mcp_call_logged_v1` carries its own `owner_id`, so it is selected by that column rather than through the Memory: it stays in the bundle of the owner that made the call after the Memory has been transferred away, and out of the receiving owner's |
| gate | system auth path, or `OwnerEraseAuthorityPort::may_export_owner` |

Three surfaces are declared gaps rather than omissions, each carrying its
`why`: `wake_config`, `blob_uploads` and `content` are erased and not
exported. They are named here so the gap is a known one.

The live `MemoryGraphValid`/Fact-grounding contract remains the ordinary live
admission and operator contract. A sealed exact hydrate uses retained graph
validity for recorded target kinds; a committed state after hard erase does
not claim Fact grounding or reconstruct the erased target's owner. Public
reads retain their existing redacted/missing-target behavior, and the internal
witness does not introduce an `Unavailable` projection state.

Memory and Goal admission take a distinct owner fence (and sourced Memory
admission also shares its exact source fence) before first-use owner-row
arbitration and before taking the sorted Memory handle/per-`t` lifecycle locks.
Owner erase takes that owner fence
exclusively; source-scope erase takes the owner fence shared and its source
fence exclusively. The resulting order is owner → source → Memory handle →
lifecycle `t` → rows. A bulk erase takes its scope fence first and only then
selects, so the Memory and Goal scope it erases is exactly the scope in place
when the fence was acquired; it locks the complete selected handle/`t` sets
before deletion, witness, sidecar, or cold-purge work. An admission or
transfer that commits before the fence is inside the erase; one that commits
after it is a write that follows a completed erase. Either way the writer is
whole — the erase never observes a partial one. Transfer exclusively fences
both endpoints in sorted owner order before its complete sorted series
handle/`t` locks and membership reread, so owner- and source-scope erase have
defined boundaries. Per-entity hydration,
forget, and single-entity erase retain their existing per-`t`/handle contract;
repository sweeps share their source-owner boundary through commit but remain
outside this bulk-erase exact-snapshot claim.

## Outcomes

| Outcome | Meaning |
|---|---|
| `Completed` | the transaction committed; the receipt reports counts and any outstanding external destruction |
| `Refused` | a precondition failed — a live roster, or a drop the host would not confirm |
| `NotFound` | the scoped owner or source is absent; no mutation |
| `Unauthorized` | the authority seam said no, or nothing was wired to ask |

Refusal is a valid result, not a failure. Every outcome carries the operation
id the caller was handed, so a host can correlate an attempt that deleted
nothing with one that deleted everything.

## Storage maintenance — `maintain-storage` pass

What survives of the old retention pass is the operator work that is about
the STORE rather than about anyone's rights: draining committed purge debts
and pruning an unbounded log.

| Action | Contract |
|---|---|
| `--retry-cold-object-purges` | retries at most `--batch-size` durable exact-key debts; each object-store deletion runs with no open database transaction, and a short transaction then clears the debt |
| `--prune-change-log-older-than` | deletes `announce` rows older than an explicit operator-supplied age horizon, per owner. There is deliberately no default: destruction requires an explicit flag |
| serialization | one pass at a time via a process-global advisory lock; an overlapping cron fire prints a skip notice and exits 0 |
| cursor safety | pruning creates an undetectable gap for forward pollers whose `since` cursor predates the horizon; choose a horizon comfortably larger than the slowest consumer's lag, or have lagging consumers re-baseline with a fresh full read |
| dry run | `--dry-run` reports would-be pruned and purge-retry counts without mutating anything or requiring an object store |

There is no Fact-retention enforcement. An owner retention window is a
promise about someone's data, made by whoever made it, and the host that made
it schedules its own `forget_memory` calls.

## External side effects

The inverses are substrate-local.

| External state | Contract |
|---|---|
| already-sent email / message / PR / transfer / notice | not rolled back by substrate deletion |
| downstream cleanup | the host's obligation |
| embedding provider egress | Fact and derived text sent to the configured embedding endpoint is disclosure to an external processor. Non-loopback plaintext HTTP is rejected |
| uploaded cited blobs | owner erase removes, in-band, every object named by that owner's `blob_uploads` and `cooled` rows — keys carry no owner, so the rows are the index and a transferred artefact is erased by whoever now holds it. Abandoned uploads whose row is gone are reclaimed by the required object-store lifecycle rule on `pending/` (see [15 §Blob storage lifecycle](15-deployment.md#blob-storage-lifecycle)) |
| legally significant tool calls | require the human-approval flow; automatic blocking is deferred (see [12](12-tool-manifest.md#compliance-metadata)) |

## Declared metadata

There is none. Lawful basis, collection purpose, retention policy, data
residency and recipient inventories were a vocabulary no code path read, and
so was the last survivor: `SchemaContract::special_category` was a per-schema
`bool` that every schema declared, nothing branched on, and no verb, erase
leg, export leg or projection consulted. It is deleted rather than kept as a
marker "a host may key off", because a declaration a host cannot see the
substrate honour is a claim about behaviour that does not exist — and the
kernel deliberately never reasons over special-category
(`docs/lean/COVERAGE.md`, SR-30..33, D16).

A host with Art. 9 obligations keeps the classification where it keeps its
other obligations, and enforces it where it can act on it: at its own ingress
and its own disclosure boundary. What the substrate offers instead is the
mechanism, not the label — a schema whose rows need different handling is a
different schema, with its own surfaces, its own `EraseRule` and its own
`ExportRule`, and those three ARE read.

## Owner policy

| Policy field | Default | Contract |
|---|---|---|
| pause flag | `false` | deferred; paused owners would skip future operator dispatch and wake execution |
| residency allowlist | empty | deferred; empty means unrestricted |
| consent state | empty opaque value | deferred; the substrate would store, the host would interpret |

All three are deferred design intent. There is no retention override and no
legal hold: the first was a schedule and the second a judgement, and neither
is the substrate's to hold.

## Deferred

| Path | Reason |
|---|---|
| `pause_owner` / `resume_owner` | restriction of processing has no implemented enforcement point |
| per-memory cascade delete | requires partial-graph repair and invocation-cache invalidation |
| tool-recipient export from calls | waits for per-call recipient storage (see [12 §Compliance Metadata](12-tool-manifest.md#compliance-metadata)) |
| automatic `legal_consequence` blocking | the human-approval Fact flow remains the required pattern (see [05 §Human approval](05-actions.md#human-approval)) |
| re-ingest suppression after erase | the `Suppressed` error code exists; no durable suppression list does. An erased owner's source may re-ingest, and the host that erased is the party that knows whether it should |

## Anchors

- `the-position`
- `contract-boundary`
- `operations`
- `the-authority-seam`
- `receipts`
- `what-an-erase-destroys`
- `the-export-bundle`
- `outcomes`
- `storage-maintenance--maintain-storage-pass`
- `external-side-effects`
- `declared-metadata`
- `owner-policy`
- `deferred`
