#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for command in base64 bash cargo cmp desktop-file-validate find grep install make mktemp rustup sed sh sort systemd-analyze systemd-hwdb tr udevadm; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'asense-verify: missing command: %s\n' "$command" >&2
    exit 1
  }
done

# Distro cargo/rustc can precede rustup in PATH while clippy-driver still comes
# from rustup. That silently mixes compiler metadata between test and Clippy
# phases. When this repository's pinned rustup toolchain is available, put its
# complete bin directory first so Cargo, rustc, rustfmt and Clippy stay aligned.
cargo_command=(cargo)
[[ -f rust-toolchain.toml ]] || {
  printf 'asense-verify: rust-toolchain.toml is missing\n' >&2
  exit 1
}
pinned_toolchain="$(
  sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
    rust-toolchain.toml
)"
[[ -n "$pinned_toolchain" && "$pinned_toolchain" != *$'\n'* ]] || {
  printf 'asense-verify: could not resolve pinned repository Rust toolchain\n' >&2
  exit 1
}
toolchain_cargo="$(rustup which --toolchain "$pinned_toolchain" cargo)" || {
  printf 'asense-verify: repository Cargo is not installed\n' >&2
  exit 1
}
toolchain_bin="${toolchain_cargo%/*}"
PATH="$toolchain_bin:$PATH"
RUSTC="$toolchain_bin/rustc"
RUSTDOC="$toolchain_bin/rustdoc"
export PATH RUSTC RUSTDOC
cargo_command=("$toolchain_cargo")

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

run "${cargo_command[@]}" fmt --all -- --check
run "${cargo_command[@]}" test --locked --all-targets --all-features
run "${cargo_command[@]}" test --locked --test kernel_rgb_protocol
run "${cargo_command[@]}" clippy --locked --all-targets --all-features -- -D warnings

# Keep the privileged helper independently buildable without GTK/WebKit or any
# other default feature pulled in by the desktop application.
run "${cargo_command[@]}" clippy --locked --bin asensed --no-default-features -- -D warnings
run "${cargo_command[@]}" build --release --locked --bin asensed --no-default-features
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
