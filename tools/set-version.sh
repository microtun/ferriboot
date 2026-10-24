#!/bin/sh
# Set one release version across every Cargo workspace in the repository and
# regenerate each workspace's Cargo.lock.
set -eu

usage() {
    printf 'Usage: %s [--keep-deps] VERSION\n' "$0"
    printf '\n'
    printf 'Set VERSION (e.g. 0.2.0 or v0.2.0-rc.1) in every [workspace.package] and\n'
    printf '[package] table with a literal version, then regenerate the Cargo.lock of\n'
    printf 'every workspace. Workspaces are found automatically (any Cargo.toml with\n'
    printf 'a [workspace] table, outside target/).\n'
    printf '\n'
    printf '  --keep-deps  Only refresh the local packages in each lock file\n'
    printf '               (cargo update --workspace) instead of re-resolving every\n'
    printf '               dependency to its latest compatible version.\n'
    printf '\n'
    printf 'Uses `cargo` from PATH; set RUSTUP_TOOLCHAIN=1.95.0 to match CI.\n'
}

keep_deps=0
version=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --keep-deps) keep_deps=1 ;;
        -*) usage >&2; exit 1 ;;
        *)
            if [ -n "$version" ]; then
                usage >&2
                exit 1
            fi
            version=$1
            ;;
    esac
    shift
done
if [ -z "$version" ]; then
    usage >&2
    exit 1
fi

# Accept the release tag form (v1.2.3) as well as the bare version.
version=${version#v}
# Same pattern as the tag check in .github/workflows/release.yml.
if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    printf 'Error: "%s" is not a semver version (MAJOR.MINOR.PATCH[-PRERELEASE]).\n' "$version" >&2
    exit 1
fi
# Fail before touching any manifest if cargo (or the selected toolchain)
# is not usable.
if ! cargo --version >/dev/null 2>&1; then
    printf 'Error: cargo is required (and RUSTUP_TOOLCHAIN, if set, must be installed).\n' >&2
    exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
if [ ! -f "$root/Cargo.toml" ]; then
    printf 'Error: %s is not the repository root (no Cargo.toml).\n' "$root" >&2
    exit 1
fi

tmp=$(mktemp)
trap 'rm -f -- "$tmp"' EXIT HUP INT TERM

# Every manifest outside build output, in a stable order. Split the list on
# newlines only, so the checkout path may contain spaces.
IFS='
'
set -f
manifests=$(find "$root" \( -name target -o -name .git \) -prune -o -name Cargo.toml -type f -print | LC_ALL=C sort)

# Rewrite `version = "..."` in [package] and [workspace.package] tables.
# `version.workspace = true` lines and dependency versions are left alone.
set_version_awk='
/^[ \t]*\[/ {
    section = $0
    sub(/#.*/, "", section)
    gsub(/[ \t]/, "", section)
}
(section == "[package]" || section == "[workspace.package]") && /^[ \t]*version[ \t]*=[ \t]*"/ {
    sub(/"[^"]*"/, "\"" ver "\"")
}
{ print }
'

printf 'Setting version %s\n' "$version"
changed=0
for manifest in $manifests; do
    awk -v ver="$version" "$set_version_awk" "$manifest" > "$tmp"
    if ! cmp -s "$tmp" "$manifest"; then
        # Write in place so the file keeps its permissions.
        cat "$tmp" > "$manifest"
        printf '  updated %s\n' "${manifest#"$root"/}"
        changed=$((changed + 1))
    fi
done
if [ "$changed" -eq 0 ]; then
    printf '  all manifests already at %s\n' "$version"
fi

# Check nothing was missed (e.g. an unusual formatting the edit did not match).
stale=$(
    for manifest in $manifests; do
        awk -v ver="$version" -v file="${manifest#"$root"/}" '
            /^[ \t]*\[/ { section = $0; sub(/#.*/, "", section); gsub(/[ \t]/, "", section) }
            (section == "[package]" || section == "[workspace.package]") && /^[ \t]*version[ \t]*=/ \
                && index($0, "\"" ver "\"") == 0 { print "  " file ": " $0 }
        ' "$manifest"
    done
)
if [ -n "$stale" ]; then
    printf 'Error: these versions were not updated:\n%s\n' "$stale" >&2
    exit 1
fi

# Each workspace has its own lock file. Run cargo from the workspace
# directory so its .cargo/config.toml applies, as in CI.
printf 'Regenerating lock files\n'
for manifest in $manifests; do
    grep -Eq '^[ \t]*\[workspace\]' "$manifest" || continue
    dir=$(dirname -- "$manifest")
    rel=${dir#"$root"}
    rel=${rel#/}
    printf '  %sCargo.lock\n' "${rel:+$rel/}"
    if [ "$keep_deps" -eq 1 ]; then
        (cd -- "$dir" && cargo update --workspace --quiet)
    else
        (cd -- "$dir" && cargo generate-lockfile --quiet)
    fi
done

printf 'Done. Review with `git diff`, then commit and tag v%s.\n' "$version"
