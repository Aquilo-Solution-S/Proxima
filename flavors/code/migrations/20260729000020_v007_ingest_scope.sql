-- Code flavor — v0.0.7 lane.
--
-- A repository can say which of its paths it wants indexed.
--
-- Ingest indexed every tracked blob under the size cap, which on a real
-- tree is not the same set as "the code someone will search for". Measured
-- over the three-repo dogfood index: of knip's 4,935 chunks, 3,389 (68.7%)
-- sit under a `fixtures/` or test path, and 3,387 of the deployment's
-- 15,244 embeddings — 22% of the whole index — are that one repository's
-- test fixtures. The operator had no way to say so.
--
-- Two lists rather than one because neither expresses the other concisely:
-- "everything but the fixtures" is an exclude, "only the Rust sources" is
-- an include. A path is in scope when it matches some include (or there
-- are no includes) AND matches no exclude — excludes win, which is the
-- rule every tool with both lists uses.
--
-- Empty defaults, so this migration changes no existing repository's
-- indexed set. Scope is a property of the REPO, not of one ingest call:
-- the incremental poller (`run_poll`) lists arbitrary commit SHAs and must
-- apply the same rule the snapshot did, or a poll would quietly re-add
-- what a snapshot excluded.
--
-- ADD COLUMN with a constant default is metadata-only; no table rewrite.

ALTER TABLE proxima_code.repos
    ADD COLUMN include_globs text[] NOT NULL DEFAULT '{}',
    ADD COLUMN exclude_globs text[] NOT NULL DEFAULT '{}';

COMMENT ON COLUMN proxima_code.repos.include_globs IS
'Gitignore-shaped globs limiting ingest to matching paths. Empty means every path is a candidate. Evaluated before exclude_globs.';

COMMENT ON COLUMN proxima_code.repos.exclude_globs IS
'Gitignore-shaped globs removing paths from ingest. Beats include_globs on conflict. A path that leaves scope is tombstoned by the next snapshot, exactly as a deleted file is.';
