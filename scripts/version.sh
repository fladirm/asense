#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

read_cargo_version() {
  local version

  version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$ ]] || {
    printf 'asense-version: invalid Cargo package version: %s\n' "$version" >&2
    exit 1
  }
  printf '%s\n' "$version"
}

debian_upstream_version() {
  local version="$1"
  printf '%s\n' "${version/-/~}"
}

require_line() {
  local expected="$1"
  local file="$2"

  grep --fixed-strings --line-regexp -- "$expected" "$ROOT/$file" >/dev/null || {
    printf 'asense-version: %s does not contain the required authority: %s\n' \
      "$file" "$expected" >&2
    exit 1
  }
}

check_authorities() {
  local changelog_version
  local debian_upstream
  local dkms_version
  local lock_version
  local module_version
  local version

  command -v dpkg-parsechangelog >/dev/null 2>&1 || {
    printf 'asense-version: dpkg-parsechangelog is required for authority checks\n' >&2
    exit 1
  }

  version="$(read_cargo_version)"
  debian_upstream="$(debian_upstream_version "$version")"
  lock_version="$(sed -n '/^name = "asense"$/{n;s/^version = "\([^"]*\)"$/\1/p;q}' \
    "$ROOT/Cargo.lock")"
  dkms_version="$(sed -n 's/^PACKAGE_VERSION="\([^"]*\)"$/\1/p' \
    "$ROOT/kernel/dkms.conf")"
  module_version="$(sed -n 's/^MODULE_VERSION("\([^"]*\)");$/\1/p' \
    "$ROOT/kernel/asense_rgb.c")"
  changelog_version="$(cd "$ROOT" && dpkg-parsechangelog -SVersion)"

  [[ "$lock_version" == "$version" && "$dkms_version" == "$version" && \
    "$module_version" == "$version" ]] || {
    printf 'asense-version: Cargo/lock/DKMS/module differ: %s %s %s %s\n' \
      "$version" "$lock_version" "$dkms_version" "$module_version" >&2
    exit 1
  }
  [[ "$changelog_version" == "$debian_upstream-"* ]] || {
    printf 'asense-version: Debian version %s is not derived from %s\n' \
      "$changelog_version" "$version" >&2
    exit 1
  }

  for source in kernel/asense_rgb.c kernel/Makefile kernel/LICENSE; do
    require_line "$source usr/src/asense-rgb-$version" debian/asense.install
  done
  require_line "docs/RELEASE_NOTES_v$version.md" debian/asense.docs
  [[ -f "$ROOT/docs/RELEASE_NOTES_v$version.md" ]] || {
    printf 'asense-version: release notes are absent for %s\n' "$version" >&2
    exit 1
  }
  grep --fixed-strings "ASense $version" "$ROOT/debian/asense.1" >/dev/null
  grep --fixed-strings "ASense $version" \
    "$ROOT/debian/asense-configure-user.8" >/dev/null
  grep --fixed-strings "<release version=\"$version\"" \
    "$ROOT/debian/io.github.fladirm.asense.metainfo.xml" >/dev/null
  # shellcheck disable=SC2016 # the literal command substitution is the contract
  grep --fixed-strings 'version="$(scripts/version.sh show)"' \
    "$ROOT/scripts/package-release.sh" >/dev/null
  # shellcheck disable=SC2016 # the literal command substitution is the contract
  grep --fixed-strings 'version="$(scripts/version.sh show)"' \
    "$ROOT/.github/workflows/release.yml" >/dev/null
  grep --fixed-strings 'scripts/version.sh check' \
    "$ROOT/scripts/verify-release.sh" >/dev/null

  for placeholder in @PKGVER@ @SOURCE_URL@ @SOURCE_SHA256@; do
    grep --fixed-strings "$placeholder" \
      "$ROOT/packaging/arch/PKGBUILD.in" >/dev/null || {
      printf 'asense-version: Arch template lost placeholder %s\n' \
        "$placeholder" >&2
      exit 1
    }
  done

  printf 'ASense version authorities: PASS version=%s debian=%s tag=v%s\n' \
    "$version" "$changelog_version" "$version"
}

case "${1:-show}" in
  show)
    read_cargo_version
    ;;
  tag)
    printf 'v%s\n' "$(read_cargo_version)"
    ;;
  debian-upstream)
    debian_upstream_version "$(read_cargo_version)"
    ;;
  release-notes)
    printf 'docs/RELEASE_NOTES_v%s.md\n' "$(read_cargo_version)"
    ;;
  check)
    check_authorities
    ;;
  *)
    printf 'Usage: %s show|tag|debian-upstream|release-notes|check\n' "$0" >&2
    exit 2
    ;;
esac
