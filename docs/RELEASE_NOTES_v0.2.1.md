## ASense v0.2.1

This patch release prevents the standalone installer and uninstaller from
overwriting or removing an ASense installation managed by APT/dpkg.

### Changed

- `install.sh` now directs users to APT when the `asense` Debian package is
  installed;
- `uninstall.sh` now directs package-managed users to `apt remove` or
  `apt purge`;
- legacy standalone installations remain upgradeable by the Debian package;
- all ASense v0.2.0 hardware support and runtime behavior are unchanged.

### Supported release baseline

- Ubuntu 26.04 LTS
- x86_64
- GNOME
- GPL-2.0-only

Ubuntu users should install or upgrade through `ppa:fladirmacht/asense`.
The standalone installer ZIP and its matching checksum remain available from
this release. See the README for both installation paths and Secure Boot notes.
