#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for command in base64 bash cargo cmp desktop-file-validate dpkg-parsechangelog find grep install make mktemp rustc rustfmt sed sh sort systemd-analyze systemd-hwdb tr udevadm; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'asense-verify: missing command: %s\n' "$command" >&2
    exit 1
  }
done

[[ ! -e rust-toolchain.toml ]] || {
  printf 'asense-verify: repository toolchain pins are forbidden; use the installed system Rust\n' >&2
  exit 1
}
if grep -F rustup .github/workflows/ci.yml .github/workflows/release.yml \
  scripts/package-release.sh >/dev/null; then
  printf 'asense-verify: build/release paths must not invoke rustup\n' >&2
  exit 1
fi
asense_cargo_path="$(command -v cargo)"
asense_rustc_path="$(command -v rustc)"
printf 'asense-verify: system Rust authority\n'
printf '  cargo_path=%s\n' "$asense_cargo_path"
printf '  cargo_version=%s\n' "$(cargo -V)"
printf '  rustc_path=%s\n' "$asense_rustc_path"
printf '  rustc_version=%s\n' "$(rustc -Vv | tr '\n' ';')"
printf '  rustfmt_version=%s\n' "$(rustfmt -V)"
cargo clippy -V >/dev/null || {
  printf 'asense-verify: the installed system Cargo lacks Clippy\n' >&2
  exit 1
}

temporary="$(mktemp -d)"
cleanup() {
  find "$temporary" -depth -type f -exec unlink -- {} \; 2>/dev/null || true
  find "$temporary" -depth -type l -exec unlink -- {} \; 2>/dev/null || true
  find "$temporary" -depth -type d -exec rmdir -- {} \; 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

run cargo fmt --all -- --check

printf '\n==> release version authorities\n'
scripts/version.sh check
version="$(scripts/version.sh show)"
grep --fixed-strings --line-regexp \
  "rustflag_separator := \$(shell printf '\\037')" debian/rules >/dev/null
grep --fixed-strings --line-regexp \
  'export CARGO_ENCODED_RUSTFLAGS := --remap-path-prefix=$(CURDIR)=/usr/src/asense$(rustflag_separator)--remap-path-prefix=$(HOME)=/usr/src/build-home' \
  debian/rules >/dev/null
grep --fixed-strings --line-regexp \
  'export ASENSE_BUILD_COMMIT := $(asense_build_commit)' debian/rules >/dev/null
grep --fixed-strings --line-regexp \
  'stamp_root="$temporary/stamp/asense-$version"' \
  scripts/package-debian-source.sh >/dev/null
grep --fixed-strings --line-regexp \
  '  "asense-$version/.asense-build-commit"' \
  scripts/package-debian-source.sh >/dev/null

printf '\n==> Arch source-package template\n'
aur_test="$temporary/aur"
scripts/render-aur.sh --render-only "$aur_test" \
  "https://github.com/fladirm/asense/archive/refs/tags/v$version.tar.gz" \
  "1111111111111111111111111111111111111111111111111111111111111111"
grep --fixed-strings --line-regexp "pkgver=$version" "$aur_test/PKGBUILD"
if grep -F -e /home/ -e /mnt/ -e wraith "$aur_test/PKGBUILD" \
  "$aur_test/asense.install" >/dev/null; then
  printf 'asense-verify: Arch package contains a build-user/path assumption\n' >&2
  exit 1
fi

run cargo test --locked --all-targets --all-features
run cargo test --locked --test kernel_rgb_protocol
run cargo clippy --locked --all-targets --all-features -- -D warnings

# Keep the privileged helper independently buildable without GTK/WebKit or any
# other default feature pulled in by the desktop application.
run cargo clippy --locked --bin asensed --no-default-features -- -D warnings
run cargo build --release --locked --bin asensed --no-default-features
run desktop-file-validate assets/asense.desktop

printf '\n==> embedded PayPal QR verification\n'
base64 docs/asense-paypal-qr.png | tr -d '\r\n' >"$temporary/paypal-qr-from-png"
tr -d '\r\n' <src/app/paypal_qr_base64.txt >"$temporary/paypal-qr-embedded"
run cmp "$temporary/paypal-qr-from-png" "$temporary/paypal-qr-embedded"

printf '\n==> shell syntax\n'
while IFS= read -r -d '' script; do
  case "$(sed -n '1p' "$script")" in
    *bash) bash -n "$script" ;;
    *'/sh') sh -n "$script" ;;
    *)
      printf 'asense-verify: unsupported shell shebang: %s\n' "$script" >&2
      exit 1
      ;;
  esac
done < <(
  find install.sh uninstall.sh scripts packaging -type f \
    \( -name '*.sh' -o -name 'asense-system-sleep' \) -print0 | sort -z
)

printf '\n==> systemd sleep-hook argument contract\n'
install -d "$temporary/sleep-bin"
# The single quotes deliberately preserve variables in the generated helper.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/bin/sh' \
  'if [ "$1" = "--quiet" ] && [ "$2" = "is-active" ]; then exit 0; fi' \
  'printf "%s\\n" "$*" >>"$ASENSE_SLEEP_TEST_LOG"' \
  >"$temporary/sleep-bin/systemctl"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$temporary/sleep-bin/logger"
chmod 0755 "$temporary/sleep-bin/systemctl" "$temporary/sleep-bin/logger"
: >"$temporary/sleep-actions"
ASENSE_SLEEP_TEST_LOG="$temporary/sleep-actions" \
  PATH="$temporary/sleep-bin:$PATH" \
  sh packaging/asense-system-sleep post suspend
grep --fixed-strings --line-regexp 'reload asense.service' \
  "$temporary/sleep-actions"
: >"$temporary/sleep-actions"
ASENSE_SLEEP_TEST_LOG="$temporary/sleep-actions" \
  PATH="$temporary/sleep-bin:$PATH" \
  sh packaging/asense-system-sleep pre suspend
[[ ! -s "$temporary/sleep-actions" ]] || {
  printf 'asense-verify: sleep hook reconciled during the pre phase\n' >&2
  exit 1
}

printf '\n==> package-to-standalone ownership guard\n'
install -d "$temporary/dpkg-bin"
printf '%s\n' \
  '#!/bin/sh' \
  'printf "%s" "deinstall ok config-files"' \
  >"$temporary/dpkg-bin/dpkg-query"
chmod 0755 "$temporary/dpkg-bin/dpkg-query"
if PATH="$temporary/dpkg-bin:$PATH" bash install.sh \
  >"$temporary/standalone-guard" 2>&1; then
  printf 'asense-verify: standalone installer accepted residual dpkg state\n' >&2
  exit 1
fi
grep --fixed-strings "run 'sudo apt purge asense'" \
  "$temporary/standalone-guard"

printf '\n==> standalone lifecycle helper behavior\n'
# shellcheck source=packaging/common.sh
source packaging/common.sh
current_account="$(id -un)"
[[ "$current_account" == "root" ]] || {
  asense_try_resolve_target_user "$current_account"
  [[ "$ASENSE_TARGET_USER" == "$current_account" ]]
}
if asense_try_resolve_target_user "asense-deleted-user-$$"; then
  printf 'asense-verify: a deleted desktop account resolved unexpectedly\n' >&2
  exit 1
fi
install -d "$temporary/modules/known/build" \
  "$temporary/modules/present-without-headers"
asense_kernel_headers_available known "$temporary/modules"
asense_kernel_release_present known "$temporary/modules"
asense_kernel_release_present present-without-headers "$temporary/modules"
if asense_kernel_headers_available present-without-headers "$temporary/modules"; then
  printf 'asense-verify: stale kernel without headers was considered buildable\n' >&2
  exit 1
fi
if asense_kernel_release_present removed "$temporary/modules"; then
  printf 'asense-verify: removed kernel was considered installed\n' >&2
  exit 1
fi

printf '\n==> systemd unit verification\n'
sed 's#/usr/libexec/asense/asensed#/bin/true#g' \
  packaging/asense.service >"$temporary/asense.service"
sed -e 's/@TARGET_USER@/root/g' -e 's/@TARGET_GROUP@/root/g' \
  packaging/asense.socket.in >"$temporary/asense.socket"
run systemd-analyze verify "$temporary/asense.service" "$temporary/asense.socket"

printf '\n==> Predator-key HWDB verification\n'
install -D -m 0644 packaging/90-asense-predator-key.hwdb \
  "$temporary/hwdb/etc/udev/hwdb.d/90-asense-predator-key.hwdb"
run systemd-hwdb --root="$temporary/hwdb" --strict update
systemd-hwdb --root="$temporary/hwdb" query \
  'evdev:atkbd:dmi:bvnInsyde:bvrV1.18:bd*:svnAcer:pnPredatorPHN16-72:pvr*' |
  grep --fixed-strings --line-regexp 'KEYBOARD_KEY_f5=prog1'

printf '\n==> exact HID udev verification\n'
run udevadm verify packaging/71-asense-hid.rules

kernel_count=0
for modules in /lib/modules/*; do
  [[ -d "$modules/build" ]] || continue
  release="${modules##*/}"
  kernel_work="$temporary/kernel-$release"
  install -d "$kernel_work"
  install -m 0644 kernel/Makefile kernel/asense_rgb.c "$kernel_work/"
  run make -C "$modules/build" M="$kernel_work" modules
  kernel_count=$((kernel_count + 1))
done
if ((kernel_count == 0)); then
  printf 'asense-verify: no installed kernel headers found under /lib/modules\n' >&2
  exit 1
fi

printf '\nASense release verification passed (%d kernel header tree(s)).\n' "$kernel_count"
