# bitty-graphics

Bounded graphics decode extension crate (landed CTX-0003, independently verified CTX-0004). Png-only decode unit plus the raster mechanics subset; placement policy, rect helpers and renderer upload validation stay in Core. Read [AGENTS](AGENTS.md). Task management lives in CarryCtx.

Prerequisite: W-133 accepted contract specifications/graphics-extraction-contract.md (bitty-terminal-docs specify c28681db closed #170, promotion 3479ce12 closed #184). Bounded protocol intake, placement, resource enforcement and upload validation remain Core responsibilities unless an accepted contract says otherwise.

Local phases CTX-0001 -> CTX-0002 -> CTX-0003 -> CTX-0004 map to Issues #4 -> #3 -> #2 -> #1. CarryCtx reports all four phases complete; GitHub Issues #4, #2 and #1 are closed. This change closes #3 on the accepted W-133 contract.

## Component distribution (0.0.24)

Component name `graphics` (owner decision 2026-10-16: strip the `bitty-` prefix, per the `net` precedent). The packed binary is `bitty-graphics`; the install spelling is `bitty extension install graphics --version 0.0.24`.

- Descriptor: [bitty-component.toml](bitty-component.toml) is the canonical record of the fixed fields. The `sha256` there is a placeholder; `scripts/pack-component.sh` renders the per-target digest at pack time and refuses to pack on any other drift.
- Pack locally: `just component-pack x86_64-unknown-linux-gnu` writes `dist/<target>.tar.gz` plus the sidecar and `SHA256SUMS` via the vendored `scripts/make-component-dist.sh`. Windows archives contain `bitty-graphics.exe`; Linux and macOS archives contain the extensionless `bitty-graphics` member required by the same Core descriptor contract. URLs: `just component-url x86_64-unknown-linux-gnu`.
- CI: the `component-dist` job in [ci.yml](.github/workflows/ci.yml) exercises the release packer on every PR (no upload). The [component release workflow](.github/workflows/component-release.yml) is the only tag publisher: on a strict `v*` tag it verifies the tag against the descriptor, runs `just check` and `just product`, builds `x86_64-unknown-linux-gnu`, packages with `scripts/package-component.sh`, and uploads `bitty/extensions/graphics/<version>/` (tarball, sidecar, `SHA256SUMS`) with `scripts/publish-component.sh` under the immutable cache policy plus read-back byte verification.
- Owner step: pushing the `v0.0.24` tag is the irreversible publish point. It consumes the organization-level `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` secrets (or repository/org variables of the same names) by name only. No tag, no publish, no merge happens from an ordinary PR.
- Pilot binary scope: `bitty-graphics` reports `--version` and refuses anything else (exit 2). The stdio serve loop lands with the Core-owned host binding (graphics#14 follow-up); publishing 0.0.24 exercises the distribution contract only and does not claim a served worker (graphics#17).
