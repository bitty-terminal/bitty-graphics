#!/usr/bin/env bash
# Regression tests for target-specific component archive members.
# This uses a simulated Windows binary so the test does not require a
# cross-compilation toolchain or a runnable Windows executable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/bitty-graphics-component-test.XXXXXX")"
trap 'rm -rf "$TEST_DIR"' EXIT

SOURCE="$TEST_DIR/source"
OUTPUT="$TEST_DIR/dist"
BINARY="$TEST_DIR/bitty-graphics.exe"
DESCRIPTOR="$SOURCE/bitty-component.toml"
WINDOWS_ARCHIVE="$OUTPUT/x86_64-pc-windows-msvc.tar.gz"
MACOS_ARCHIVE="$OUTPUT/aarch64-apple-darwin.tar.gz"

mkdir -p "$SOURCE" "$OUTPUT"
printf 'simulated Windows PE payload\n' >"$BINARY"
DIGEST="$(sha256sum "$BINARY" | cut -d' ' -f1)"
cat >"$DESCRIPTOR" <<EOF
[component]
name = "graphics"
version = "0.0.1"
protocol = [1, 1]
executable = "bitty-graphics"
sha256 = "$DIGEST"
EOF

"$ROOT/scripts/make-component-dist.sh" \
  --source "$SOURCE" \
  --version 0.0.1 \
  --target x86_64-pc-windows-msvc \
  --binary "$BINARY" \
  --output "$WINDOWS_ARCHIVE"

WINDOWS_MEMBERS="$(tar -tzf "$WINDOWS_ARCHIVE" | LC_ALL=C sort)"
WINDOWS_EXPECTED="$(printf '%s\n%s\n' bitty-component.toml bitty-graphics.exe | LC_ALL=C sort)"
test "$WINDOWS_MEMBERS" = "$WINDOWS_EXPECTED"
tar -xOf "$WINDOWS_ARCHIVE" bitty-component.toml >"$TEST_DIR/windows-descriptor.toml"
cmp "$DESCRIPTOR" "$TEST_DIR/windows-descriptor.toml"

"$ROOT/scripts/make-component-dist.sh" \
  --source "$SOURCE" \
  --version 0.0.1 \
  --target aarch64-apple-darwin \
  --binary "$BINARY" \
  --output "$MACOS_ARCHIVE"

MACOS_MEMBERS="$(tar -tzf "$MACOS_ARCHIVE" | LC_ALL=C sort)"
MACOS_EXPECTED="$(printf '%s\n%s\n' bitty-component.toml bitty-graphics | LC_ALL=C sort)"
test "$MACOS_MEMBERS" = "$MACOS_EXPECTED"

echo "test-component-dist: PASS (Windows suffix and non-Windows logical name)"
