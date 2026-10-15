# Velk name research and initial package

Checked on 2026-10-15.

Published [`velk` version `0.0.0-dev`](https://crates.io/crates/velk/0.0.0-dev)
on 2026-10-15 under `MIT OR Apache-2.0`. The initial reservation is not yanked.

## Registry and existing uses

- The [crates.io API](https://crates.io/api/v1/crates/velk) returned HTTP 404 with
  `crate velk does not exist`. The
  [sparse registry index](https://index.crates.io/ve/lk/velk) also returned HTTP 404.
  This indicates the name was available when checked; availability can change
  until publication succeeds.
- [ljaaskela/velk](https://github.com/ljaaskela/velk) is an existing C++17 component
  object model library with interfaces, properties, events, and plugins.
- [Velk CMS](https://velkcms.com/) is an existing content management system.

The name is short and usable as a Cargo package and binary name, but already has
other software uses. These checks establish neither exclusive naming rights nor
trademark clearance. No repository URL is declared in the package because this
checkout has no configured Git remote.

## Initial release

The publishable package is `crates/velk`, named `velk`, with a literal
`version = "0.0.0-dev"` and `license = "MIT OR Apache-2.0"`. Its version is chosen
locally, without looking up or incrementing a registry version. The workspace's
internal packages also use `0.0.0-dev` and retain `publish = false`.

The reservation has no dependencies, executable, or public API. It does not bundle
the editor's unpublished crates or Git dependencies. Its README identifies it as
an initial reservation for the existing editor project.

`0.0.0-dev` precedes the stable `0.0.0` release. It uses the lowest major, minor,
and patch numbers, but is not the absolute lowest possible SemVer prerelease
(`0.0.0-0` sorts earlier). Cargo's
[prerelease rules](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#pre-releases)
exclude prereleases from ordinary stable-version requirements. Users can still
explicitly opt in, for example with `velk = "=0.0.0-dev"`; this is not a mechanism
for hiding or blocking downloads.

The [crates.io usage policy](https://crates.io/policies) disallows prolonged name
reservations without genuine functionality, purpose, or significant development
activity in the corresponding repository. Keep this package connected to the
actively developed editor and add its public repository URL when available.

## Publish and later yank

Check the archive and perform a verified dry run before publishing:

```sh
cargo package -p velk --list --allow-dirty --locked
cargo publish -p velk --dry-run --allow-dirty --locked
```

The initial publication can include reviewed, uncommitted preparation files:

```sh
cargo publish -p velk --registry crates-io --allow-dirty --locked
```

`--allow-dirty` does not commit or push changes. Subsequent releases should
normally be made from a clean, committed checkout.

To yank this exact version later:

```sh
cargo yank --registry crates-io --version 0.0.0-dev velk
```

Per the [Cargo yank documentation](https://doc.rust-lang.org/cargo/commands/cargo-yank.html),
yanking prevents new dependency resolution from choosing the version. It does not
delete the archive or invalidate existing lockfiles. Publication also makes the
name/version pair immutable: a future release needs a different version, even
after this one is yanked. This preparation does not automatically yank anything.
