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

## Cutting a release

**Merging cuts the release; nobody tags by hand.** A PR that bumps
`proxima_core::RELEASE_VERSION` in `crates/core/src/lib.rs` becomes
`v${RELEASE_VERSION}` the moment it lands on `main`, with notes generated from
the commits since the previous tag. A merge that leaves the constant alone cuts
nothing, so ordinary changes accumulate until a release is worth naming.

1. Preview what the next release would say: `scripts/changelog.sh vX.Y.Z`. Check
   breaking changes and migration guidance.
2. Bump `RELEASE_VERSION` in the release-preparation PR. MCP initialization and
   REST OpenAPI report this value; unpublished Cargo package versions remain
   separate. `scripts/check-release-version.py` runs in the PR gate and accepts
   only the next patch, minor, or major of the highest existing tag.
3. Merge through the required gate against up-to-date `main`. The tag and the
   GitHub Release appear from that push.

A tag pushed by hand still works — the same workflow runs on a `v*` push and
refuses any tag that disagrees with the tagged tree's `RELEASE_VERSION`.

## Consuming Proxima

Pin every Proxima crate in a consumer with the **same selector form**, or
`proxima-core` resolves twice:

```toml
# Track a named release.
proxima = { git = "https://github.com/Aquilo-Solution-S/Proxima", tag = "v0.0.14" }

# Track the edge. Cargo.lock still pins one exact commit; `cargo update -p
# proxima` is how you move. There is no moving `latest` tag and there will not
# be one — a tag names one commit forever.
proxima = { git = "https://github.com/Aquilo-Solution-S/Proxima", branch = "main" }
```

Upgrade guidance: [v0.0.12 SDK and runtime changes](docs/how-to/migrate-flavor-sdk.md#v0012),
[additive database migration](docs/how-to/migrations.md#v0012), and
[Fact outbox setup](docs/how-to/fact-outbox.md).
