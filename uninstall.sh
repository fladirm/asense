#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/common.sh
source "$ROOT/packaging/common.sh"

TARGET_USER="${1:-${SUDO_USER:-${USER:-}}}"
DKMS_CONF="$ROOT/kernel/dkms.conf"
DKMS_NAME="$(asense_dkms_value PACKAGE_NAME "$DKMS_CONF")"
DKMS_VERSION="$(asense_dkms_value PACKAGE_VERSION "$DKMS_CONF")"
DKMS_VERSIONS=()
DKMS_QUERY_STATUS=0
FAN_RESET_STATUS="unavailable"
UNINSTALL_STATUS=0

add_dkms_version() {
  local version="$1"
  local existing

  [[ "$version" =~ ^[A-Za-z0-9][A-Za-z0-9._+~-]*$ ]] ||
    asense_die "unsafe version in DKMS state: $version"
  for existing in "${DKMS_VERSIONS[@]}"; do
    [[ "$existing" != "$version" ]] || return 0
  done
  DKMS_VERSIONS+=("$version")
}

collect_dkms_versions() {
  local line
  local remainder
  local source
  local status_output
  local version

  status_output="$(asense_root dkms status -m "$DKMS_NAME")" ||
    asense_die "cannot inspect existing DKMS state for $DKMS_NAME"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    [[ "$line" == "$DKMS_NAME/"* ]] ||
      asense_die "DKMS returned an unrelated module while filtering $DKMS_NAME: $line"
    remainder="${line#"$DKMS_NAME/"}"
    if [[ "$remainder" =~ ^([^,[:space:]:]+)(,|:) ]]; then
      version="${BASH_REMATCH[1]}"
      add_dkms_version "$version"
    else
      asense_die "cannot parse ASense DKMS state: $line"
    fi
  done <<<"$status_output"
  add_dkms_version "$DKMS_VERSION"
  for source in /usr/src/"$DKMS_NAME"-*; do
    [[ -e "$source" || -L "$source" ]] || continue
    version="${source#"/usr/src/$DKMS_NAME-"}"
    add_dkms_version "$version"
  done
}

for command in chown chmod dkms env flock getent grep modprobe systemctl systemd-hwdb touch udevadm; do
  asense_require_command "$command"
done
asense_run_with_package_lock "$ROOT/uninstall.sh" "$@"
[[ -n "$TARGET_USER" && "$TARGET_USER" != "root" ]] ||
  asense_die "target desktop user is missing; pass it explicitly as the first argument"
asense_resolve_target_user "$TARGET_USER"
asense_init_privilege
asense_root true
collect_dkms_versions

# Disable the activation source first, then stop any already activated helper.
asense_root systemctl disable asense.socket 2>/dev/null || true
asense_stop_unit_if_loaded asense.socket
asense_stop_unit_if_loaded asense.service
# ExecStopPost performs a first best-effort reset.  Verify Auto only after the
# socket and service are both stopped, when no connected GUI can race the final
# readback by submitting another Manual or Maximum request.
if [[ -x /usr/libexec/asense/asensed ]]; then
  if asense_root /usr/libexec/asense/asensed --failsafe-auto; then
    FAN_RESET_STATUS="verified"
  else
    FAN_RESET_STATUS="failed"
    UNINSTALL_STATUS=1
    asense_warn "failsafe fan reset failed; firmware should recover Auto on reboot"
  fi
fi
if [[ -d /sys/module/asense_rgb ]]; then
  asense_root modprobe -r asense_rgb
fi
[[ ! -d /sys/module/asense_rgb ]] || asense_die "asense_rgb remained loaded; uninstall aborted"

for version in "${DKMS_VERSIONS[@]}"; do
  if asense_dkms_registered "$DKMS_NAME" "$version"; then
    asense_root dkms remove --force -m "$DKMS_NAME" -v "$version" --all
  else
    DKMS_QUERY_STATUS=$?
    ((DKMS_QUERY_STATUS == 1)) || asense_die "cannot inspect DKMS registration state"
  fi
  if asense_dkms_registered "$DKMS_NAME" "$version"; then
    asense_die "DKMS registration remains for $DKMS_NAME/$version"
  else
    DKMS_QUERY_STATUS=$?
    ((DKMS_QUERY_STATUS == 1)) || asense_die "cannot verify DKMS removal"
  fi
  asense_root rm -rf -- "/usr/src/$DKMS_NAME-$version"
done

if ! asense_remove_shortcut; then
  asense_warn "system files were removed, but the GNOME shortcut could not be cleaned"
fi

asense_root rm -f -- \
  /etc/systemd/system/asense.socket \
  /etc/systemd/system/asense.service \
  /etc/systemd/system/sockets.target.wants/asense.socket \
  /usr/lib/systemd/system-sleep/asense \
  /etc/udev/hwdb.d/90-asense-predator-key.hwdb \
  /usr/bin/asense \
  /usr/libexec/asense/asense \
  /usr/libexec/asense/asensed \
  /usr/share/applications/asense.desktop \
  /usr/share/icons/hicolor/scalable/apps/asense.svg \
  "$ASENSE_TARGET_HOME/.local/share/applications/asense.desktop" \
  /run/asense-control.sock \
  /run/asense-mutation.lock
asense_root systemd-hwdb update
asense_root udevadm trigger --subsystem-match=input --action=change
asense_root udevadm settle --timeout=10
asense_root systemctl daemon-reload
asense_root systemctl reset-failed asense.service 2>/dev/null || true
asense_refresh_desktop_caches

# Retain the self-contained uninstaller until every fallible system refresh
# above has completed, so a failed transaction can be retried from the same
# installed helper path.
asense_root rm -f -- \
  /usr/libexec/asense/uninstall.sh \
  /usr/libexec/asense/packaging/common.sh \
  /usr/libexec/asense/kernel/dkms.conf \
  /usr/libexec/asense/INSTALL-PROVENANCE.txt
asense_root rmdir /usr/libexec/asense/packaging 2>/dev/null || true
asense_root rmdir /usr/libexec/asense/kernel 2>/dev/null || true
asense_root rmdir /usr/libexec/asense 2>/dev/null || true
# The outer privileged flock still owns the unlinked inode until this script
# exits, while future transactions get a fresh lock path after uninstall.
asense_root rm -f -- "$ASENSE_PACKAGE_LOCK"

case "$FAN_RESET_STATUS" in
  verified)
    printf 'ASense %s removed; firmware Auto fan control was confirmed.\n' "$DKMS_VERSION"
    ;;
  failed)
    printf 'ASense %s removed; firmware Auto fan control could not be confirmed (reboot recommended).\n' \
      "$DKMS_VERSION"
    ;;
  *)
    printf 'ASense %s removed; no installed helper was available to verify firmware Auto fan control.\n' \
      "$DKMS_VERSION"
    ;;
esac
case "$ASENSE_SHORTCUT_STATUS" in
  removed) ;;
  unavailable)
    printf "%s\n" "The selected account's GNOME shortcut remains and must be removed after login."
    ;;
  *)
    printf "%s\n" "The selected account's GNOME shortcut cleanup was not confirmed."
    ;;
esac
exit "$UNINSTALL_STATUS"
