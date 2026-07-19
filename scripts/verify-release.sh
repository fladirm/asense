#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for command in base64 bash cargo cmp desktop-file-validate find grep install make mktemp sed sh sort systemd-analyze systemd-hwdb tr udevadm; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'asense-verify: missing command: %s\n' "$command" >&2
    exit 1
  }
done

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
