# bitty-graphics

Bounded graphics decode extension crate (landed CTX-0003, independently verified CTX-0004). Bounded PNG and raw RGB/RGBA decode plus the raster mechanics subset; placement policy, rect helpers and renderer upload validation stay in Core. Read [AGENTS](AGENTS.md). Task management lives in CarryCtx.

Prerequisite: W-133 accepted contract specifications/graphics-extraction-contract.md (bitty-terminal-docs specify c28681db closed #170, promotion 3479ce12 closed #184). Bounded protocol intake, placement, resource enforcement and upload validation remain Core responsibilities unless an accepted contract says otherwise.

Local phases CTX-0001 -> CTX-0002 -> CTX-0003 -> CTX-0004 map to Issues #4 -> #3 -> #2 -> #1. CarryCtx reports all four phases complete; GitHub Issues #4, #2 and #1 are closed. This change closes #3 on the accepted W-133 contract.

## Component distribution (0.0.25)

Component name `graphics` (owner decision 2026-10-16: strip the `bitty-` prefix, per the `net` precedent). The packed binary is `bitty-graphics`; the install spelling is `bitty extension install graphics --version 0.0.25`.

The packed binary serves a bounded stdio decode protocol. This lands the serve loop graphics#17 asked for; the Core-owned host client and composition-root binding remain follow-up work (bitty#2163).

### Binary surface

- `bitty-graphics` (no arguments) serves protocol v1 on stdin/stdout. stderr is diagnostic only and never protocol.
- `bitty-graphics --version` / `-V` prints `bitty-graphics <version>`.
- `bitty-graphics --help` / `-h` prints usage. Any other argument is refused with exit 2.
- No configuration file, environment variable, path, network, or further argv is read. The wire is the only request input.

### Wire protocol v1

Framing is `length:u32be` followed by `payload[length]`; the payload starts with `tag:u8` and fixed fields. Integers are big-endian. Strings are `len:u32be` plus UTF-8 and every length is bounded. Optional values are `0x00` or `0x01` plus the value. `length` is rejected from the four-byte header alone when it is zero or above the direction cap; no payload byte is read or allocated.

1. The host sends `Hello` (`min:u16 max:u16 component:str version:str mode:u8`) as the first frame.
2. The component answers `HelloAck` (`protocol:u16 component:str version:str capabilities:u8` plus the capability block below). `protocol` is `1`.
3. The host sends `DecodeRequest`: non-zero `request:u64`, transmit format (`100` PNG, `24` raw RGB, `32` raw RGBA), optional declared `width`/`height`, declared output `budget_bytes`, `deadline_ms`, and the opaque image `payload`.
4. The component answers one `DecodeResponse` with normalized RGBA8 pixels (`format = 1`, `width`, `height`, `stride = width * 4`, `rgba.len() = width * height * 4`) or a typed `Error` carrying the same request id.
5. `Shutdown` writes `ShutdownAck` and exits 0; stdin EOF also exits 0 without a frame.

Advertised capability block (`HelloAck`, in order): request frame bytes, response frame bytes, payload bytes, max image dimension, max image pixels, max decoded bytes, max concurrent decodes, rate capacity, rate per second.

| Bound                     | Value                     | Failure behavior                                               |
| ------------------------- | ------------------------- | -------------------------------------------------------------- |
| Request frame payload     | 4 MiB + 1 KiB             | connection-level `Error`, exit 3                               |
| Response frame payload    | 64 MiB + 1 KiB            | encoder refuses; the component never emits it                  |
| Image payload per request | 4 MiB (`IMG-1`)           | connection-level `Error`, exit 3                               |
| Image side                | 8192 px                   | `budget`, connection stays open                                |
| Image area                | 4096 x 4096 px (`IMG-2`)  | `budget`, connection stays open                                |
| Decoded bitmap            | 64 MiB RGBA (`IMG-3`)     | `budget`, connection stays open                                |
| Decode concurrency        | 1 (sequential loop)       | requests are pipelined, never decoded in parallel              |
| Request deadline          | 1..=30,000 ms             | field refused; an elapsed deadline returns `timeout`           |
| Request rate              | 120 burst / 60 per second | `rate_limited`, 16 consecutive violations close the connection |
| Error message             | 256 bytes                 | decoder refuses                                                |

Typed error kinds: `denied` (safe mode or withheld capability), `invalid_request`, `budget`, `unsupported` (transmit format), `protocol` (framing/handshake), `timeout`, `internal`, `rate_limited`, `malformed_image`. A malformed image, over-cap dimensions, or length mismatch is a per-request error: the connection stays open and the next request is served. Framing, handshake, and message-schema violations are connection-level errors that exit 3. A declared output budget above the component ceiling is clamped down to it; a decoded bitmap larger than the effective budget is refused with `budget`.

`Hello.mode` bit `0x01` declares safe mode and bit `0x02` declares the host granted the decode capability. Safe mode, a missing grant, or both make the session deprived: the handshake completes with zero capabilities, decode returns `denied`, and shutdown still works. The component never self-grants. v1 has no raster request, response paging, or cancellation; those stay with the Core binding.

The full schema and module-level contract live in [src/component](src/component/mod.rs); it is the authoritative codec, and every bound is checked on both encode and decode.

### Packaging

- Descriptor: [bitty-component.toml](bitty-component.toml) is the canonical record of the fixed fields (`name`, `version`, `protocol = [1, 1]`, `executable`). The `sha256` there is a placeholder; `scripts/pack-component.sh` renders the per-target digest at pack time and refuses to pack on any other drift.
- Pack locally: `just component-pack x86_64-unknown-linux-gnu` writes `dist/<target>.tar.gz` plus the sidecar and `SHA256SUMS` via the vendored `scripts/make-component-dist.sh`. Windows archives contain `bitty-graphics.exe`; Linux and macOS archives contain the extensionless `bitty-graphics` member required by the Core descriptor contract. URLs: `just component-url x86_64-unknown-linux-gnu`.
- Supported targets: Linux glibc x86_64/aarch64, macOS arm64/x86_64, and Windows x86_64 are declared in [component-targets.toml](component-targets.toml). CI validates the matrix and exercises each target on its native runner; see [docs/component-release-matrix.md](docs/component-release-matrix.md) for current per-target evidence, the documented exclusions (musl, Windows arm64), and the catalog-row contract. The Windows packaging leg is not green yet.
- CI: the `component-targets`, `component`, and `component-aggregate` jobs in [ci.yml](.github/workflows/ci.yml) validate the release matrix, then check, test, clippy, pack, run, and emit the catalog row on each supported native runner, run the live serve-loop suite against the extracted Linux x86_64 executable, and finally verify the complete `SHA256SUMS` plus a descriptor/digest audit of every tarball. The [component release workflow](.github/workflows/component-release.yml) is the only tag publisher: on a strict `v*` tag it verifies the tag against the descriptor, runs `just check` and `just product`, packages the supported matrix (the package job does not run clippy), aggregates the verified matrix, and uploads `bitty/extensions/graphics/<version>/` (tarballs, sidecars, `SHA256SUMS`) with `scripts/publish-component.sh` under the immutable cache policy plus read-back byte verification. The catalog row fragment is generated and verified but not uploaded: `extensions/catalog.v1` is shared across components and is owned by a future merge job.
- Owner step: pushing a `v*` tag is the irreversible publish point. It consumes the organization-level `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` secrets (or repository/org variables of the same names) by name only. No tag, no publish, and no upload happens from an ordinary PR.

### Verification

- `just product` runs `cargo fmt --check`, clippy with `-D warnings`, and `cargo test --workspace --locked`.
- [tests/serve_loop.rs](tests/serve_loop.rs) spawns the real binary and covers the handshake and caps, PNG/RGB/RGBA decode, normalized response shape, typed decode failures that keep the connection open, safe-mode deprivation, zero-length/oversized/unknown/truncated/trailing frames, clean `Shutdown`, clean EOF, and the CLI surface.
- Set `BITTY_GRAPHICS_COMPONENT_BIN` to run that suite against a packed release executable; CI does this for the Linux archive.

### Residual work

- Core still owns bounded intake, placement policy, the aggregate image budget, pre-allocation and pre-upload validation, deadlines, crash recovery, and capability/consent. The Core host client and composition-root binding are tracked by bitty#2163 and are not implemented here.
- Raster requests and pooled decode concurrency are not part of protocol v1; the frozen in-process seam in [src/lib.rs](src/lib.rs) remains the raster contract until a host requirement lands.
