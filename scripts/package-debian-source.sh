#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for command in cargo git mktemp sha256sum tar xz; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'asense-debian-source: missing command: %s\n' "$command" >&2
    exit 1
  }
done

scripts/version.sh check
status="$(git status --porcelain=v1 --untracked-files=all)"
[[ -z "$status" ]] || {
  printf 'asense-debian-source: worktree must be completely clean\n%s\n' \
    "$status" >&2
  exit 1
}

version="$(scripts/version.sh show)"
debian_upstream="$(scripts/version.sh debian-upstream)"
commit="$(git rev-parse --verify HEAD)"
source_date_epoch="$(git show -s --format=%ct HEAD)"
output="${1:-$(dirname -- "$ROOT")}"
mkdir -p "$output"
output="$(cd -- "$output" && pwd)"

upstream="$output/asense_${debian_upstream}.orig.tar.xz"
vendor_archive="$output/asense_${debian_upstream}.orig-vendor.tar.xz"
checksums="$output/asense_${debian_upstream}.orig-SHA256SUMS.txt"
for artifact in "$upstream" "$vendor_archive" "$checksums"; do
  [[ ! -e "$artifact" ]] || {
    printf 'asense-debian-source: refusing to overwrite %s\n' "$artifact" >&2
    exit 1
  }
done
temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT INT TERM

git archive --format=tar --prefix="asense-$version/" HEAD \
  >"$temporary/upstream.tar"
xz -9e --threads=1 --stdout "$temporary/upstream.tar" >"$temporary/upstream.tar.xz"

cargo vendor --locked --versioned-dirs "$temporary/vendor" \
  >"$temporary/cargo-vendor-config.txt"
tar --sort=name --mtime="@$source_date_epoch" \
  --owner=0 --group=0 --numeric-owner \
  --pax-option=delete=atime,delete=ctime \
  -C "$temporary/vendor" -cf "$temporary/vendor.tar" .
xz -9e --threads=1 --stdout "$temporary/vendor.tar" >"$temporary/vendor.tar.xz"

install -m 0644 "$temporary/upstream.tar.xz" "$upstream"
install -m 0644 "$temporary/vendor.tar.xz" "$vendor_archive"
(
  cd "$output"
  sha256sum "$(basename -- "$upstream")" "$(basename -- "$vendor_archive")" \
    >"$(basename -- "$checksums")"
)

printf 'Debian source authorities created from commit %s:\n  %s\n  %s\n  %s\n' \
  "$commit" "$upstream" "$vendor_archive" "$checksums"
