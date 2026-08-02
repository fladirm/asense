#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
render_only=0
if [[ "${1:-}" == "--render-only" ]]; then
  render_only=1
  shift
fi

if (($# != 3)); then
  printf 'Usage: %s [--render-only] OUTPUT_DIR SOURCE_URL SOURCE_SHA256\n' "$0" >&2
  exit 2
fi

output="$1"
source_url="$2"
source_sha256="${3,,}"
version="$("$ROOT/scripts/version.sh" show)"

[[ "$source_url" == https://* || \
  ("${ASENSE_AUR_ALLOW_LOCAL_SOURCE:-0}" == 1 && "$source_url" == file://*) ]] || {
  printf 'asense-aur: source URL must be HTTPS (or an explicitly allowed local test URL)\n' >&2
  exit 1
}
[[ "$source_sha256" =~ ^[0-9a-f]{64}$ ]] || {
  printf 'asense-aur: source SHA-256 must be exactly 64 lowercase hex digits\n' >&2
  exit 1
}

install -d -m 0755 "$output"
for target in PKGBUILD .SRCINFO asense.install; do
  [[ ! -e "$output/$target" ]] || {
    printf 'asense-aur: refusing to overwrite %s\n' "$output/$target" >&2
    exit 1
  }
done

content="$(<"$ROOT/packaging/arch/PKGBUILD.in")"
content="${content//@PKGVER@/$version}"
content="${content//@SOURCE_URL@/$source_url}"
content="${content//@SOURCE_SHA256@/$source_sha256}"
[[ "$content" != *'@PKGVER@'* && "$content" != *'@SOURCE_URL@'* && \
  "$content" != *'@SOURCE_SHA256@'* ]] || {
  printf 'asense-aur: unresolved template placeholder\n' >&2
  exit 1
}
printf '%s\n' "$content" >"$output/PKGBUILD"
install -m 0644 "$ROOT/packaging/arch/asense.install" \
  "$output/asense.install"

if ((render_only)); then
  printf 'Rendered %s/PKGBUILD (SRCINFO intentionally deferred).\n' "$output"
  exit 0
fi

command -v makepkg >/dev/null 2>&1 || {
  printf 'asense-aur: makepkg is required to generate .SRCINFO\n' >&2
  exit 1
}
(
  cd "$output"
  makepkg --printsrcinfo >.SRCINFO.tmp
  mv .SRCINFO.tmp .SRCINFO
)
tab=$'\t'
grep --fixed-strings --line-regexp "${tab}pkgver = $version" \
  "$output/.SRCINFO" >/dev/null
grep --fixed-strings --line-regexp "${tab}sha256sums = $source_sha256" \
  "$output/.SRCINFO" >/dev/null
printf 'Rendered %s/PKGBUILD and verified .SRCINFO for ASense %s.\n' \
  "$output" "$version"
