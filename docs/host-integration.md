# Host integration
> Status: current.

An embedded host composes Proxima with its own authentication, HTTP router,
extra stores, publication intake, and lifecycle policy. The `proxima` facade
is the host boundary. A flavor receives typed services; it does not receive
raw core tables or resolve owners itself. See [concepts](concepts.md) for the
boundary and [configuration](configuration.md) for the settings inventory.

Read and write projector cursors through `Engine::load_source_cursor` and
`Engine::store_source_cursor`, using the request's `AuthzContext`. Goal display
fields are validated by `GoalPayloadWrite::from_payload`; its
`GoalWriteBuildError` carries the typed refusal. `GoalTopologyWrite::new`
takes an optional assignment: pass `Some(GoalAssignmentTarget::perspective(id))`
for a supplied Perspective or `None` for an unassigned goal. Evidence may be
empty, including when marking a goal achieved. HTTP routes are listed in
[protocol](protocol.md).

## Boot a composed host

Implement `FlavorApp` for the application bundle. Defaults are applied first;
environment and explicit builder calls then overlay them.

```rust
use proxima::{AppInfo, FlavorApp, RuntimeBuilder};
use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use std::net::SocketAddr;

struct MyApp;

impl FlavorBundle for MyApp {
    fn register(_: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> { Ok(()) }
    fn migrators() -> Vec<NamedMigrator> { Vec::new() }
}

impl FlavorApp for MyApp {
    fn app_info() -> AppInfo {
        AppInfo { id: "my-app", title: "My App", version: env!("CARGO_PKG_VERSION") }
    }

    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder.store_number(3).mcp_bind(SocketAddr::from(([127, 0, 0, 1], 31415)))
    }
}

#[tokio::main]
async fn main() -> Result<(), proxima::ProximaError> {
    proxima::run::<MyApp>().await?;
    Ok(())
}
```

The host normally implements `FlavorBundle` and links one or more flavors;
the tuple `FlavorApp` implementation folds configuration, services, and HTTP
mounts left to right. `build()` gives a booted runtime for embedding; `run()`
also starts configured listeners and background workers. Authenticated MCP
requests carry a server-resolved `AuthzContext`; callers never supply a
resolved owner or role.

With an object store configured, the runtime sweeps expired uploads and drains
object-purge debt itself (`RuntimeBuilder::object_purge_interval`, default 60
seconds, zero turns it off); a host built on the runtime writes no code for it.
A library host that composes `PgStorage` and `CitedBlobStore` without the
runtime calls `proxima::object_maintenance_pass` on a cadence no longer than
the upload TTL. The pass takes the same storage maintenance lease as the
runtime and `maintain-storage`, and skips when another pass holds it. Both
the runtime's boot and the pass refuse while the purge queue holds debt for
another object store; see [operate](operate.md).

Migration ledgers are checked before any schema change. A recorded version
must be shipped by a composed source using that same ledger; sources sharing
a ledger are checked as one set. Separate ledgers are never populated from
another ledger.
Databases with old copies recorded on both shared and separate ledgers also
require a reset; there is no copy-cutover compatibility path.

A host that deliberately shares a database between releases can explicitly set
`RuntimeBuilder::missing_migration_policy(MissingMigrationPolicy::Allow)`.
The deployable host and its maintenance commands read the same choice from
`PROXIMA_MISSING_MIGRATION_POLICY`, as does `tools/dev-migrate`. The same
policy applies when boot skips migrations. Standalone migration and
planning callers use the corresponding `_with_policy` functions; the ordinary
functions reject missing versions. Allowing missing versions still rejects
dirty ledgers, changed checksums, and migration lanes that would replay or
reorder previously applied work.

Read-only boot and planning share the runner's migration lock so they cannot
validate between source commits. They fail after a five-second lock wait.
Complete the out-of-band migration job before starting pods that skip
migrations; allowing missing versions does not bypass this serialization.

Standalone migration and preflight callers freeze the same core/flavor PG
sidecars as boot. Register core sidecars and the bundle's sidecars in a
`PgSidecarRegistry`, then call `freeze_against(&registry)` with the frozen
flavor registry. Pass `&sidecars` as the third argument to
`run_core_and_flavor_migrations` and `preflight_without_migrations`; their
`_with_policy` forms take it before the policy. `HostTemplate::new` takes the
owned frozen sidecars as its fourth argument and includes the composition in
its template identity. These types and migration operations are available
through `proxima`; test templates use `proxima::testkit`.

The runner derives Follows owner RLS from this registry before and after the
flavor migration lanes, including runs with no pending SQL. Skip-migration
preflight checks the generated marks and owner indexes without changing them.
Normal boot supplies this registry automatically. See
[sidecar owner modes](build-a-flavor.md#2-persist-the-typed-payload).

When a host first runs this runner on an existing database, three
things change. A Follows table gets the host rule (its `owner_id` AND its
memory) whatever class the flavor's SQL gave it, so owner reads stop
returning rows of forgotten memories. A Follows table without an index
leading with `owner_id` gets `<table>_owner_follows_idx`, built inside the
migration transaction with a plain (non-concurrent) `CREATE INDEX`, which
blocks writes to that table while it builds. Skip-migration boot and
`preflight_without_migrations` refuse with `MigrationError::SidecarOwnerRls`
until one migration run with the composition's frozen sidecars has written
the marks and indexes. Standalone migration jobs must pass that registry,
not `PgSidecarRegistryFrozen::default()`.

## Authentication and owner scope

Provide an `Authenticator` through `RuntimeBuilder::authenticator`, or use the
OIDC adapter from `auth-oidc`. The authenticator validates the bearer, maps
`(iss, sub)` to a `UserId`, and produces owner access for the request. The
`X-Proxima-Owner` value is a raw owner UUID selected from that authorized
scope. It is not an authorization proof.

The stock `OidcAuthenticator` and `OidcBindingSet` retain the verified token
payload on that request's `AuthzContext`. A Flavor handler reads it through
`ctx.authz().verified_claims()`, which borrows an immutable JSON claim map
without exposing the bearer. The map includes registered claims; the host or
Flavor interprets its application-specific fields. Owner selection and
narrowing preserve the map, while each authentication supplies its own values.
Custom authenticators may attach their already verified payload with
`AuthzContext::with_verified_claims`; contexts without one return `None`.

The scope gate judges the caller's role on the owner the call's `space`
resolves to.

Tool admission combines two distinct policies. `ServedSurface` configures a
deployment through `RuntimeBuilder::served_surface`; every declared read-only
tool/action and core resource remains served. `ToolScope` is the authenticated
caller's exact key set, including reads and resources. `CapabilitySet` applies
both policies before the independent owner-role gate. Neither policy supplies
owner authority.

A dispatcher requires its declared action key, such as `core_upload:read_url`.
A resource requires its template or static key, such as `resource:memory`;
`resource:how-to` controls the guide too. A bare dispatcher does not grant its
actions. Listings and calls use the same actual declaration, including dynamic
host tools. Custom role shaping may narrow each policy without converting one
into the other. Replace deployment `.tool_scope(...)` calls with
`.served_surface(ServedSurface::...)` and `tool_palette_excluding` with
`served_surface_excluding`; retain `ToolScope` for caller scopes.

An embedded host may install an `OwnerAccessPort` and an optional forwarder
policy. Reads are set-based and owner-scoped; writes pass through the engine's
authorization and command ports. Do not recreate owner predicates in a flavor
or add a caller-controlled ACL/share set.

For subjects in `PROXIMA_FORWARDER_SUBJECTS`, the signed token must carry an
`owner` UUID claim, a `by` principal UUID and an explicitly present `via`
claim containing a session UUID or `null` for a direct principal call.
Missing or malformed writer claims refuse authentication. The forwarder
remains the authenticated subject; its
`PROXIMA_FORWARDER_ROLE` applies only to that registered personal or group
owner. An absent owner header uses the claim. A present `X-Proxima-Owner`
header and any live session must agree with it before the target is resolved.
An unknown owner grants no role. Ordinary subjects ignore the claim.

With the PostgreSQL registry, a personal owner is registered by its first
ordinary authenticated write. Complete that write before forwarding to a new
person, or supply an authoritative `OwnerRegistry` for provisioned owners.
Forwarding does not register an unknown owner.

Forwarding never unions ordinary memberships into the request, grants group
management, or permits delegation. `ForwarderRole` is the closed choice
`Viewer`, `Ingest`, or `Editor`; none writes Goals or manages groups.
`OwnerRegistry` probes both owner kinds. Custom authenticators use
`AuthzContext::from_verified_forwarded_owner` only after verifying the
credential. That pending context grants no roles until the edge resolves its
claim through the configured policy and registry. Host role shaping must
preserve the owner and writer bindings. Replace implementations of the retired `GroupRegistry`
with `OwnerRegistry::is_registered_owner` when upgrading.

Each memory or Goal revision records the writer captured in its sealed
`OwnerScope`, separately from its owner, credential subject, payload author
or grounding sources. A custom authenticator binds `WriterAttribution` with
`AuthzContext::with_attested_writer` before `proxima::authenticate` seals the
context. For a forwarder it supplies the verified pair to
`AuthzContext::from_verified_forwarded_owner(subject, owner, writer)`.
Ordinary OIDC calls record their mapped principal with `via: null`.
There are no author headers or caller-supplied author arguments. A host role
shaper must preserve the attested pair, and a sealed context cannot replace it.

Attaching a subgroup or changing its relation requires manage authority on
both groups. Detaching it requires manage authority on either endpoint;
removing a person requires manage authority on the parent group. PostgreSQL
write policies enforce these boundaries. Lock requests retain both endpoints
for tree serialization; their read and write policies require manage authority
on either endpoint.

For authority facts on a host-owned transaction, call
`PgPlatformScope::authority_on(&mut tx, targets)` with an already open
`READ COMMITTED` transaction. Declare the complete group, person and
registered-owner footprint in `AuthorityTargets`, including membership-edit
endpoints, before taking host revision or claim mutation locks. Construction
acquires the existing tree and subject fences and rechecks the root map before
returning a `PgAuthoritySession`. A changed footprint returns `Retryable`; a
newly discovered target requires restarting the whole transaction with the
complete selection. This trusted platform path works before authentication and
requires no `AuthzContext`; its facts grant no authorization.

`session.group(id)` and `session.person(id).await` return views that query fresh
facts, including earlier writes on that transaction. Direct membership rows
carry `Relation`; `effective_roles().await` includes inheritance.
`session.registered_owner(id).await` reads the registered owner kind;
`session.containment(ancestor, descendant).await` reads directional group
containment with a distinct `SameGroup` result. Group principals remain groups;
they are refused by the person view. Unknown registered owners refuse lookup.
The caller retains its SQL, conditional writes, commit and
whole-transaction retry; dropping the session only releases the Rust borrow,
while PostgreSQL keeps its fences until commit or rollback.
`add_group_member` returns `MembershipAddOutcome::Changed` or `AlreadyPresent`
under its write fence, so a host can update a revision only for a change.

A host that only needs a person's direct relations, without the session's
fences, calls `direct_group_relations(&mut *tx, person).await` on its own
transaction. It returns one `DirectGroupMembership` per membership row, in
group and relation order: nothing inherited, and several relations in one
group stay separate entries instead of folding into a role (`group_role` gives
that). It takes no lock, so it never waits for another connection's membership
edit, and it sees the caller's own uncommitted rows.

Inside an authenticated Proxima write callback, use
`PgHostStateSession::authority_memberships` for the current subject's effective
memberships instead. That session derives its subject from the sealed write
permit; it does not accept the platform gateway's caller-selected footprint.

## Other stores and HTTP

Use `FlavorApp::services` to expose a typed service, and
`FlavorApp::mount_http` to add host-owned routes. The host accessor provides a
pool and the resolved query policy:

```rust
fn services(ctx: &AppContext) -> Result<FlavorServices, FlavorServiceError> {
    let store = MyStore::new(ctx.host().clone_pool_for_host());
    Ok(FlavorServices::with(store))
}
```

Every memory handle is a bare UUID. The host assigns each store a unique
16-bit number among the stores it routes between; configure it with
`RuntimeBuilder::store_number` or `PROXIMA_STORE_NUMBER`. First boot persists
the number; a later mismatch refuses startup. UUIDv8 handles carry that number
and sort by timestamp and counter. Start fresh stores for this release.

For a store accessed by flavor workers, register an `OtherStores` port with
`RuntimeBuilder::other_stores`; the facade wraps it as a typed service with the
configured timeout. The extra-store transaction is separate from a Proxima
`UnitOfWork`; use host-state participation when the change must be atomic with
a Fact write. A flavor never queries `proxima_core` directly.

## Vector index

By default a store keeps its chunk vectors in its own database, written in the
transaction that changes their rows. A host can keep them in an index outside
it instead, one index serving several stores: implement the `VectorIndex` port
(`put`, `delete`, `set_owner`, `nearest`, `entries`) and register it with
`RuntimeBuilder::vector_index`. `InMemoryVectorIndex` is the reference
implementation. The store's database still needs the vector extension.

```rust
let index = VectorIndexClient::new("https://index.internal", store_number, &plaintext_hosts)?
    .with_bearer_token(token);
let builder = RuntimeBuilder::default().vector_index(std::sync::Arc::new(index));
```

`proxima::vector_index_http` is the network form: `VectorIndexClient` is a
`VectorIndex` over HTTP, and `vector_index_router` serves any `VectorIndex` to
the stores it names, under the host's own authentication. Its rustdoc lists the
wire. The client speaks plaintext only to loopback or a named plaintext host.

The store keeps each entity's head, its current version per space, and calls
the index in this order:

- the embedding drain writes the head and `put`s the vectors before its claim
  commits, so a job completes only once the index has them;
- every later head change (forget, erase, transfer, a space purge, the orphan
  sweep) leaves durable debt in its own transaction, paid at the start of the
  next drain pass;
- an erase pays its owner's debt before it commits, including what an earlier
  forget left for the drain: an index failure fails the erase and its
  transaction rolls back, while what the index already deleted stays deleted
  until the sweep finds it and reconciliation embeds it again;
- the orphan sweep compares the index's `entries` with the heads, deletes
  entries no head names, moves entries to their head's owner, and deletes heads
  the index lost so reconciliation embeds them again.

Every method is idempotent and may be repeated after a failure. A `put` the
client gave up on can still reach the index after a later `delete`; the sweep
deletes such an entry, since no head names it. Index hits are
candidates either way: the store reads their memory rows under owner RLS before
it uses their IDs or scores, so a wrong or stale entry costs results, never a
row.

Derivation reads all foreign origins within one call-wide deadline and refuses
before writing if they cannot all be read. Lineage deduplicates foreign origins
across the page, bounds distinct reads, and renders unread origins as unavailable
with a reason. The limits and concurrency contract live in
[`other_stores` rustdoc](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/storage_ports/other_stores.rs).

## Fact publication outbox

Implement `DurableIntake` in the host and register it with
`RuntimeBuilder::outbox_intake`, or register named consumers with
`outbox_consumer`. A captured listenable Fact is committed to the outbox in
the write transaction. A consumer receives it at least once; deduplicate on
the event id. `Ok(Accepted)` and `Ok(Rejected)` mark a record published, while
an intake error releases it for retry. Each record is its own `accept` call; the
marks for a claimed batch are written in one transaction, at the end of the batch
or after half the lease, whichever is first. No consumer means Facts are still
captured until the configured pending bound is reached.

Erase and lease takeover are checked twice: just before `accept`, and again
when the record is marked published, under the lane lock and in the marking
transaction. A record erased or reclaimed by another process before `accept`
is not handed over. If that happens while `accept` is running, your intake
already has the event: Proxima cannot take it back, and it does not record the
record as published. Treat that as at-least-once delivery.

```rust
let builder = RuntimeBuilder::default()
    .outbox_intake(std::sync::Arc::new(MyIntake::new()))
    .published_retention(std::time::Duration::from_secs(86_400));
```

Bind publication source, model provenance, and host-bound extensions once on
the authenticated context. The host owns consumer ledgers, retention, and
actor attribution. The typed `PublicationRecord`, `DurableIntake`, and delivery outcomes own the
CloudEvents envelope and retry contract in rustdoc. Scheduling, execution, and
call logging belong to the host; the outbox is its change channel.

The built-in HTTP sink reads `PROXIMA_OUTBOX_HTTP_URL` and delivers each event
to that receiver without following redirects. Plaintext HTTP requires loopback
or an exact host entry in `PROXIMA_PLAINTEXT_HOSTS`, whether or not a token is
configured. See [configuration](configuration.md) for the token-file and CA
settings.

## Host-state participant

Register one `PgHostStateParticipant` with
`RuntimeBuilder::host_state_participant`. Boot binds it to the actual
participant descriptor and declared tables. A `UnitOfWork::apply_host_state`
command carries one owner and a duplicate-free subset of those tables; the
private maintenance permit checks participant, table, and owner before the
handler runs. A dispatcher can route several typed command payloads without
creating another authority path.

This capability only maintains the host's registered state. It does not grant
Fact, Abstraction, Perspective, or Goal writes and does not bypass ordinary
owner authorization. The callback receives a `PgHostStateSession`; use
`session.transaction()` for host SQL and honor the stamped owner and declared
table scope.

An owner-authorized host command can call
`session.authority_memberships().await?` to read the authenticated subject's
complete current group memberships, including inherited roles, even when
the memory unit selects only the subject's personal owner. The returned
`SubjectMemberships` is an answer, not an owner authorization witness. The
port takes no subject or authority-scope argument and grants no membership
mutation or additional memory reach. Maintenance, delegated, and forwarded
commands cannot use this reader.

The reader holds the subject's existing projection fence shared until the
unit commits or rolls back. Membership changes and nested-group changes
that affect that subject hold it exclusively, including the first membership
added to an empty projection. Reads require `READ COMMITTED`; a read after
waiting observes the preceding writer's committed projection. Keep the unit
short and do not hold it across network calls. The participant and
transaction-scoped capability types own the API in rustdoc; facade PG tests
exercise scope, atomicity, and both concurrency orders.

## Erase and export callbacks

Register lifecycle callbacks on the same boot-frozen participant when its
declared tables need owner, source, or exact-Fact maintenance. Core performs
authorization and acquires the lifecycle fence; the callback receives a typed
selection and returns receipts for the declared tables. Whole-owner, source,
flavor-scoped, and exact-physical-Fact scopes remain distinct. Export is a
read path under the caller's authorization; it does not grant erase authority.

An erase commits core inverse work and the participant callback atomically. A
connection loss after commit may leave the caller's observed result unknown;
retry using the operation's idempotency boundary. For hard deletion rules see
[data and erasure](data-and-erasure.md).

## Verify an embedded host

```sh
cargo check -p my-host
cargo test -p my-host
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
```

Start the host against a local database, inspect `tools/list` and the resource
catalog, and exercise one authorized read, one Fact write, one outbox intake,
and one lifecycle callback. Run the full workspace gates before publishing;
see [contributing](contributing.md).
