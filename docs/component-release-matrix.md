# Component Release Matrix

Status: **in progress** for the `graphics` component (issue
`bitty-terminal/bitty#2491`, milestone 0.0.27). The target matrix, packaging,
and verification tooling exist, but the Windows packaging leg has not produced
a green CI run yet, so this page does not claim per-target proof for
`x86_64-pc-windows-msvc`.

This page is the release contract for the native `graphics` component. The
machine-readable source of truth is `component-targets.toml`; this document
explains what it means, how CI proves each target, and which targets are
deliberately excluded.

## Purpose and scope

`graphics` is a host executable, not a Lua plugin. A single tarball cannot run
on every host, so each supported Rust target produces its own tarball, its own
`SHA256SUMS` entry, and its own `extensions/catalog.v1` row keyed by the target
triple. The Core installer resolves the host triple
(`component_seed::host_target_triple`) and requests exactly that artifact.

The matrix covers Linux glibc x86_64/aarch64, macOS arm64/x86_64, and Windows
x86_64. Anything not listed as supported publishes no bytes: a missing row is
an explicit "not installable yet", never a silent fallback to another target.

## Supported targets

| Target triple               | OS      | Arch    | Native CI runner   | Evidence                                         |
| --------------------------- | ------- | ------- | ------------------ | ------------------------------------------------ |
| `x86_64-unknown-linux-gnu`  | linux   | x86_64  | `ubuntu-24.04`     | check, test, clippy, run, pack, catalog row      |
| `aarch64-unknown-linux-gnu` | linux   | aarch64 | `ubuntu-24.04-arm` | check, test, clippy, run, pack, catalog row      |
| `aarch64-apple-darwin`      | macos   | aarch64 | `macos-15`         | check, test, clippy, run, pack, catalog row      |
| `x86_64-apple-darwin`       | macos   | x86_64  | `macos-15-intel`   | check, test, clippy, run, pack, catalog row      |
| `x86_64-pc-windows-msvc`    | windows | x86_64  | `windows-2025`     | check, test, clippy (pack/catalog row not green) |

Four rows have green PR CI evidence on their native GitHub-hosted runner:
`cargo check` and `cargo test` prove compilation and behavior, clippy denies
warnings on that platform, the tarball is packed, and the packaged binary is
executed (`--version`) before its catalog row is emitted. The Windows row
still fails at packaging, so it has no pack or catalog-row evidence yet.

## Documented exclusions

These triples are host triples the Core installer can request, but this
repository publishes nothing for them. They are recorded with a `reason` in
`component-targets.toml`; the verifier refuses any tarball or catalog row for
an unsupported triple.

| Target triple               | Why excluded                                                                                                                     |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `x86_64-unknown-linux-musl` | Alpine/musl hosts request a static musl artifact; the component links glibc, and a musl build needs its own toolchain/ABI proof. |
| `aarch64-pc-windows-msvc`   | Out of the 0.0.27 scope; needs a Windows arm64 native runner and ABI proof before any artifact is published.                     |

## Artifacts and catalog rows

For version `X.Y.Z` and a supported target `T`:

```text
https://cdn.bitty.run/extensions/graphics/X.Y.Z/T.tar.gz
https://cdn.bitty.run/extensions/graphics/X.Y.Z/SHA256SUMS
```

Each tarball contains exactly two members at the archive root: the logical
descriptor `bitty-component.toml` (with the target binary digest) and the
executable `bitty-graphics` (`bitty-graphics.exe` on Windows targets).

`scripts/component-release.sh catalog` emits the registry fragment:

```text
schema 1
extension graphics X.Y.Z aarch64-apple-darwin <tarball sha256>
extension graphics X.Y.Z aarch64-unknown-linux-gnu <tarball sha256>
extension graphics X.Y.Z x86_64-apple-darwin <tarball sha256>
extension graphics X.Y.Z x86_64-pc-windows-msvc <tarball sha256>
extension graphics X.Y.Z x86_64-unknown-linux-gnu <tarball sha256>
```

Rows are sorted by target and their digest is the tarball digest, byte-equal to
that release's `SHA256SUMS` entry. The fragment is verified but **not uploaded**
by this repository: `extensions/catalog.v1` is one shared object across
components, so a single-component fragment would clobber other components'
rows. The future catalog merge job owns that key; until it exists, installs use
the pinned-version fallback described in the Core packaging contract.

## CI evidence

`.github/workflows/ci.yml`:

- `component-targets` validates the descriptor, the matrix, the
  at-least-one-supported-target gate, and the CDN URL mapping for every
  supported target, then emits the matrix.
- `component` runs once per supported target on its native runner: `cargo
check --workspace --all-targets`, `cargo test --workspace`, clippy with
  `-D warnings`, packaging, native `--version` execution, and the per-target
  catalog row. This job currently fails on the Windows target at packaging.
- `component-aggregate` merges the target artifacts, regenerates
  `SHA256SUMS`, runs the full-matrix verifier (all supported targets present,
  every archive member and digest checked), and prints the catalog fragment.

`.github/workflows/component-release.yml` repeats the per-target packaging on
the tag path: `cargo check`, `cargo test`, packaging, native `--version`
execution, and the per-target catalog row. The tag `package` job does **not**
run clippy; clippy runs in the Linux `verify` job (`just product`, which also
runs `cargo fmt --check` and `cargo test`) and in PR CI. Aggregation and
publish run only after every matrix leg succeeded, so a partial or unverified
target set can never reach R2. The publish job uploads payloads only; the
catalog fragment is not uploaded (see above). Local packaging never uploads.

## Local commands

```sh
# Gate: descriptor + release matrix (fails with zero supported targets)
scripts/component-release.sh check

# Supported targets, plain or GitHub Actions matrix JSON
scripts/component-release.sh targets
scripts/component-release.sh targets --format json

# Pack one target (native build required), then verify and emit rows
scripts/package-component.sh --version X.Y.Z \
  --target x86_64-unknown-linux-gnu --output dist/x86_64-unknown-linux-gnu.tar.gz
scripts/component-release.sh manifest --dist dist
scripts/component-release.sh catalog --dist dist --output catalog/catalog.v1

# Release gate: exactly the supported matrix, every archive verified
scripts/component-release.sh verify --dist dist
```

The tooling is Python 3.11+ (`tomllib`) behind `scripts/component-release.sh`,
which resolves `python3` or `python` so the same command works on Linux, macOS,
and Windows runners. It contains no repository-specific literal: the component
name comes from `bitty-component.toml`, the version from that descriptor plus
`Cargo.toml`, and the target set from `component-targets.toml`.

## Security and portability

- No host paths, usernames, credentials, bucket names, or CDN hosts are
  hardcoded in the matrix or the tooling. The CDN URL template is the one
  already owned by `scripts/make-component-dist.sh`.
- Unsupported targets are refused before any manifest or catalog output is
  written; the verifier also refuses wrapper directories, extra members,
  symlinks, stale descriptor versions, and digest mismatches.
- Publication remains tag-only and read-back verified in
  `scripts/publish-component.sh`; this change does not tag, upload, or mutate
  R2 state.

## Open points

- `x86_64-unknown-linux-musl`: needs a static musl build plus a native Alpine
  test leg.
- `aarch64-pc-windows-msvc`: needs a Windows arm64 native runner and ABI proof.
- `x86_64-pc-windows-msvc`: the packaging leg must go green (pack tarball,
  execute the binary, emit the catalog row) before this page claims that row
  as proven.
- The `extensions/catalog.v1` merge job (multi-component) remains a follow-up;
  this repository only proves its own row fragment.
