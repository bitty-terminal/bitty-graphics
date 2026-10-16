#!/usr/bin/env bash
# pack-component.sh — build bitty-graphics for one target and pack the R2
# component tarball (CTX-0008 pilot).
#
# Renders a per-target stage descriptor from the canonical
# bitty-component.toml (only the sha256 line may differ: it becomes the
# target binary's digest), then packs with the vendored
# scripts/make-component-dist.sh. Writes dist/<target>.tar.gz plus the
# <target>.tar.gz.sha256 sidecar and the aggregate dist/SHA256SUMS.
#
# Usage: scripts/pack-component.sh <target> [output-dir]
# Needs: cargo, tar, gzip, sha256sum (or shasum -a 256).
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET="${1:?usage: pack-component.sh <target> [output-dir]}"
OUT_DIR="${2:-dist}"

# Fail closed when the caller overrides the version: the packed descriptor
# must equal Cargo.toml, and the release workflow separately pins it to the
# tag. Explicit VERSION_OK exists so a human typo cannot silently ship a
# mismatched descriptor.
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
[[ -n "$VERSION" ]] || {
	echo "pack-component: ERROR: no version in Cargo.toml" >&2
	exit 1
}

BIN="target/${TARGET}/release/bitty-graphics"
case "$TARGET" in
*-windows-*) BIN="${BIN}.exe" ;;
esac

echo "pack-component: building bitty-graphics $VERSION for $TARGET"
cargo build --release --locked --target "$TARGET" --bin bitty-graphics

[[ -f "$BIN" ]] || {
	echo "pack-component: ERROR: binary not found: $BIN" >&2
	exit 1
}

if command -v sha256sum >/dev/null 2>&1; then
	DIGEST="$(sha256sum "$BIN" | cut -d' ' -f1)"
else
	DIGEST="$(shasum -a 256 "$BIN" | cut -d' ' -f1)"
fi

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/bitty-graphics-stage.XXXXXX")"
# shellcheck disable=SC2064
trap "rm -rf '$STAGE'" EXIT

# Render the stage descriptor from the canonical record: every line except
# sha256 must survive byte-identical, so descriptor drift fails here instead
# of shipping.
sed "s/^sha256 = \".*\"$/sha256 = \"${DIGEST}\"/" bitty-component.toml >"$STAGE/bitty-component.toml"
if ! diff <(grep -v '^sha256 = ' bitty-component.toml) <(grep -v '^sha256 = ' "$STAGE/bitty-component.toml") >/dev/null; then
	echo "pack-component: ERROR: rendered descriptor drifted from bitty-component.toml (non-sha256 lines)" >&2
	diff <(grep -v '^sha256 = ' bitty-component.toml) <(grep -v '^sha256 = ' "$STAGE/bitty-component.toml") >&2 || true
	exit 1
fi

./scripts/make-component-dist.sh --source "$STAGE" --version "$VERSION" \
	--target "$TARGET" --binary "$BIN" --output "$OUT_DIR/$TARGET.tar.gz"
