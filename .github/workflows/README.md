# CI and release workflows

Two workflows build and ship crack.

## ci.yml

Runs on every push and pull request. One matrix job builds, tests, and lints the
Rust workspace on each shipped target, each on a native runner:

| Target                    | Runner           |
| ------------------------- | ---------------- |
| x86_64-unknown-linux-gnu  | ubuntu-24.04     |
| aarch64-unknown-linux-gnu | ubuntu-24.04-arm |
| x86_64-apple-darwin       | macos-13         |
| aarch64-apple-darwin      | macos-14         |
| x86_64-pc-windows-msvc    | windows-latest   |

Native runners mean `cargo test` runs the real binary for each target, not an
emulated one. The linux arm64 job uses the hosted ubuntu arm runner, which is
free for public repos; if the repo goes private, switch that row to cross or
qemu.

Steps per target: checkout, install the stable toolchain plus the target, cargo
build --release, cargo test, cargo clippy with warnings denied, cargo fmt
--check.

A second job typechecks `npm/` with tsc, but only once `npm/tsconfig.json`
exists, so it stays green while that package tree is still being built.

## release.yml

Runs on a pushed tag matching `v*`. The published version is the tag with the
leading `v` removed.

1. A build matrix compiles the release binary for each of the five targets on
   its native runner and uploads each binary as an artifact named after its npm
   package.
2. A publish job downloads every binary, drops it into the matching
   `npm/crack-<os>-<cpu>/bin/` (`crack.exe` for windows), sets every package
   version to the tag, pins the launcher's optionalDependencies to that version,
   then publishes. Per-target packages publish first, the launcher last.

Triple to npm package:

| Rust triple               | npm package               | binary        |
| ------------------------- | ------------------------- | ------------- |
| x86_64-unknown-linux-gnu  | @sysce/crack-linux-x64    | bin/crack     |
| aarch64-unknown-linux-gnu | @sysce/crack-linux-arm64  | bin/crack     |
| x86_64-apple-darwin       | @sysce/crack-darwin-x64   | bin/crack     |
| aarch64-apple-darwin      | @sysce/crack-darwin-arm64 | bin/crack     |
| x86_64-pc-windows-msvc    | @sysce/crack-win32-x64    | bin/crack.exe |

## Trusted publishing

Publishing uses npm trusted publishing over OIDC from this repo's Actions. No npm
token lives in CI. The publish job declares `id-token: write`, upgrades npm to
11+, and runs `npm publish` with no `NODE_AUTH_TOKEN`; npm exchanges the OIDC
token for publish rights and records provenance.

Set this up once on npmjs.com: for the launcher and each per-target package, add
a trusted publisher pointing at `Fracital/crack` and the `release.yml` workflow.

## First publish is manual

Trusted publishing needs the package record to exist first. So the first version
of the launcher (`@sysce/crack`) and of every per-target package
(`@sysce/crack-<os>-<cpu>`) is published by hand as the `e9x` npm login:

```
cd npm/<package> && npm publish --access public
```

Do that for all six packages once. After the records exist and the trusted
publishers are configured, release.yml handles every later publish on tag.
