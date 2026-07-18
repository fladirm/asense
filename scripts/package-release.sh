#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for command in cargo find getconf git grep install mktemp rustc sha256sum sort strings touch uname zip; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'asense-release: missing command: %s\n' "$command" >&2
    exit 1
  }
done

worktree_status="$(git status --porcelain=v1 --untracked-files=all)"
[[ -z "$worktree_status" ]] || {
  printf 'asense-release: the worktree must be completely clean (including untracked files)\n' >&2
  printf '%s\n' "$worktree_status" >&2
  exit 1
}

version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)"
[[ -n "$version" ]] || {
  printf 'asense-release: cannot read package version\n' >&2
  exit 1
}
dkms_version="$(sed -n 's/^PACKAGE_VERSION="\([^"]*\)"$/\1/p' kernel/dkms.conf)"
module_version="$(sed -n 's/^MODULE_VERSION("\([^"]*\)");$/\1/p' kernel/asense_rgb.c)"
[[ "$dkms_version" == "$version" && "$module_version" == "$version" ]] || {
  printf 'asense-release: Cargo, DKMS and kernel module versions must match (%s, %s, %s)\n' \
    "$version" "$dkms_version" "$module_version" >&2
  exit 1
}
commit="$(git rev-parse --verify HEAD)"
short_commit="$(git rev-parse --short=12 HEAD)"
source_date_epoch="$(git show -s --format=%ct HEAD)"
[[ "$source_date_epoch" =~ ^[0-9]+$ ]] || {
  printf 'asense-release: cannot read the release commit timestamp\n' >&2
  exit 1
}
[[ "$(uname -m)" == "x86_64" ]] || {
  printf 'asense-release: installer assets must be built on x86_64\n' >&2
  exit 1
}
runtime_libc="$(getconf GNU_LIBC_VERSION)"
[[ "$runtime_libc" == "glibc 2.43" ]] || {
  printf 'asense-release: build on the Ubuntu 26.04/glibc 2.43 release baseline (found %s)\n' \
    "$runtime_libc" >&2
  exit 1
}
release_tag="${ASENSE_RELEASE_TAG:-}"
if [[ -n "$release_tag" ]]; then
  [[ "$release_tag" == "v$version" ]] || {
    printf 'asense-release: tag %s does not match package version v%s\n' \
      "$release_tag" "$version" >&2
    exit 1
  }
  [[ "$(git rev-parse --verify "$release_tag^{commit}")" == "$commit" ]] || {
    printf 'asense-release: tag %s does not point at HEAD\n' "$release_tag" >&2
    exit 1
  }
fi
export LC_ALL=C SOURCE_DATE_EPOCH="$source_date_epoch" TZ=UTC
# Keep local usernames and checkout paths out of panic/source diagnostics and
# make release output independent of the builder's HOME and workspace paths.
rustflag_separator=$'\x1f'
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$ROOT=/usr/src/asense${rustflag_separator}--remap-path-prefix=$HOME=/usr/src/build-home"
unset RUSTFLAGS
installer_name="asense-v${version}-ubuntu-26.04-x86_64-installer-${short_commit}"
source_name="asense-v${version}-source-${short_commit}"
installer_zip="$ROOT/${installer_name}.zip"
source_zip="$ROOT/${source_name}.zip"
installer_checksum="$installer_zip.sha256"
source_checksum="$source_zip.sha256"
checksums="$ROOT/asense-v${version}-${short_commit}-SHA256SUMS.txt"
temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT INT TERM
export CARGO_TARGET_DIR="$temporary/cargo-target"

cargo build --release --locked --bin asensed --no-default-features
cargo build --release --locked --bin asense --features gui
for binary in "$CARGO_TARGET_DIR/release/asense" "$CARGO_TARGET_DIR/release/asensed"; do
  if strings "$binary" | grep -F -e "$ROOT" -e "$HOME" >/dev/null; then
    printf 'asense-release: local build path remained in %s\n' "$binary" >&2
    exit 1
  fi
done

bundle="$temporary/$installer_name"
install -d "$bundle/bin" "$bundle/assets" "$bundle/docs/screenshots" \
  "$bundle/kernel" "$bundle/packaging"
install -m 0755 "$CARGO_TARGET_DIR/release/asense" "$bundle/bin/asense"
install -m 0755 "$CARGO_TARGET_DIR/release/asensed" "$bundle/bin/asensed"
install -m 0755 install.sh uninstall.sh "$bundle/"
install -m 0644 README.md LICENSE Cargo.lock "$bundle/"
install -m 0644 assets/asense.desktop assets/asense.svg "$bundle/assets/"
install -m 0644 docs/screenshots/asense-compact.png \
  docs/screenshots/asense-advanced.png "$bundle/docs/screenshots/"
install -m 0644 docs/RELEASING.md "$bundle/docs/"
install -m 0644 kernel/LICENSE kernel/Makefile kernel/asense_rgb.c kernel/dkms.conf \
  "$bundle/kernel/"
install -m 0644 packaging/90-asense-predator-key.hwdb \
  packaging/asense.service packaging/asense.socket.in packaging/common.sh \
  packaging/asense-system-sleep \
  "$bundle/packaging/"

(
  cd "$bundle"
  printf 'version=%s\n' "$version"
  printf 'commit=%s\n' "$commit"
  printf 'source_date_epoch=%s\n' "$source_date_epoch"
  printf 'target=x86_64-unknown-linux-gnu\n'
  printf 'runtime_libc=%s\n' "$runtime_libc"
  printf 'rustc=%s\n' "$(rustc -V)"
  printf 'cargo=%s\n' "$(cargo -V)"
  printf 'gui_sha256=%s\n' "$(sha256sum bin/asense | cut -d' ' -f1)"
  printf 'daemon_sha256=%s\n' "$(sha256sum bin/asensed | cut -d' ' -f1)"
  printf '\n[payload-sha256]\n'
  while IFS= read -r -d '' payload; do
    sha256sum "${payload#./}"
  done < <(find . -type f ! -name RELEASE-MANIFEST.txt -print0 | sort -z)
) >"$bundle/RELEASE-MANIFEST.txt"
chmod 0644 "$bundle/RELEASE-MANIFEST.txt"

# ZIP stores filesystem mtimes.  Normalize every staged entry to the release
# commit so identical inputs on the same release toolchain produce identical
# installer archives.
find "$bundle" -exec touch -h -d "@$source_date_epoch" -- {} +

rm -f -- "$installer_zip" "$source_zip" "$installer_checksum" \
  "$source_checksum" "$checksums"
(
  cd "$temporary"
  find "$installer_name" -type f -print | sort | zip -X -q "$installer_zip" -@
)
git archive --format=zip --prefix="$source_name/" -o "$source_zip" HEAD
(
  cd "$ROOT"
  sha256sum "$(basename -- "$installer_zip")" >"$installer_checksum"
  sha256sum "$(basename -- "$source_zip")" >"$source_checksum"
  sha256sum "$(basename -- "$installer_zip")" "$(basename -- "$source_zip")" >"$checksums"
)

printf 'Installer:          %s\nInstaller checksum: %s\nSource:             %s\nSource checksum:    %s\nChecksums:          %s\n' \
  "$installer_zip" "$installer_checksum" "$source_zip" "$source_checksum" "$checksums"
