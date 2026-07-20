#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/common.sh
source "$ROOT/packaging/common.sh"

refuse_package_managed_install() {
  local status

  command -v dpkg-query >/dev/null 2>&1 || return 0
  status="$(dpkg-query -W -f='${Status}' asense 2>/dev/null || true)"
  case "$status" in
    "" | "purge ok not-installed" | "unknown ok not-installed")
      return 0
      ;;
    "deinstall ok config-files")
      asense_die "the removed Debian package still owns purge-time ASense state; run 'sudo apt purge asense' before using the standalone installer"
      ;;
    *)
      asense_die "ASense is managed by APT or dpkg is changing it; use 'sudo apt update && sudo apt install --only-upgrade asense', or purge the package before using the standalone installer"
      ;;
  esac
}

refuse_package_managed_install

absolute_payload_path() {
  local path="$1"
  local directory

  if directory="$(cd -- "$(dirname -- "$path")" 2>/dev/null && pwd -P)"; then
    printf '%s/%s\n' "$directory" "$(basename -- "$path")"
  else
    # Preserve the original value so the normal executable check below can
    # report which payload is missing.
    printf '%s\n' "$path"
  fi
}

DEFAULT_BINARY="$ROOT/bin/asense"
[[ -x "$DEFAULT_BINARY" ]] || DEFAULT_BINARY="$ROOT/target/release/asense"
BINARY="$(absolute_payload_path "${1:-$DEFAULT_BINARY}")"
# An explicit GUI path denotes a binary pair.  Resolve the helper beside that
# GUI by default so installing a bundle from outside ROOT cannot silently pick
# up a stale target/release/asensed from this checkout.
DEFAULT_DAEMON_BINARY="$(dirname -- "$BINARY")/asensed"
DAEMON_BINARY="$(absolute_payload_path "${ASENSE_DAEMON_BINARY:-$DEFAULT_DAEMON_BINARY}")"
TARGET_USER="${2:-${SUDO_USER:-${USER:-}}}"
DKMS_CONF="$ROOT/kernel/dkms.conf"
DKMS_NAME="$(asense_dkms_value PACKAGE_NAME "$DKMS_CONF")"
DKMS_VERSION="$(asense_dkms_value PACKAGE_VERSION "$DKMS_CONF")"
DKMS_SOURCE="/usr/src/$DKMS_NAME-$DKMS_VERSION"
GAMING_GUID="7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56"
BATTERY_GUID="79772EC5-04B1-4BFD-843C-61E7F77B6CC9"
APGE_GUID="61EF69EA-865C-4BC3-A502-A0DEBA0CB531"
KERNEL_RELEASE="$(uname -r)"
SYSTEM_VENDOR="$(sed -n '1p' /sys/class/dmi/id/sys_vendor 2>/dev/null || true)"
SYSTEM_PRODUCT="$(sed -n '1p' /sys/class/dmi/id/product_name 2>/dev/null || true)"
IS_ACER=0
IS_REFERENCE_MODEL=0
HAS_KNOWN_WMI=0
HAS_BINDABLE_WMI=0
INSTALL_DKMS=0
WMI_CONFLICTS=()
KERNEL_RELEASES=()
OLD_DKMS_VERSIONS=()
OLD_DKMS_REGISTERED_VERSIONS=()
OLD_DKMS_RECORD_VERSIONS=()
OLD_DKMS_RECORD_KERNELS=()
OLD_DKMS_RECORD_STATES=()

TEMP_DIR=""
BACKUP_DIR=""
PROVENANCE_FILE=""
CANDIDATE_VERSION=""
CANDIDATE_SOURCE=""
CANDIDATE_REGISTERED=0
ROLLBACK_ARMED=0
OLD_MODULE_LOADED=0
OLD_SOCKET_ACTIVE=0
OLD_SOCKET_ENABLED=0
OLD_SERVICE_ACTIVE=0
OLD_MODULE_PATH=""

PACKAGE_PATHS=(
  /usr/libexec/asense/asense
  /usr/libexec/asense/asensed
  /usr/libexec/asense/uninstall.sh
  /usr/libexec/asense/packaging/common.sh
  /usr/libexec/asense/kernel/dkms.conf
  /usr/libexec/asense/INSTALL-PROVENANCE.txt
  /usr/bin/asense
  /etc/systemd/system/asense.service
  /etc/systemd/system/asense.socket
  /usr/lib/systemd/system-sleep/asense
  /etc/udev/rules.d/71-asense-hid.rules
  /etc/udev/hwdb.d/90-asense-predator-key.hwdb
  /usr/share/applications/asense.desktop
  /usr/share/icons/hicolor/scalable/apps/asense.svg
)

root_path_exists() {
  asense_root test -e "$1" || asense_root test -L "$1"
}

add_kernel_release() {
  local candidate="$1"
  local existing

  [[ -n "$candidate" && "$candidate" != */* ]] ||
    asense_die "unsafe kernel release in DKMS state: $candidate"
  asense_kernel_headers_available "$candidate" || return 1
  for existing in "${KERNEL_RELEASES[@]}"; do
    [[ "$existing" != "$candidate" ]] || return 0
  done
  KERNEL_RELEASES+=("$candidate")
}

validate_dkms_component() {
  local label="$1"
  local value="$2"

  [[ "$value" =~ ^[A-Za-z0-9][A-Za-z0-9._+~-]*$ ]] ||
    asense_die "unsafe $label in DKMS state: $value"
}

add_old_dkms_version() {
  local version="$1"
  local existing

  validate_dkms_component version "$version"
  for existing in "${OLD_DKMS_VERSIONS[@]}"; do
    [[ "$existing" != "$version" ]] || return 0
  done
  OLD_DKMS_VERSIONS+=("$version")
}

add_old_registered_version() {
  local version="$1"
  local existing

  add_old_dkms_version "$version"
  for existing in "${OLD_DKMS_REGISTERED_VERSIONS[@]}"; do
    [[ "$existing" != "$version" ]] || return 0
  done
  OLD_DKMS_REGISTERED_VERSIONS+=("$version")
}

record_old_dkms_state() {
  local version="$1"
  local kernel="$2"
  local state="$3"

  add_old_registered_version "$version"
  if [[ -n "$kernel" ]]; then
    validate_dkms_component kernel "$kernel"
    if ! add_kernel_release "$kernel"; then
      if asense_kernel_release_present "$kernel"; then
        asense_die "kernel $kernel is still installed but its headers are missing; install the matching headers or remove that kernel before upgrading ASense"
      fi
      # A DKMS registration can outlive a removed kernel and its headers. It
      # is safe to discard only that orphaned target during the upgrade.
      asense_warn "ignoring stale DKMS target $version for removed kernel $kernel"
      return 0
    fi
  fi
  OLD_DKMS_RECORD_VERSIONS+=("$version")
  OLD_DKMS_RECORD_KERNELS+=("$kernel")
  OLD_DKMS_RECORD_STATES+=("$state")
}

collect_old_dkms_state() {
  local line
  local remainder
  local version
  local release
  local state
  local status_output
  local source

  if ((INSTALL_DKMS)); then
    add_kernel_release "$KERNEL_RELEASE" ||
      asense_die "kernel headers are missing for $KERNEL_RELEASE"
  fi
  status_output="$(asense_root dkms status -m "$DKMS_NAME")" ||
    asense_die "cannot inspect existing DKMS state for $DKMS_NAME"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    [[ "$line" == "$DKMS_NAME/"* ]] ||
      asense_die "DKMS returned an unrelated module while filtering $DKMS_NAME: $line"
    remainder="${line#"$DKMS_NAME/"}"
    if [[ "$remainder" =~ ^([^,[:space:]:]+),[[:space:]]+([^,[:space:]]+),[[:space:]]+[^:]+:[[:space:]]+(installed|built)([[:space:]]|$) ]]; then
      version="${BASH_REMATCH[1]}"
      release="${BASH_REMATCH[2]}"
      state="${BASH_REMATCH[3]}"
      record_old_dkms_state "$version" "$release" "$state"
    elif [[ "$remainder" =~ ^([^,[:space:]:]+):[[:space:]]+added([[:space:]]|$) ]]; then
      version="${BASH_REMATCH[1]}"
      record_old_dkms_state "$version" "" "added"
    else
      asense_die "cannot preserve unrecognized DKMS state: $line"
    fi
  done <<<"$status_output"

  # Preserve an unregistered source tree for the incoming version too.  This
  # can exist after a manually interrupted development installation.
  if root_path_exists "$DKMS_SOURCE"; then
    add_old_dkms_version "$DKMS_VERSION"
  fi

  for version in "${OLD_DKMS_REGISTERED_VERSIONS[@]}"; do
    source="/usr/src/$DKMS_NAME-$version"
    root_path_exists "$source/dkms.conf" ||
      asense_die "registered DKMS source is missing and cannot be rolled back: $source"
  done
}

remove_registered_asense_versions() {
  local version

  for version in "${OLD_DKMS_REGISTERED_VERSIONS[@]}"; do
    asense_root dkms remove --force -m "$DKMS_NAME" -v "$version" --all
  done
}

backup_path() {
  local path="$1"
  local destination="$BACKUP_DIR/files$path"

  if root_path_exists "$path"; then
    asense_root install -d -m 0700 "$(dirname -- "$destination")"
    asense_root cp -a -- "$path" "$destination"
  fi
}

restore_path() {
  local path="$1"
  local source="$BACKUP_DIR/files$path"

  asense_root rm -rf -- "$path"
  if root_path_exists "$source"; then
    asense_root install -d -m 0755 "$(dirname -- "$path")"
    asense_root cp -a -- "$source" "$path"
  fi
}

render_dkms_source() {
  local destination="$1"
  local version="$2"
  local rendered="$TEMP_DIR/dkms.conf.$version"

  sed "s/^PACKAGE_VERSION=\"[^\"]*\"$/PACKAGE_VERSION=\"$version\"/" \
    "$DKMS_CONF" >"$rendered"
  asense_root install -d -o root -g root -m 0755 "$destination"
  asense_root install -o root -g root -m 0644 \
    "$ROOT/kernel/asense_rgb.c" "$ROOT/kernel/Makefile" "$destination/"
  asense_root install -o root -g root -m 0644 "$rendered" "$destination/dkms.conf"
}

inspect_wmi_endpoints() {
  local guid
  local owner
  local path
  local target

  for guid in "$GAMING_GUID" "$BATTERY_GUID" "$APGE_GUID"; do
    for path in /sys/bus/wmi/devices/"$guid" /sys/bus/wmi/devices/"$guid"-*; do
      [[ -d "$path" ]] || continue
      HAS_KNOWN_WMI=1
      if [[ -L "$path/driver" ]]; then
        target="$(readlink -- "$path/driver" 2>/dev/null || true)"
        if [[ -n "$target" ]]; then
          owner="${target##*/}"
          if [[ "$owner" != "asense_rgb" ]]; then
            WMI_CONFLICTS+=("${path##*/}:$owner")
            continue
          fi
        fi
      fi
      HAS_BINDABLE_WMI=1
    done
  done
}

ping_control_service() {
  asense_as_target python3 - <<'PY'
import re
import socket

client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.settimeout(5.0)
client.connect("/run/asense-control.sock")
reader = client.makefile("rb")
client.sendall(b"HELLO 2\n")
handshake = reader.readline(4097)
if re.fullmatch(rb"OK protocol=2 daemon=[^ \r\n]+\n", handshake) is None:
    raise SystemExit(f"unexpected ASense protocol handshake: {handshake!r}")
client.sendall(b"PING\n")
response = reader.readline(4097)
if response != b"OK ready\n":
    raise SystemExit(f"unexpected ASense control response: {response!r}")
PY
  asense_root systemctl is-active --quiet asense.service ||
    asense_die "socket activation did not leave asense.service active"
}

verify_control_service() {
  # Exercise every command wired into the unit: socket-activated ExecStart,
  # resume reconciliation through ExecReload, and the failsafe ExecStopPost.
  ping_control_service
  asense_root systemctl reload asense.service ||
    asense_die "asense.service reload smoke test failed"
  ping_control_service
  asense_root systemctl stop asense.service ||
    asense_die "asense.service stop smoke test failed"
  if asense_root systemctl is-active --quiet asense.service; then
    asense_die "asense.service remained active after its stop smoke test"
  fi
  ping_control_service
}

manifest_value() {
  local key="$1"
  local manifest="$2"
  local value

  value="$(sed -n "s/^${key}=//p" "$manifest")"
  [[ -n "$value" && "$value" != *$'\n'* ]] ||
    asense_die "cannot read unique $key from $manifest"
  printf '%s\n' "$value"
}

create_install_provenance() {
  local release_manifest="$ROOT/RELEASE-MANIFEST.txt"
  local gui_hash
  local daemon_hash
  local commit="unavailable"

  gui_hash="$(sha256sum "$BINARY" | cut -d' ' -f1)"
  daemon_hash="$(sha256sum "$DAEMON_BINARY" | cut -d' ' -f1)"

  if [[ -f "$release_manifest" && "$BINARY" -ef "$ROOT/bin/asense" &&
    "$DAEMON_BINARY" -ef "$ROOT/bin/asensed" ]]; then
    if ! sed -n '/^\[payload-sha256\]$/,$p' "$release_manifest" |
      sed '1d' | (cd "$ROOT" && sha256sum --check --strict --quiet -); then
      asense_die "installer payload does not match RELEASE-MANIFEST.txt"
    fi
    [[ "$(manifest_value version "$release_manifest")" == "$DKMS_VERSION" ]] ||
      asense_die "release manifest version does not match DKMS payload"
    [[ "$(manifest_value gui_sha256 "$release_manifest")" == "$gui_hash" ]] ||
      asense_die "GUI binary does not match RELEASE-MANIFEST.txt"
    [[ "$(manifest_value daemon_sha256 "$release_manifest")" == "$daemon_hash" ]] ||
      asense_die "daemon binary does not match RELEASE-MANIFEST.txt"
    install -m 0644 "$release_manifest" "$PROVENANCE_FILE"
    return 0
  fi

  if command -v git >/dev/null 2>&1; then
    commit="$(git -C "$ROOT" rev-parse --verify HEAD 2>/dev/null || printf unavailable)"
  fi
  {
    printf 'version=%s\n' "$DKMS_VERSION"
    printf 'commit=%s\n' "$commit"
    printf 'origin=local-build\n'
    printf 'gui_sha256=%s\n' "$gui_hash"
    printf 'daemon_sha256=%s\n' "$daemon_hash"
  } >"$PROVENANCE_FILE"
}

restore_old_dkms() {
  local index
  local release
  local state
  local version
  local restored=0
  local rebuild_ok=1
  local running_module_rebuilt=0

  asense_root modprobe -r asense_rgb 2>/dev/null || true
  asense_root dkms remove --force -m "$DKMS_NAME" -v "$DKMS_VERSION" --all \
    >/dev/null 2>&1 || true
  asense_root rm -rf -- "$DKMS_SOURCE"

  for version in "${OLD_DKMS_VERSIONS[@]}"; do
    restore_path "/usr/src/$DKMS_NAME-$version"
  done

  for version in "${OLD_DKMS_REGISTERED_VERSIONS[@]}"; do
    if ! asense_root dkms add -m "$DKMS_NAME" -v "$version"; then
      rebuild_ok=0
      break
    fi
  done
  if ((rebuild_ok)); then
    for index in "${!OLD_DKMS_RECORD_VERSIONS[@]}"; do
      version="${OLD_DKMS_RECORD_VERSIONS[$index]}"
      release="${OLD_DKMS_RECORD_KERNELS[$index]}"
      state="${OLD_DKMS_RECORD_STATES[$index]}"
      [[ "$state" == "added" ]] && continue
      if ! asense_root dkms build -m "$DKMS_NAME" -v "$version" -k "$release"; then
        rebuild_ok=0
        break
      fi
      if [[ "$state" == "installed" ]] &&
        ! asense_root dkms install -m "$DKMS_NAME" -v "$version" -k "$release"; then
        rebuild_ok=0
        break
      fi
      if [[ "$state" == "installed" && "$release" == "$KERNEL_RELEASE" ]]; then
        running_module_rebuilt=1
      fi
    done
  fi
  # Rebuilding an empty/added-only DKMS inventory does not restore a module
  # that had been loaded manually. In that case the exact backed-up module
  # file below is the only valid runtime rollback source.
  if ((rebuild_ok && (!OLD_MODULE_LOADED || running_module_rebuilt))); then
    restored=1
  fi

  if ((!restored)) && [[ -n "$OLD_MODULE_PATH" ]] &&
    root_path_exists "$BACKUP_DIR/files$OLD_MODULE_PATH"; then
    restore_path "$OLD_MODULE_PATH"
    asense_root depmod -a "$KERNEL_RELEASE"
    restored=1
  fi

  if ((OLD_MODULE_LOADED)); then
    asense_root modprobe asense_rgb || restored=0
  fi
  ((restored)) || asense_warn "automatic DKMS rollback was incomplete; inspect dkms status"
}

restore_old_package() {
  local path

  for path in "${PACKAGE_PATHS[@]}" "$ASENSE_TARGET_HOME/.local/share/applications/asense.desktop"; do
    restore_path "$path"
  done
  asense_root systemctl daemon-reload || true
  asense_root systemd-hwdb update || true
  asense_root udevadm control --reload-rules || true
  asense_root udevadm trigger --subsystem-match=hidraw --action=change || true
  asense_root udevadm trigger --subsystem-match=input --action=change || true
  asense_refresh_desktop_caches || true

  if ((OLD_SOCKET_ENABLED)); then
    asense_root systemctl enable asense.socket || true
  else
    asense_root systemctl disable asense.socket 2>/dev/null || true
  fi
  if ((OLD_SOCKET_ACTIVE)); then
    asense_root systemctl start asense.socket || true
  fi
  if ((OLD_SERVICE_ACTIVE)); then
    asense_root systemctl start asense.service || true
  fi
}

cleanup() {
  local status=$?

  trap - EXIT INT TERM
  set +e
  if ((status != 0 && ROLLBACK_ARMED)); then
    asense_warn "installation failed; restoring the previous ASense installation"
    asense_stop_unit_if_loaded asense.socket
    asense_stop_unit_if_loaded asense.service
    restore_old_dkms
    restore_old_package
  fi
  if ((CANDIDATE_REGISTERED)); then
    asense_root dkms remove --force -m "$DKMS_NAME" -v "$CANDIDATE_VERSION" --all >/dev/null 2>&1
  fi
  [[ -z "$CANDIDATE_SOURCE" ]] || asense_root rm -rf -- "$CANDIDATE_SOURCE"
  [[ -z "$BACKUP_DIR" ]] || asense_root rm -rf -- "$BACKUP_DIR"
  [[ -z "$TEMP_DIR" ]] || rm -rf -- "$TEMP_DIR"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for command in chown chmod cut dkms env flock getent grep install make modinfo modprobe python3 readlink sed sha256sum systemctl systemd-hwdb touch udevadm; do
  asense_require_command "$command"
done
asense_run_with_package_lock "$ROOT/install.sh" "$@"
[[ -n "$TARGET_USER" && "$TARGET_USER" != "root" ]] ||
  asense_die "target desktop user is missing; pass it explicitly as the second argument"
[[ -x "$BINARY" ]] || asense_die "release binary not found: $BINARY (run: cargo build --release)"
[[ -x "$DAEMON_BINARY" ]] ||
  asense_die "release daemon not found: $DAEMON_BINARY (run: cargo build --release --bin asensed --no-default-features)"
asense_resolve_target_user "$TARGET_USER"
asense_init_privilege
asense_root true

[[ "$SYSTEM_VENDOR" == "Acer" ]] && IS_ACER=1
[[ "$SYSTEM_VENDOR" == "Acer" && "$SYSTEM_PRODUCT" == "Predator PHN16-72" ]] &&
  IS_REFERENCE_MODEL=1
if ((IS_ACER)); then
  inspect_wmi_endpoints
fi
if ((IS_ACER && HAS_BINDABLE_WMI)); then
  INSTALL_DKMS=1
fi
if ((!IS_ACER)); then
  asense_warn "non-Acer system: installing the generic read-only application; Acer firmware controls stay unavailable"
elif ((!HAS_KNOWN_WMI)); then
  asense_warn "no known Acer WMI endpoint is currently present; kernel-backed profiles/hwmon and generic telemetry remain available"
fi
for conflict in "${WMI_CONFLICTS[@]}"; do
  asense_warn "WMI endpoint ${conflict%%:*} is already owned by ${conflict#*:}; ASense will not create a second writer"
done
if ((!INSTALL_DKMS && HAS_KNOWN_WMI)); then
  asense_warn "all known Acer WMI endpoints are already owned; installing kernel-backed and read-only controls only"
fi
if ((INSTALL_DKMS)); then
  [[ -d "/lib/modules/$KERNEL_RELEASE/build" ]] ||
    asense_die "kernel headers are missing for $KERNEL_RELEASE"
fi
collect_old_dkms_state

TEMP_DIR="$(mktemp -d)"
BACKUP_DIR="$(asense_root mktemp -d /var/tmp/asense-install-backup.XXXXXX)"
asense_root chmod 0700 "$BACKUP_DIR"
PROVENANCE_FILE="$TEMP_DIR/INSTALL-PROVENANCE.txt"
create_install_provenance
if ((INSTALL_DKMS)); then
  # Build the exact candidate through DKMS while the current installed module
  # is still registered and running. This makes compilation failure
  # non-destructive.
  CANDIDATE_VERSION="$DKMS_VERSION.candidate.$PPID.$$"
  CANDIDATE_SOURCE="/usr/src/$DKMS_NAME-$CANDIDATE_VERSION"
  render_dkms_source "$CANDIDATE_SOURCE" "$CANDIDATE_VERSION"
  CANDIDATE_REGISTERED=1
  asense_root dkms add -m "$DKMS_NAME" -v "$CANDIDATE_VERSION"
  for release in "${KERNEL_RELEASES[@]}"; do
    asense_root dkms build -m "$DKMS_NAME" -v "$CANDIDATE_VERSION" -k "$release"
  done
  asense_root dkms remove --force -m "$DKMS_NAME" -v "$CANDIDATE_VERSION" --all
  CANDIDATE_REGISTERED=0
  asense_root rm -rf -- "$CANDIDATE_SOURCE"
  CANDIDATE_SOURCE=""
fi

asense_root systemctl is-active --quiet asense.socket && OLD_SOCKET_ACTIVE=1 || true
asense_root systemctl is-enabled --quiet asense.socket && OLD_SOCKET_ENABLED=1 || true
asense_root systemctl is-active --quiet asense.service && OLD_SERVICE_ACTIVE=1 || true
[[ -d /sys/module/asense_rgb ]] && OLD_MODULE_LOADED=1
OLD_MODULE_PATH="$(modinfo -n asense_rgb 2>/dev/null || true)"
[[ "$OLD_MODULE_PATH" == /* ]] || OLD_MODULE_PATH=""

for path in "${PACKAGE_PATHS[@]}" "$ASENSE_TARGET_HOME/.local/share/applications/asense.desktop"; do
  backup_path "$path"
done
for version in "${OLD_DKMS_VERSIONS[@]}"; do
  backup_path "/usr/src/$DKMS_NAME-$version"
done
[[ -z "$OLD_MODULE_PATH" ]] || backup_path "$OLD_MODULE_PATH"
ROLLBACK_ARMED=1

# Stop the socket first so it cannot reactivate the service while files and
# the kernel module are being replaced.
asense_stop_unit_if_loaded asense.socket
asense_stop_unit_if_loaded asense.service
if [[ -d /sys/module/asense_rgb ]]; then
  asense_root modprobe -r asense_rgb
fi
[[ ! -d /sys/module/asense_rgb ]] || asense_die "asense_rgb remained loaded after modprobe -r"

remove_registered_asense_versions
for version in "${OLD_DKMS_VERSIONS[@]}"; do
  asense_root rm -rf -- "/usr/src/$DKMS_NAME-$version"
done
if ((INSTALL_DKMS)); then
  render_dkms_source "$DKMS_SOURCE" "$DKMS_VERSION"
  asense_root dkms add -m "$DKMS_NAME" -v "$DKMS_VERSION"
  for release in "${KERNEL_RELEASES[@]}"; do
    asense_root dkms build -m "$DKMS_NAME" -v "$DKMS_VERSION" -k "$release"
    asense_root dkms install -m "$DKMS_NAME" -v "$DKMS_VERSION" -k "$release"
  done
  asense_root modprobe asense_rgb
  [[ -d /sys/module/asense_rgb ]] || asense_die "asense_rgb did not remain loaded"
  asense_root udevadm settle --timeout=10
fi
"$DAEMON_BINARY" --probe >/dev/null

SOCKET_RENDERED="$TEMP_DIR/asense.socket"
sed -e "s/@TARGET_USER@/$ASENSE_TARGET_USER/g" \
  -e "s/@TARGET_GROUP@/$ASENSE_TARGET_GROUP/g" \
  "$ROOT/packaging/asense.socket.in" >"$SOCKET_RENDERED"

asense_root install -D -o root -g root -m 0755 "$BINARY" /usr/libexec/asense/asense
asense_root install -D -o root -g root -m 0755 \
  "$DAEMON_BINARY" /usr/libexec/asense/asensed
asense_root install -D -o root -g root -m 0755 \
  "$ROOT/uninstall.sh" /usr/libexec/asense/uninstall.sh
asense_root install -D -o root -g root -m 0644 \
  "$ROOT/packaging/common.sh" /usr/libexec/asense/packaging/common.sh
asense_root install -D -o root -g root -m 0644 \
  "$ROOT/kernel/dkms.conf" /usr/libexec/asense/kernel/dkms.conf
asense_root install -D -o root -g root -m 0644 \
  "$PROVENANCE_FILE" /usr/libexec/asense/INSTALL-PROVENANCE.txt
asense_root ln -sfn /usr/libexec/asense/asense /usr/bin/asense
asense_root install -D -o root -g root -m 0644 \
  "$ROOT/packaging/asense.service" /etc/systemd/system/asense.service
asense_root install -D -o root -g root -m 0644 \
  "$SOCKET_RENDERED" /etc/systemd/system/asense.socket
asense_root install -D -o root -g root -m 0755 \
  "$ROOT/packaging/asense-system-sleep" /usr/lib/systemd/system-sleep/asense
if ((IS_ACER)); then
  asense_root install -D -o root -g root -m 0644 \
    "$ROOT/packaging/71-asense-hid.rules" /etc/udev/rules.d/71-asense-hid.rules
else
  asense_root rm -f -- /etc/udev/rules.d/71-asense-hid.rules
fi
asense_root install -D -o root -g root -m 0644 \
  "$ROOT/assets/asense.desktop" /usr/share/applications/asense.desktop
asense_root install -D -o root -g root -m 0644 \
  "$ROOT/assets/asense.svg" /usr/share/icons/hicolor/scalable/apps/asense.svg
if ((IS_REFERENCE_MODEL)); then
  asense_root install -D -o root -g root -m 0644 \
    "$ROOT/packaging/90-asense-predator-key.hwdb" \
    /etc/udev/hwdb.d/90-asense-predator-key.hwdb
else
  asense_root rm -f -- /etc/udev/hwdb.d/90-asense-predator-key.hwdb
fi

# Remove the obsolete per-user duplicate; the system desktop entry is visible
# to the target user and has one authoritative lifecycle.
asense_root rm -f -- "$ASENSE_TARGET_HOME/.local/share/applications/asense.desktop"
asense_root systemd-hwdb update
asense_root udevadm control --reload-rules
asense_root udevadm trigger --subsystem-match=hidraw --action=change
asense_root udevadm settle --timeout=10
if ((IS_REFERENCE_MODEL)); then
  asense_root udevadm trigger --subsystem-match=input --action=change
  asense_root udevadm settle --timeout=10
fi
asense_root systemctl daemon-reload
asense_root systemctl enable --now asense.socket
asense_root systemctl is-active --quiet asense.socket || asense_die "asense.socket is not active"
verify_control_service
asense_refresh_desktop_caches

ROLLBACK_ARMED=0
if ((IS_REFERENCE_MODEL)) && ! asense_install_shortcut; then
  asense_warn "system install succeeded, but the GNOME PredatorSense shortcut could not be installed"
fi

if ((IS_REFERENCE_MODEL)); then
  case "$ASENSE_SHORTCUT_STATUS" in
    installed)
      printf 'ASense %s installed for %s (Predator key: XF86Launch1).\n' \
        "$DKMS_VERSION" "$ASENSE_TARGET_USER"
      ;;
    unavailable)
      printf 'ASense %s installed for %s; Predator-key shortcut setup is pending.\n' \
        "$DKMS_VERSION" "$ASENSE_TARGET_USER"
      ;;
    *)
      printf 'ASense %s installed for %s; Predator-key shortcut setup failed.\n' \
        "$DKMS_VERSION" "$ASENSE_TARGET_USER"
      ;;
  esac
else
  printf 'ASense %s installed for %s; launch it from the desktop menu or /usr/bin/asense.\n' \
    "$DKMS_VERSION" "$ASENSE_TARGET_USER"
fi
if ((INSTALL_DKMS)); then
  printf 'DKMS kernels: %s\n' "${KERNEL_RELEASES[*]}"
else
  printf 'DKMS: not installed (no unclaimed known Acer WMI endpoint).\n'
fi
