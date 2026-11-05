# Contributing
> Status: current.

Proxiima has two authorities: the Lean kernel under
[`docs/lean/Causa`](https://github.com/Aquilo-Solution-S/proxiima/tree/main/docs/lean/Causa)
defines domainless invariants; Rust code, SQL, tests, and the live MCP/REST
catalog implement and expose them. A prose page explains a contract but does
not amend the kernel. If the implementation and kernel disagree, stop and
resolve the contract before coding.

## Before editing

Read [concepts](concepts.md), the relevant integration page, and the
repository's `AGENTS.md` instructions. Map the data stream from its external input
to its consumer. Join existing contracts instead of adding a second source of
truth. Keep scratch plans, review notes, and bulky evidence under ignored
`.local/`; tracked docs contain durable decisions and executable checks.

The core boundary is deliberate: strict F/A/P/Goal layering, server-resolved
owner scope, typed payloads and sidecars, build-time spiice registration,
atomic command-port writes, and hard deletion only through an authorized
`wipeable` path. Similarity is query-time behavior and never authors an edge.

## Code and tests

Prefer a type that makes an invalid state unrepresentable. Parse untrusted
configuration, network data, database rows, and serialized payloads at the
boundary, returning a typed error. Recheck facts that can become stale at the
commit boundary. Tests should prove intent and boundary refusal, not duplicate
the implementation.

Typed payloads keep their semantic projections typed through authorization,
storage and read models. Protocol JSON is decoded or emitted at the boundary;
inner code does not serialize a payload and inspect JSON to recover natural-key,
display or schema meaning. `PayloadProjection` and the frozen
`NaturalKeyDeclaration` are the compiler-visible seam. Every payload trait
(including cited-object payloads) implements that supertrait; its `KEY_FIELDS`,
`display`, and `durable` methods
are explicit typed fields, not recovered from protocol JSON. Wire names
therefore remain separate from durable identity codecs, which are explicit
compatibility formats. Use `KeyField::padded_text` only for PostgreSQL `CHAR(n)`
columns; ordinary `text` fields must use `KeyField::text`.

`NaturalKeyDeclaration::of::<P>()` is reserved for a stateful Fact whose
projection declares the key; stateless schemas use
`NaturalKeyDeclaration::EMPTY`. PG sidecar installation is an async fallible
boundary: `with_sidecars(...).await` validates declared key columns against
the real SQL schema and must be awaited and propagated during boot.

Do not add a runtime spiice registry, raw spiice SQL against core tables,
caller-supplied owner authorization, a third edge kind, generic JSON payload
escape hatches, or compatibility aliases for removed ontology. If a feature
needs a connection with its own reason or confidence, add the node that owns
that statement; do not add an edge sidecar.

## Migrations

Released migration files are immutable from v0.0.48 onward; a schema fix is a
new migration whose version is above every released version in its directory.
Definitions in `crates/storage-pg/definitions/` are edited in place.
Run `scripts/check-migrations.py` before changing SQL.
At a reset, number the new baseline above the old head, move mutable
objects into definitions, and refuse every earlier ledger with
`SchemaResetRequired`: pre-alpha stores install fresh, and no compatibility
arm stays. Delete the replaced migration files, and prove the new baseline
equals the old head's schema and seed rows. A migration that only a
pgvector-backed store needs has a description starting with `pgvector`; a store
with a host vector index skips it, and its definitions live in
`definitions/vector.sql`.

Before retiring a Fact sidecar, run the erase-capable release with its host
inverses and sidecars still present. Stop producers of the retired schema.
The later schema-only migration uses the existing platform-scoped migration
transaction, at `READ COMMITTED`:

```sql
SELECT proxiima_core.assert_fact_schema_drained(
    'example/retired-v1', ARRAY['proxiima_example.retired_v1']);
DROP TABLE proxiima_example.retired_v1;
DELETE FROM proxiima_core.spiice_surface
 WHERE table_name = 'proxiima_example.retired_v1';
```

Remove the sidecar's generated presence trigger on `proxiima_core.memory` in
that migration as well. The guard requires the non-bypass migration role,
platform scope, and existing declared sidecar names. It holds a memory writer
fence until transaction end and refuses any raw admission naming the schema
or a retiring table, including forgotten versions and rows of other owners.
Keep the guard, DDL, registration removal, and migration ledger commit in the
same transaction; a successful call in an earlier transaction proves nothing
about the later DROP. Admission checks the sidecar's catalog existence as well
as its registration, so a stale writer cannot commit a stamp after DROP. The
guard erases no data and stores no completion flag; its refusal instructs the
operator to run the erase-capable release first. A fresh empty store can
proceed directly.

## Local verification

Use the smallest relevant check during iteration, then the complete gates
before pushing. This is the one list; CI runs the same commands.

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --locked
cargo nextest run --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
cd docs/lean && lake build
cd ../..
python3 scripts/check-migrations.py --self-test
python3 scripts/check-migrations.py
python3 scripts/check-doc-links.py
python3 scripts/check-doc-status.py
python3 scripts/check-settings.py --self-test
python3 scripts/check-settings.py
python3 scripts/check-upgrade-notes.py --self-test
python3 scripts/check-release-version.py --self-test
python3 scripts/check-release-version.py
python3 scripts/verify-culprit.py --self-test
python3 scripts/check-architecture-guardrails.py --self-test
python3 scripts/check-architecture-guardrails.py
python3 scripts/check-sql-policy.py --self-test
python3 scripts/check-sql-policy.py
python3 scripts/check-schema-ids.py --self-test
python3 scripts/check-schema-ids.py
python3 scripts/check-lean-axioms.py --self-test
python3 scripts/check-lean-axioms.py
python3 -m pip install -r requirements-docs.txt
python3 -m mkdocs build --strict
```

Postgres-backed tests need `PROXIIMA_TEST_PG_URL`; the local development
bootstrap is described in [get started](get-started.md).

The suite runs eight test processes at once with no retries
(`.config/nextest.toml`), and they share the server's `max_connections`.
The dev compose file and CI set it to 200. A full run peaks near 60 client
backends; re-measure (`pg_stat_activity`) before raising `test-threads`.
A fixture boot that times out on its pool reports the server's connection
use in its panic.

Every `DROP DATABASE` forces a checkpoint. With `fsync` on, those
checkpoints wait on disk and a full local run pays seconds per drop. The
dev compose file and CI turn `fsync`, `synchronous_commit`, and
`full_page_writes` off on that disposable server; crash safety is
irrelevant for per-test databases. Do not restore them in CI.

The PgBouncer lane runs the storage suites that depend on what a connection
carries between transactions (export/erase fence, maintenance lease, pool
startup parameters) with their serving pools behind PgBouncer 1.26.1 or later
in transaction mode. Set `PROXIIMA_TEST_PGBOUNCER_URL` to that PgBouncer (in
front of the `PROXIIMA_TEST_PG_URL` server, configured like the `pgbouncer`
job in `.github/workflows/ci.yml`) and run the job's nextest filter. Without
the variable, `pgbouncer_pg` returns early and the other suites connect
direct.

The cited-blob tests return early and pass when the `PROXIIMA_S3_*` variables
are unset. A complete run needs an S3-compatible store with a versioned
bucket and the five variables CI sets in `.github/workflows/ci.yml`. CI
serves bucket `proxiima-cited-blobs` at `http://localhost:9000`. Locally, the
dev compose file serves `http://127.0.0.1:9100` (`PROXIIMA_DEV_S3_PORT`
overrides the port) with keys `proxiima` / `proxiima-dev`, and
`docker compose -f docker-compose.dev.yml --profile init up s3-init` creates
its versioned bucket `proxiima-dev`; region `us-east-1` works for both. Report
a run without the variables as skipping that lane. Do not hide a failing
required check. Fix it, record a precise `.local` ledger entry with command
and output, or escalate the blocker.

## Review and release

Keep commits narrow and use the repository subjects: `docs(<scope>): …`,
`feat(<component>): …`, `fix(<component>): …`, or `chore(<component>): …`.
The orchestrating engineer reviews actual diffs, reruns load-bearing gates,
and owns commits, pushes, and publication. Do not commit from delegated work.

Breaking commits carry an `Upgrade:` footer with concrete operator/client
actions. `cliff.toml` renders these in the release notes;
`scripts/check-upgrade-notes.py --self-test` tests the actual parser/template.
Per-release changes live only in those notes.

Changes are pushed to `main` directly; `verified` names the last `main`
commit that passed CI, and new work starts from it. After CI passes, `verify.yml`
atomically advances `verified` and the lightweight `nightly` tag to that tested
commit. `nightly` follows every successful verification; it has no daily schedule
or GitHub Release entry. Routine changes leave `proxiima_core::RELEASE_VERSION`
unchanged.

A numbered release is cut by a deliberate bump of that constant: once
`verify.yml` advances `verified` onto the commit, the value becomes the
`v${VERSION}` tag and git-cliff generates notes. A commit that leaves the
constant unchanged cuts no release.
Run full local gates before pushing, and `scripts/check-release-version.py`
for a version bump. Do not tag by hand.

`nightly` is a moving source selector. Use an immutable numbered tag or commit
SHA for reproducible builds, and keep the same git selector across all Proxiima
dependencies. Cargo locks git dependencies to a commit until `cargo update`.
For a local clone that already has `nightly`, explicitly refresh only that tag
with `git fetch origin +refs/tags/nightly:refs/tags/nightly`. Nightly source trees
retain the last declared release version; identify their exact source by SHA.

For a spiice change, also verify the served catalog and the host e2e path. For
an outbox or host-state change, exercise the transaction and retry or rollback
boundary against real Postgres. For an API change, update the facade rustdoc,
the relevant integration page, and the executable example together.

Continue with [build a spiice](build-a-spiice.md) or [host integration](host-integration.md).
