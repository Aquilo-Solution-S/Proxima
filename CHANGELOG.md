# Changelog

Release notes are published in [GitHub Releases](https://github.com/Aquilo-Solution-S/Proxima/releases).

They are generated from Conventional Commits with [git-cliff](https://git-cliff.org/).
Preview the next release locally with:

```sh
scripts/changelog.sh
```

Pass a tag to preview the unreleased commits under that version:

```sh
scripts/changelog.sh v0.0.11
```

Before tagging a release:

1. Update `proxima_core::RELEASE_VERSION` in `crates/core/src/lib.rs` in the
   release-preparation PR. MCP initialization and REST OpenAPI report this value;
   unpublished Cargo package versions remain separate.
2. Require the PR gate to pass against up-to-date `main`, then merge. That gate
   validates the release changes; a second run on the `main` push is unnecessary.
3. Preview the generated notes with `scripts/changelog.sh vX.Y.Z`; check breaking
   changes and migration guidance.
4. Tag the verified `main` commit. The release workflow refuses a tag that differs
   from `v${RELEASE_VERSION}`.

The v0.0.11 Rust SDK changes are documented in the
[migration guide](docs/how-to/migrate-flavor-sdk.md). Existing database migrations
and served MCP/REST tool schemas are unchanged by that SDK overhaul.
