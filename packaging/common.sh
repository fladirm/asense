#!/usr/bin/env bash

# Shared installer helpers.  This file is sourced by install.sh and
# uninstall.sh; it intentionally does not change shell options on its own.

ASENSE_SHORTCUT_PATH="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/asense/"
ASENSE_SHORTCUT_SCHEMA="org.gnome.settings-daemon.plugins.media-keys"
ASENSE_PACKAGE_LOCK="/run/lock/asense-package.lock"
ASENSE_SHORTCUT_STATUS="not-attempted"

asense_die() {
  printf 'asense: %s\n' "$*" >&2
  exit 1
}

asense_warn() {
  printf 'asense: warning: %s\n' "$*" >&2
}

asense_require_command() {
  command -v "$1" >/dev/null 2>&1 || asense_die "required command is missing: $1"
}

asense_init_privilege() {
  if ((EUID == 0)); then
    ASENSE_ROOT_COMMAND=()
  else
    asense_require_command sudo
    sudo -v
    ASENSE_ROOT_COMMAND=(sudo)
  fi
}

asense_root() {
  "${ASENSE_ROOT_COMMAND[@]}" "$@"
}

# Serialize the complete privileged install/uninstall transaction.  `flock`
# owns the descriptor in the privileged parent for the lifetime of the
# re-executed script, so an unprivileged caller never needs write access to the
# root-owned lock file.  The marker cannot be used to bypass the lock because
# it is accepted only in an already-root process.
asense_run_with_package_lock() {
  local script="$1"
  shift
  local status

  if [[ "${ASENSE_PACKAGE_LOCK_HELD:-}" == "1" ]]; then
    ((EUID == 0)) || asense_die "invalid unprivileged package-lock marker"
    return 0
  fi

  asense_init_privilege
  asense_root touch "$ASENSE_PACKAGE_LOCK"
  asense_root chown root:root "$ASENSE_PACKAGE_LOCK"
  asense_root chmod 0600 "$ASENSE_PACKAGE_LOCK"

  if asense_root flock --exclusive --nonblock --conflict-exit-code 75 \
    "$ASENSE_PACKAGE_LOCK" env ASENSE_PACKAGE_LOCK_HELD=1 "$script" "$@"; then
    exit 0
  else
    status=$?
  fi
  if ((status == 75)); then
    asense_die "another ASense install or uninstall transaction is running"
  fi
  exit "$status"
}

asense_resolve_target_user() {
  local account

  account="$(getent passwd "$1")" || asense_die "unknown target user: $1"
  IFS=: read -r _ _ ASENSE_TARGET_UID ASENSE_TARGET_GID _ ASENSE_TARGET_HOME _ <<<"$account"
  [[ "$ASENSE_TARGET_UID" =~ ^[0-9]+$ ]] || asense_die "invalid uid for target user: $1"
  [[ "$ASENSE_TARGET_GID" =~ ^[0-9]+$ ]] || asense_die "invalid gid for target user: $1"
  [[ "$ASENSE_TARGET_HOME" == /* ]] || asense_die "target user has no absolute home: $1"

  ASENSE_TARGET_USER="$1"
  ASENSE_TARGET_GROUP="$(id -gn "$1")" || asense_die "cannot resolve primary group for: $1"
  ASENSE_TARGET_RUNTIME="/run/user/$ASENSE_TARGET_UID"
}

asense_as_target() {
  local -a environment=(
    "HOME=$ASENSE_TARGET_HOME"
    "USER=$ASENSE_TARGET_USER"
    "LOGNAME=$ASENSE_TARGET_USER"
    "XDG_RUNTIME_DIR=$ASENSE_TARGET_RUNTIME"
    "DBUS_SESSION_BUS_ADDRESS=unix:path=$ASENSE_TARGET_RUNTIME/bus"
  )

  if ((EUID == ASENSE_TARGET_UID)); then
    env "${environment[@]}" "$@"
  elif command -v runuser >/dev/null 2>&1; then
    asense_root runuser -u "$ASENSE_TARGET_USER" -- env "${environment[@]}" "$@"
  else
    asense_require_command sudo
    asense_root sudo -H -u "$ASENSE_TARGET_USER" env "${environment[@]}" "$@"
  fi
}

asense_gsettings_ready() {
  command -v gsettings >/dev/null 2>&1 &&
    command -v python3 >/dev/null 2>&1 &&
    [[ -S "$ASENSE_TARGET_RUNTIME/bus" ]]
}

asense_shortcut_list() {
  local action="$1"
  local current
  local updated

  current="$(asense_as_target gsettings get \
    "$ASENSE_SHORTCUT_SCHEMA" custom-keybindings)" || return 1
  updated="$(python3 -c '
import ast
import sys

action, shortcut, raw = sys.argv[1:]
if raw.startswith("@as "):
    raw = raw[4:]
items = ast.literal_eval(raw)
if not isinstance(items, list) or not all(isinstance(item, str) for item in items):
    raise SystemExit("custom-keybindings is not a string array")
items = [item for item in items if item != shortcut]
if action == "add":
    items.append(shortcut)
elif action != "remove":
    raise SystemExit("invalid shortcut-list action")
print("[" + ", ".join(repr(item) for item in items) + "]")
' "$action" "$ASENSE_SHORTCUT_PATH" "$current")" || return 1
  asense_as_target gsettings set \
    "$ASENSE_SHORTCUT_SCHEMA" custom-keybindings "$updated"
}

asense_install_shortcut() {
  local binding="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$ASENSE_SHORTCUT_PATH"

  if ! asense_gsettings_ready; then
    ASENSE_SHORTCUT_STATUS="unavailable"
    asense_warn "GNOME session bus is unavailable; PredatorSense shortcut was not changed"
    return 0
  fi
  ASENSE_SHORTCUT_STATUS="failed"
  asense_shortcut_list add || return 1
  asense_as_target gsettings set "$binding" name "ASense" || return 1
  asense_as_target gsettings set "$binding" command "/usr/bin/asense --toggle" || return 1
  asense_as_target gsettings set "$binding" binding "XF86Launch1" || return 1
  ASENSE_SHORTCUT_STATUS="installed"
}

asense_remove_shortcut() {
  local binding="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$ASENSE_SHORTCUT_PATH"

  if ! asense_gsettings_ready; then
    ASENSE_SHORTCUT_STATUS="unavailable"
    asense_warn "GNOME session bus is unavailable; remove the ASense custom shortcut after login"
    return 0
  fi
  ASENSE_SHORTCUT_STATUS="failed"
  asense_shortcut_list remove || return 1
  asense_as_target gsettings reset-recursively "$binding" || return 1
  ASENSE_SHORTCUT_STATUS="removed"
}

asense_dkms_value() {
  local key="$1"
  local file="$2"
  local value

  value="$(sed -n "s/^${key}=\"\([^\"]*\)\"$/\\1/p" "$file")"
  [[ -n "$value" && "$value" != *$'\n'* ]] ||
    asense_die "cannot read $key from $file"
  printf '%s\n' "$value"
}

asense_dkms_registered() {
  local line
  local prefix_comma="$1/$2,"
  local prefix_colon="$1/$2:"
  local status_output

  status_output="$(asense_root dkms status -m "$1" -v "$2" 2>/dev/null)" || return 2
  while IFS= read -r line; do
    if [[ "$line" == "$prefix_comma"* || "$line" == "$prefix_colon"* ]]; then
      return 0
    fi
  done <<<"$status_output"
  return 1
}

asense_stop_unit_if_loaded() {
  local state

  state="$(asense_root systemctl show --property=LoadState --value "$1" 2>/dev/null || true)"
  if [[ -n "$state" && "$state" != "not-found" ]]; then
    asense_root systemctl stop "$1"
  fi
}

asense_refresh_desktop_caches() {
  if command -v update-desktop-database >/dev/null 2>&1; then
    asense_root update-desktop-database /usr/share/applications
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    asense_root gtk-update-icon-cache --force --ignore-theme-index /usr/share/icons/hicolor
  fi
}
