# Releasing

Releases are driven by a pushed version tag such as `v0.4.0`. Before the first
automated release, configure `expensive` on crates.io with a GitHub trusted
publisher using:

- repository: `lindestad/expensive`
- workflow: `release.yml`
- environment: `crates-io`

The workflow exchanges GitHub's OIDC identity for a short-lived crates.io token;
no long-lived registry secret is required.

For each release:

1. Update `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, and user documentation.
2. Run `cargo fmt --check`, Clippy, tests, the MSRV check, and
   `cargo publish --dry-run --locked`.
3. Merge the release commit to `main`.
4. Create and push the matching tag, for example `git tag v0.4.0` followed by
   `git push origin v0.4.0`.

The tag workflow verifies that the tag matches the crate version, publishes to
crates.io, builds native archives for Linux, macOS, and Windows, and creates the
GitHub release with generated notes.
