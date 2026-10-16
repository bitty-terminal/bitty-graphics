# bitty-graphics

Bounded graphics decode extension crate (landed CTX-0003, independently verified CTX-0004). Png-only decode unit plus the raster mechanics subset; placement policy, rect helpers and renderer upload validation stay in Core. Read [AGENTS](AGENTS.md). Task management lives in CarryCtx.

Prerequisite: W-133 accepted contract specifications/graphics-extraction-contract.md (bitty-terminal-docs specify c28681db closed #170, promotion 3479ce12 closed #184). Bounded protocol intake, placement, resource enforcement and upload validation remain Core responsibilities unless an accepted contract says otherwise.

Local phases CTX-0001 -> CTX-0002 -> CTX-0003 -> CTX-0004 map to Issues #4 -> #3 -> #2 -> #1. CarryCtx reports all four phases complete; GitHub Issues #4, #2 and #1 are closed. This change closes #3 on the accepted W-133 contract.

## Component distribution (pilot, CTX-0008)

Component name `graphics` (owner decision 2026-10-16: strip the `bitty-` prefix, per the `net` precedent). The packed binary is `bitty-graphics`; the install spelling is `bitty extension install graphics --version 0.0.1`.

- Descriptor: [bitty-component.toml](bitty-component.toml) is the canonical record of the fixed fields. The `sha256` there is a placeholder; `scripts/pack-component.sh` renders the per-target digest at pack time and refuses to pack on any other drift.
- Pack locally: `just component-pack x86_64-unknown-linux-gnu` writes `dist/<target>.tar.gz` plus the sidecar and `SHA256SUMS` via the vendored `scripts/make-component-dist.sh` (byte-identical to `bitty@8eb168ad` `scripts/make-component-dist.sh`). URLs: `just component-url x86_64-unknown-linux-gnu`.
- CI: the `component-dist` job in [ci.yml](.github/workflows/ci.yml) packs and verifies the host target on every PR (no upload). The [release workflow](.github/workflows/release.yml) builds five targets and, on a `v*` tag only, uploads `bitty/extensions/graphics/<version>/` (tarballs, sidecars, `SHA256SUMS`) to R2 with the immutable cache policy and read-back hash verification mirrored from bitty's `r2-mirror` job.
- Owner step: pushing the `v0.0.1` tag is the irreversible publish point. It consumes the organization-level `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` secrets by name only. No tag, no publish, no merge happens from the pilot PR itself.
- Pilot binary scope: `bitty-graphics` reports `--version` and refuses anything else (exit 2). The stdio serve loop lands with the Core-owned host binding (graphics#14 follow-up).
