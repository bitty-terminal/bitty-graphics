# Component Release Matrix

Status: **implemented** for the `graphics` component (issue
`bitty-terminal/bitty#2491`, milestone 0.0.27).

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

| Target triple               | OS      | Arch    | Native CI runner   | Evidence                                      |
| --------------------------- | ------- | ------- | ------------------ | --------------------------------------------- |
| `x86_64-unknown-linux-gnu`  | linux   | x86_64  | `ubuntu-24.04`     | check, test, clippy, smoke, pack, catalog row |
| `aarch64-unknown-linux-gnu` | linux   | aarch64 | `ubuntu-24.04-arm` | check, test, clippy, smoke, pack, catalog row |
| `aarch64-apple-darwin`      | macos   | aarch64 | `macos-15`         | check, test, clippy, smoke, pack, catalog row |
| `x86_64-apple-darwin`       | macos   | x86_64  | `macos-15-intel`   | check, test, clippy, smoke, pack, catalog row |
| `x86_64-pc-windows-msvc`    | windows | x86_64  | `windows-2025`     | check, test, clippy, smoke, pack, catalog row |

Every row has green PR CI evidence on its native GitHub-hosted runner:
`cargo check` and `cargo test` prove compilation and behavior, clippy denies
warnings on that platform, the packed tarball is unpacked and the packaged
binary passes the protocol smoke before its catalog row is emitted, and only
then is the artifact uploaded.

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

The canonical root `bitty-component.toml` keeps its zero-digest placeholder;
`scripts/pack-component.sh` renders a per-target copy whose `sha256` is that
target executable's digest. The placeholder rule is unchanged: the packer
replaces only that line, and install-time verification refuses any descriptor
whose `sha256` does not match the executable, so the zero sentinel can never
be installed.
The release also writes `release/component-digests.toml`, a generated record
with one row per supported target:

```toml
schema = 1
name = "graphics"
version = "X.Y.Z"
protocol = [1, 1]

[[target]]
triple = "x86_64-unknown-linux-gnu"
tarball = "x86_64-unknown-linux-gnu.tar.gz"
tarball_sha256 = "<tarball sha256>"
executable = "bitty-graphics"
executable_sha256 = "<executable sha256>"
```

`scripts/component-release.sh verify --record release/component-digests.toml`
requires the record to name exactly the supported matrix and every digest to
match the verified artifacts. The record is a release-candidate artifact, not
an R2 object; it is written outside `dist/` so the publisher's exact file-set
gate stays intact.

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

## Protocol smoke

`scripts/component_smoke.py` (through `scripts/component-release.sh smoke`) is
the native-host check that the artifact actually runs, not just that it has a
plausible name. It spawns the unpacked executable, verifies `--version`, then
speaks protocol v1 over stdin/stdout:

1. `Hello` -> `HelloAck` with protocol 1, the descriptor component/version,
   and a non-empty capability set.
2. One raw RGBA `DecodeRequest` (1x1) -> `DecodeResponse` with request id 1,
   RGBA8 format, stride 4, and the exact input pixels.
3. `Shutdown` -> `ShutdownAck`, process exit 0.

Every native CI and release matrix leg runs this against the extracted
tarball before uploading it. A component that starts but cannot complete the
handshake or one decode fails the leg, so no artifact is advertised without
native protocol evidence. `just component-smoke path/to/bitty-graphics` runs
the same check locally.

## Verification gate

`scripts/component-release.sh verify` fails when:

- a supported target has no tarball, no sidecar, or no `SHA256SUMS` entry;
- an unsupported target appears in the manifest or dist;
- the archive is not exactly the descriptor plus the expected executable;
- the embedded descriptor identity/version/protocol or executable digest
  drifts;
- the executable is empty, lacks an execute bit (non-Windows targets), or its
  first bytes are not the expected ELF/Mach-O/PE magic for that target;
- a tarball digest changes between manifest and verification.

`--record` adds the per-target digest-record check. The tag path runs the same
gate after every matrix leg, so a partial or unverified set can never reach
R2.

## CI evidence

`.github/workflows/ci.yml`:

- `component-targets` validates the descriptor, the matrix, the
  at-least-one-supported-target gate, and the CDN URL mapping for every
  supported target, then emits the matrix.
- `component` runs once per supported target on its native runner: `cargo
check --workspace --all-targets`, `cargo test --workspace`, clippy with
  `-D warnings`, packaging, protocol smoke against the packed executable, and
  the per-target catalog row.
- `component-aggregate` merges the target artifacts, regenerates
  `SHA256SUMS`, writes and verifies `release/component-digests.toml`, runs
  the full-matrix verifier (all supported targets present, every archive
  member, digest, mode, and magic checked), and prints the catalog fragment.

`.github/workflows/component-release.yml` repeats the per-target packaging on
the tag path: `cargo check`, `cargo test`, packaging, protocol smoke against
the packed executable, and the per-target catalog row. The tag `package` job
does **not** run clippy; clippy runs in the Linux `verify` job (`just product`,
which also runs `cargo fmt --check` and `cargo test`) and in PR CI. Aggregation
and publish run only after every matrix leg succeeded. The publish job uploads
payloads only; the catalog fragment and digest record are not uploaded (see
above). Local packaging never uploads.

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

# Native-host protocol smoke against the unpacked executable
scripts/component-release.sh smoke --binary path/to/unpacked/bitty-graphics

# Aggregate-only: collect every supported target's tarball into dist first
# (native builds on their own runners), then record and verify the full matrix.
scripts/component-release.sh record --dist dist --output release/component-digests.toml
scripts/component-release.sh verify \
  --dist dist --record release/component-digests.toml
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
  symlinks, stale descriptor versions, digest mismatches, wrong-platform
  executable magic, and non-executable Linux/macOS members.
- The protocol smoke only spawns the artifact it was given, never a shell or
  an interpreter, and stops that child on any failure.
- Publication remains tag-only and read-back verified in
  `scripts/publish-component.sh`; this change does not tag, upload, or mutate
  R2 state.

## Open points

- `x86_64-unknown-linux-musl`: needs a static musl build plus a native Alpine
  test leg.
- `aarch64-pc-windows-msvc`: needs a Windows arm64 native runner and ABI proof.
- The `extensions/catalog.v1` merge job (multi-component) remains a follow-up;
  this repository only proves its own row fragment.
