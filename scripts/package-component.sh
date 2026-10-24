#!/usr/bin/env bash
# Package one bitty-graphics target using the DIR-030 component layout.
#
# Adapted from bitty-network `scripts/package-component.sh`: the release
# workflow passes the tag version and the Rust target, this script validates
# the canonical descriptor and delegates the build/pack step to the existing
# `scripts/pack-component.sh` (which renders the per-target digest and packs
# with the vendored `scripts/make-component-dist.sh`).
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
readonly REPO_ROOT
readonly DESCRIPTOR_FILE="bitty-component.toml"
readonly COMPONENT_NAME="graphics"
readonly EXECUTABLE="bitty-graphics"
readonly ZERO_DIGEST="0000000000000000000000000000000000000000000000000000000000000000"
readonly MAX_DESCRIPTOR_BYTES=4096

VERSION=""
TARGET=""
OUTPUT=""
PRINT_URL=0
CDN_BASE=""

die() {
	printf 'package-component: ERROR: %s\n' "$1" >&2
	exit 1
}

usage() {
	cat <<'EOF'
Usage: scripts/package-component.sh --target TRIPLE --output PATH [--version X.Y.Z]
       scripts/package-component.sh --print-url --target TRIPLE [--version X.Y.Z] [--cdn-base URL]

  --target TRIPLE  Rust target triple; output basename must be TRIPLE.tar.gz
  --output PATH    output archive path (pack-component builds the binary)
  --version X.Y.Z  descriptor version (defaults to bitty-component.toml)
  --print-url      print the archive URL and SHA256SUMS URL without writing
  --cdn-base URL   CDN base for --print-url (default: make-component-dist.sh)
EOF
}

valid_version() {
	[[ "$1" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
}

valid_target() {
	[[ "$1" =~ ^[a-z0-9_]+(-[a-z0-9_]+)+$ ]]
}

descriptor_value() {
	awk -F'"' -v key="$1" '
		$1 ~ "^[[:space:]]*" key "[[:space:]]*=" { print $2; exit }
	' "$REPO_ROOT/$DESCRIPTOR_FILE"
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--target)
		TARGET="${2:-}"
		shift 2
		;;
	--output)
		OUTPUT="${2:-}"
		shift 2
		;;
	--version)
		VERSION="${2:-}"
		shift 2
		;;
	--print-url)
		PRINT_URL=1
		shift
		;;
	--cdn-base)
		CDN_BASE="${2:-}"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		usage >&2
		die "unknown argument: $1"
		;;
	esac
done

descriptor="$REPO_ROOT/$DESCRIPTOR_FILE"
[[ -f "$descriptor" && ! -L "$descriptor" ]] || die "root descriptor is missing"
descriptor_size="$(wc -c <"$descriptor" | tr -d ' ')"
[[ "$descriptor_size" -le "$MAX_DESCRIPTOR_BYTES" ]] || die "root descriptor exceeds $MAX_DESCRIPTOR_BYTES bytes"

cargo_version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$REPO_ROOT/Cargo.toml" | head -n 1)"
valid_version "$cargo_version" || die "Cargo.toml version must be strict X.Y.Z: $cargo_version"
descriptor_version="$(descriptor_value version)"
valid_version "$descriptor_version" || die "descriptor version must be strict X.Y.Z: $descriptor_version"
[[ -z "$VERSION" ]] || valid_version "$VERSION" || die "version must be strict X.Y.Z: $VERSION"
[[ -z "$VERSION" ]] || [[ "$VERSION" == "$cargo_version" ]] || die "version $VERSION differs from Cargo.toml $cargo_version"
[[ -z "$VERSION" ]] || [[ "$VERSION" == "$descriptor_version" ]] || die "version $VERSION differs from descriptor $descriptor_version"
if [[ "${GITHUB_REF_TYPE:-}" == "tag" && -n "${GITHUB_REF_NAME:-}" ]]; then
	[[ "$GITHUB_REF_NAME" == "v$cargo_version" ]] || die "tag $GITHUB_REF_NAME does not match version $cargo_version"
fi

grep -Fx '[component]' "$descriptor" >/dev/null || die "root descriptor must contain [component]"
grep -Fx "name = \"$COMPONENT_NAME\"" "$descriptor" >/dev/null || die "root descriptor name must be $COMPONENT_NAME"
grep -Fx 'protocol = [1, 1]' "$descriptor" >/dev/null || die "root descriptor protocol must be [1, 1]"
grep -Fx "executable = \"$EXECUTABLE\"" "$descriptor" >/dev/null || die "root descriptor executable must be $EXECUTABLE"
grep -Fx "sha256 = \"$ZERO_DIGEST\"" "$descriptor" >/dev/null || die "root descriptor must use the zero digest sentinel"

valid_target "$TARGET" || die "target must look like a Rust target triple: $TARGET"

if [[ "$PRINT_URL" -eq 1 ]]; then
	print_args=(--print-url --name "$COMPONENT_NAME" --version "$cargo_version" --target "$TARGET")
	if [[ -n "$CDN_BASE" ]]; then
		print_args+=(--cdn-base "$CDN_BASE")
	fi
	exec "$SCRIPT_DIR/make-component-dist.sh" "${print_args[@]}"
fi

[[ -n "$OUTPUT" ]] || die "--output is required"
[[ "$(basename -- "$OUTPUT")" == "$TARGET.tar.gz" ]] || die "output basename must be $TARGET.tar.gz"
if [[ "$OUTPUT" != /* ]]; then
	OUTPUT="$PWD/${OUTPUT#./}"
fi
output_dir="$(dirname -- "$OUTPUT")"
mkdir -p -- "$output_dir"

"$SCRIPT_DIR/pack-component.sh" "$TARGET" "$output_dir"

archive="$output_dir/$TARGET.tar.gz"
[[ -f "$archive" && ! -L "$archive" ]] || die "expected archive is missing: $archive"
[[ -f "$archive.sha256" && ! -L "$archive.sha256" ]] || die "expected sidecar is missing: $archive.sha256"
[[ -f "$output_dir/SHA256SUMS" && ! -L "$output_dir/SHA256SUMS" ]] || die "expected aggregate manifest is missing: $output_dir/SHA256SUMS"

archive_executable="$EXECUTABLE"
if [[ "$TARGET" == *-windows-* ]]; then
	archive_executable="$EXECUTABLE.exe"
fi
expected_members="$(printf '%s\n%s\n' "$DESCRIPTOR_FILE" "$archive_executable" | LC_ALL=C sort)"
actual_members="$(tar -tzf "$archive" | LC_ALL=C sort)"
[[ "$actual_members" == "$expected_members" ]] || die "archive members are not exactly descriptor plus executable"

if command -v sha256sum >/dev/null 2>&1; then
	sha256_tool=(sha256sum)
else
	sha256_tool=(shasum -a 256)
fi
# GNU sha256sum emits "<hex>  <name>" in text mode but "<hex> *<name>" in
# binary mode, and Git-Bash/MSYS defaults to binary mode. Read the digest and
# name ourselves, stripping the optional binary marker, instead of relying on
# `sha256sum -c`: check mode honors that marker, and MSYS text mode would
# translate CRLF bytes inside the tarball and corrupt the comparison.
while read -r digest name; do
	[[ -n "$digest" && -n "$name" ]] || die "SHA256SUMS contains an empty entry"
	[[ "$digest" =~ ^[0-9a-f]{64}$ ]] || die "SHA256SUMS contains a malformed digest: $digest"
	name="${name#\*}"
	if [[ "$name" == */* ]]; then
		die "SHA256SUMS entry is not a plain filename: $name"
	fi
	actual="$("${sha256_tool[@]}" "$output_dir/$name" | cut -d' ' -f1)"
	[[ "$actual" == "$digest" ]] || die "SHA256SUMS verification failed for $name"
done <"$output_dir/SHA256SUMS"

printf 'package-component: PASS (name=%s, target=%s, version=%s, output=%s)\n' \
	"$COMPONENT_NAME" "$TARGET" "$cargo_version" "$archive"
