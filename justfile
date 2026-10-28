# Metadata-only gates. These are not Rust/Lua product evidence.
prettier_version := "3.9.6"
markdownlint_version := "0.23.1"
actionlint_version := "1.7.12"

fmt-check:
    bunx --bun prettier@{{prettier_version}} --check . --ignore-unknown

markdownlint:
    bunx --bun markdownlint-cli2@{{markdownlint_version}}

metadata:
    test -s README.md && test -s AGENTS.md && test -s repo.toml
    test -s .carryctx/config.toml
    python3 -c 'import tomllib; from pathlib import Path; [tomllib.loads(p.read_text()) for p in [Path("repo.toml"), Path(".carryctx/config.toml")]]'

hygiene:
    #!/usr/bin/env bash
    set -euo pipefail
    bad=0
    while IFS= read -r -d '' f; do
        case "$f" in
            *.sqlite|*.db|*.zst|*.tar|*.tar.gz|*.tgz|node_modules/*|target/*|dist/*|.worktrees/*)
                echo "unexpected artifact tracked: $f" >&2; bad=1 ;;
        esac
    done < <(git ls-files -z --cached --others --exclude-standard)
    test "$bad" -eq 0

paths:
    #!/usr/bin/env bash
    set -euo pipefail
    pattern='(/hom''e/|/Use''rs/|/mn''t/[A-Za-z]|[A-Za-z]:[\\/]Use''rs[\\/])'
    found=0
    while IFS= read -r -d '' f; do
        if grep -nEI "$pattern" "$f"; then found=1; fi
    done < <(git ls-files -z --cached --others --exclude-standard)
    if [ "$found" -ne 0 ]; then
        echo 'hardcoded host path detected (portable-path gate)' >&2
        exit 1
    fi
    echo 'portable-path gate passed'

actionlint:
    @installed="$(actionlint --version | head -n 1)"; test "$installed" = "{{actionlint_version}}" || { echo "actionlint {{actionlint_version}} required; found $installed" >&2; exit 1; }
    actionlint -color -shellcheck=

check:
    just fmt-check
    just markdownlint
    just metadata
    just hygiene
    just paths
    just component-test
    just component-release-check
    just component-release-test
    just component-smoke-test

workflows:
    actionlint
    act -n

# Publish a redacted CarryCtx snapshot to refs/heads/carryctx-snapshots.
workflow-publish:
    ./scripts/workflow-publish.sh

# Fetch and import the published CarryCtx snapshot (fresh-clone recovery).
workflow-import:
    git fetch origin refs/heads/carryctx-snapshots:refs/remotes/origin/carryctx-snapshots
    carryctx import --from-git refs/remotes/origin/carryctx-snapshots

# Product gates: full Rust verification for the landed crate.
product:
    cargo fmt --all -- --check
    RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets
    cargo test --workspace --locked

# Build the component binary for one target and pack the R2 tarball form
# (CTX-0008 pilot): dist/<target>.tar.gz plus sidecar and SHA256SUMS.
component-pack target out="dist":
    ./scripts/pack-component.sh {{target}} {{out}}

# Exercise target-specific archive member naming without cross-compiling.
component-test:
    ./scripts/test-component-dist.sh

# Gate: descriptor, supported release matrix, and declared target support.
component-release-check:
    ./scripts/component-release.sh check

# Fixture tests for the release matrix, manifest, catalog.v1 rows, and the
# generated per-target digest record.
component-release-test:
    ./scripts/tests/component-release.test.sh

# Fixture tests for the packaged-binary protocol smoke (no Rust build).
component-smoke-test:
    ./scripts/tests/component-smoke.test.sh

# Protocol smoke against one packaged/built executable: Hello/DecodeRequest ->
# HelloAck/DecodeResponse and a clean Shutdown.
component-smoke binary:
    ./scripts/component-release.sh smoke --binary "{{binary}}"

# Write the per-target release digest record from a complete verified dist.
component-release-record dist out="release/component-digests.toml":
    ./scripts/component-release.sh record --dist "{{dist}}" --output "{{out}}"

# Print the CDN URLs for this component version (pure mapping, no network).
component-url target:
    ./scripts/make-component-dist.sh --print-url --name graphics --version "$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)" --target {{target}}
