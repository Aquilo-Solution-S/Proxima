# Changelog

Release notes are published in [GitHub Releases](https://github.com/Aquilo-Solution-S/Proxima/releases).

They are generated from Conventional Commits with [git-cliff](https://git-cliff.org/).
Preview the next release locally with:

```sh
scripts/changelog.sh
```

Pass a tag to preview the unreleased commits under that version:

```sh
scripts/changelog.sh v0.0.12
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

Upgrade guidance: [v0.0.12 SDK and runtime changes](docs/how-to/migrate-flavor-sdk.md#v0012),
[additive database migration](docs/how-to/migrations.md#v0012), and
[Fact outbox setup](docs/how-to/fact-outbox.md).
