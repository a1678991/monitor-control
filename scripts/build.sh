#!/usr/bin/env bash
# Build and package both applications for the native Linux host.
set -euo pipefail

if [[ $# -ne 0 ]]; then
    echo "Usage: $0 (builds a release archive and checksum in dist/)" >&2
    exit 2
fi

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
target=$(rustc -vV | sed -n 's/^host: //p')
case "$target" in
    *-unknown-linux-gnu) ;;
    *) echo "Unsupported host: $target; a native GNU/Linux toolchain is required." >&2; exit 1 ;;
esac

version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c \
    'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "monitorctl"))')
if [[ ${GITHUB_REF_TYPE:-} == tag && ${GITHUB_REF_NAME:-} != "v$version" ]]; then
    echo "Release tag ${GITHUB_REF_NAME:-} must match workspace version v$version." >&2
    exit 1
fi

# Explicit target and output directory keep packaging independent of Cargo environment defaults.
cargo build --workspace --release --locked --target "$target" --target-dir target
name="monitor-control-${version}-${target}"
mkdir -p dist
staging=$(mktemp -d "$PWD/dist/.build.XXXXXXXX")
trap 'rm -rf -- "$staging"' EXIT
mkdir -p "$staging/$name/packaging"
for binary in monitorctl monitorctl-gui; do
    install -m755 "target/$target/release/$binary" "$staging/$name/$binary"
done
install -m644 README.md "$staging/$name/"
install -m644 packaging/* "$staging/$name/packaging/"
tar -C "$staging" -czf "$staging/$name.tar.gz" "$name"
mv -- "$staging/$name.tar.gz" "dist/$name.tar.gz"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
printf 'Built dist/%s.tar.gz and its SHA-256 checksum\n' "$name"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    printf 'artifact_name=%s\ntarget=%s\n' "$name" "$target" >> "$GITHUB_OUTPUT"
fi
