# ASense

**CZ:** Nativní ovládací panel pro Acer Predator Helios Neo 16
**PHN16-72** na Linuxu. ASense spojuje výkonové profily, ventilátory,
čtyřzónové RGB, vybrané platformní funkce a živou hardwarovou telemetrii do
jedné CZ/EN aplikace. Firmware transport je záměrně povolen jen pro ověřený
model **PHN16-72**; BIOS verze se reportuje diagnosticky, ale není umělým
compatibility gate.

**EN:** A native Linux control panel for the Acer Predator Helios Neo 16
**PHN16-72**. ASense combines performance profiles, fan control, four-zone
RGB, selected platform controls and live hardware telemetry in one bilingual
CZ/EN application. Firmware writes are deliberately gated to the verified
**PHN16-72** model. The BIOS version is reported diagnostically, while
capability probes and verified readback protect writes across BIOS updates.

## Screenshots / Náhledy

<p align="center">
  <img src="docs/screenshots/asense-compact.png" alt="ASense compact control panel" width="32%">
  <img src="docs/screenshots/asense-advanced.png" alt="ASense advanced metrics panel" width="63%">
</p>

## Features / Funkce

- Eco, Quiet, Balanced, Performance and Turbo firmware profiles;
- firmware Auto, independent manual CPU/GPU and Maximum fan modes;
- live CPU/GPU temperature, load and fan RPM gauges;
- NVIDIA load, VRAM, clocks, power, P-state and throttle-reason telemetry;
- OEM Turbo GPU offsets (`+100 MHz` core, `+200 MHz` memory) with NVML
  readback, rollback and a typed receipt of the confirmed profile, offsets and
  available GPU power ceiling;
- four-zone keyboard RGB, brightness and firmware effects;
- 80% battery charge limit and confirmation-gated firmware battery
  calibration;
- powered-off USB charging thresholds;
- keyboard timeout, boot sound, LCD override and rear-logo controls when the
  firmware exposes them;
- tabbed advanced metrics and read-only CPU, GPU, memory, firmware and PCI
  hardware inventory;
- PredatorSense hardware-key integration on GNOME (`XF86Launch1`).

Unsupported controls stay visibly disabled. ASense does not guess unknown WMI
methods, scan EC registers or silently emulate missing firmware functions.
Battery calibration reports only the firmware-supported and active readback;
there is no invented progress percentage. Its modal supplements that readback
with live battery state-of-charge, charge status and AC/USB source telemetry.
As a fail-closed ASense safety policy, USB-C-only power is blocked for starting
calibration. Keep an AC adapter connected for the entire cycle. The public
firmware interface exposes no decoded completion event; after the cycle,
refresh the readback and stop calibration manually if it remains active.

## Supported hardware / Podporovaný hardware

| Component | Required value |
| --- | --- |
| Vendor | Acer |
| Product | Predator PHN16-72 |
| BIOS | reported, not version-gated |
| Tested GPU path | NVIDIA GeForce RTX 4070 Laptop, Acer subsystem `1025:1731` |
| Current validation stack | Ubuntu 26.04 / glibc 2.43, Linux `7.1.3-070103-generic`, NVIDIA open KMD `610.43.02` |
| Desktop integration | GNOME on Linux |

The installer and kernel module both enforce this boundary. Supporting another
Predator model requires an explicit review of its DMI identity and firmware
protocol; changing a string is not considered support.

Standard Linux interfaces such as `hwmon` and `platform_profile` may also be
present on other Acer systems, but this release neither certifies nor promises
their firmware-specific behaviour. The private RGB/platform WMI protocol and
OEM NVIDIA profile are PHN16-72-specific.

The validation stack records the current release test environment; it is not a
substitute for the identity and capability gates above and does not turn other
driver, kernel or distribution combinations into certified targets.

The installer aborts before making changes on any product other than the exact
`Acer / Predator PHN16-72` identity. Other Acer and Predator models do not get
an unofficial partial-control mode. The NVIDIA GPU is optional for the Acer
control plane, but OEM NVIDIA writes are enabled only for the tested RTX 4070
PCI/subsystem identity shown above.

## Architecture / Architektura

ASense is primarily written in **Rust 2024**. The desktop UI uses
**Dioxus 0.7 + GTK/WebKitGTK**. An unprivileged GUI talks through a protected
Unix socket to a root-owned, systemd socket-activated Rust helper. A small
**GPL-2.0-only C/DKMS kernel module** exposes only the WMI operations verified
for this model. NVIDIA telemetry and OEM clock offsets use NVML directly.

The privileged protocol requires a newline-terminated `HELLO 1` as its first
frame and returns a `protocol=1` daemon receipt; rejected negotiation closes the
session. After negotiation it accepts only exact `OK` or `ERR` response tokens.
A valid command `ERR` leaves the healthy session open; only a transport failure,
timeout or malformed protocol response invalidates it. Manual and Maximum fan
modes return to firmware Auto when the GUI actually disconnects or the service
stops.

Setters require firmware readback and multi-step writes roll back on failure.
Profile changes return a typed, stable receipt containing the confirmed Acer
profile, NVIDIA offset state, P-state capability and, when exposed by NVML, the
enforced/maximum GPU power limit and clock-event reasons.

Acer and NVIDIA telemetry recover independently after startup races, driver or
module loss and resume, using bounded retries. While Acer telemetry reconnects,
the last displayed sample is retained only as stale context and the UI reports
that state until a fresh sample arrives. An NVML session loss does not stop the
Acer stream: NVIDIA-only values become unavailable with a diagnostic while
NVML is reopened. Telemetry failures do not overwrite the result of a verified
control operation.

## Prebuilt release / Předkompilované vydání

The recommended GitHub installation path is the
[`ubuntu-26.04-x86_64-installer` ZIP](https://github.com/fladirm/asense/releases/latest).
It contains both prebuilt programs (`asense` and the GUI-free `asensed`) plus
the complete transactional installer. No Rust toolchain is required. DKMS
still compiles the small `asense_rgb` kernel module locally, so matching kernel
headers and a C build toolchain remain necessary.

The supported build and prebuilt-release baseline is **Ubuntu 26.04 LTS on
x86_64 with glibc 2.43**. The installer is built and supported on that
baseline. Its dynamically linked binaries may run on an older distribution
when every required ABI and shared-library version is available, but such
combinations are not supported; build from source on the target distribution
instead.

Install the runtime and DKMS prerequisites:

```bash
sudo apt update
sudo apt install \
  build-essential dkms "linux-headers-$(uname -r)" kmod udev util-linux \
  python3 unzip mokutil desktop-file-utils \
  libgtk-3-0t64 libwebkit2gtk-4.1-0 libxdo3 libssl3t64
```

From the GitHub Release page download the installer ZIP and its matching
`.zip.sha256` file. Do **not** choose GitHub's automatically generated
`Source code` archive when you want the ready-to-install binaries. Verify and
extract the downloaded asset, then run the installer as the logged-in GNOME
user, not through `sudo`:

```bash
sha256sum --check asense-v0.1.0-ubuntu-26.04-x86_64-installer-*.zip.sha256
unzip asense-v0.1.0-ubuntu-26.04-x86_64-installer-*.zip
cd asense-v0.1.0-ubuntu-26.04-x86_64-installer-*/
./install.sh
```

The installer validates the archive manifest, builds the DKMS candidate before
replacing an existing installation, probes the WMI transport and smoke-tests
the versioned daemon protocol. A Secure Boot system may first require
[MOK enrollment](#secure-boot); after enrollment and reboot, run the installer
again.

## Build from source / Sestavení ze zdrojů

Source builds are supported on the same Ubuntu 26.04 release baseline. Building
on another distribution can adapt the userspace ABI to that system, but it does
not broaden the exact PHN16-72 hardware boundary or make that platform a tested
release target.

Ubuntu 26.04 dependencies:

```bash
sudo apt update
sudo apt install \
  build-essential pkg-config git dkms "linux-headers-$(uname -r)" libelf-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
  desktop-file-utils python3 mokutil
```

Install Rust with `rustup`; `rust-toolchain.toml` selects the release-pinned
toolchain. From a source checkout (or the checksummed source Release ZIP), run:

```bash
cargo test --locked
cargo build --release --locked --bin asensed --no-default-features
cargo build --release --locked --bin asense --features gui
./install.sh
```

The two explicit build commands are intentional: `asensed` is the privileged,
GUI-free service binary, while `asense` is the unprivileged desktop binary.

## Installation, upgrades and verification / Instalace, aktualizace a kontrola

Run the installer as the logged-in desktop user, **not** through `sudo`. It
requests elevation only for system operations:

```bash
./install.sh
```

An explicit release binary and desktop account can be supplied:

```bash
./install.sh /absolute/path/to/asense "$USER"
```

With an explicit GUI path, the installer selects an `asensed` binary from the
same directory. `ASENSE_DAEMON_BINARY=/absolute/path/to/asensed` can override
that pairing for development builds.

The installer first performs a non-destructive DKMS candidate build for the
running kernel and every kernel ABI on which the previous ASense version was
registered. It then backs up the active module, binary and systemd integration,
installs the exact candidate for the same ABI set and verifies the WMI
transport. Any later failure triggers rollback to the previous installation.
The protected control socket is assigned to exactly one selected desktop
account. Re-running a newer installer upgrades the existing installation in
place; do not uninstall the previous version first. The GNOME shortcut is
stored only for the selected account.
As a final transaction check, installation smoke-tests socket activation,
service reload, clean stop and socket reactivation.

After installation, launch ASense from the GNOME application menu or run:

```bash
asense
```

An installation that exits successfully has already verified the payload,
DKMS module, firmware probe and daemon handshake. These commands provide a
quick independent status check:

```bash
systemctl is-enabled asense.socket
systemctl is-active asense.socket
dkms status -m asense-rgb
modinfo -F version asense_rgb
cat /usr/libexec/asense/INSTALL-PROVENANCE.txt
```

### Predator hardware key / Hardwarová klávesa Predator

On the tested PHN16-72, the installer maps the PredatorSense key's `atkbd`
scan code to `XF86Launch1` and registers `/usr/bin/asense --toggle` through
GNOME Settings. The complete path is verified on the validation machine with
BIOS 1.18, including launches recorded by `gsd-media-keys`.

Automatic shortcut registration applies only to the selected, currently
logged-in **GNOME** account whose session bus is available during installation.
It is not system-wide for every local user. A different desktop environment,
another account, an install from SSH/TTY without the user's active session bus,
or an existing `XF86Launch1` conflict needs a desktop-specific shortcut. The
installer prints a warning instead of claiming success when it cannot register
the GNOME binding; the hardware mapping and the rest of the installation still
remain valid.

Uninstall with:

```bash
./uninstall.sh
```

The installer also retains a self-contained uninstall helper, its shared
support file and release provenance. If the extracted installer directory has
already been deleted, run:

```bash
/usr/libexec/asense/uninstall.sh
```

Uninstall requests and verifies firmware Auto fan control before removing the
service and module; if that confirmation fails, it reports the failure and
recommends a reboot rather than claiming success. It removes the daemon/socket,
DKMS registration and source, HWDB key mapping and desktop entry/icon. It also
removes the selected account's GNOME shortcut when that account's session bus
is available; otherwise it prints the manual-cleanup warning before removing
the retained payload and runtime package lock.

Other user-selected firmware settings (profile, NVIDIA offsets, RGB, charge
limit, USB charging, boot sound, LCD override and rear logo) deliberately
remain as configured. Uninstalling the application is not treated as a
firmware factory reset.

## AS IS / Bez záruky

ASense is provided **AS IS**, without warranty, fitness guarantee or an
obligation to provide individual support. Firmware and power controls can
change hardware behaviour; users remain responsible for backups, cooling and
reviewing changes before use.

ASense je poskytován **TAK, JAK JE**, bez záruky, garance vhodnosti nebo nároku
na individuální podporu. Firmware a výkonové ovladače mohou měnit chování
hardwaru; uživatel odpovídá za zálohy, chlazení a kontrolu změn před použitím.

## Release assets / Vydání

Prebuilt binaries belong to a GitHub **Release**, not to Git history. A pushed
`v*` tag runs the complete Ubuntu 26.04 release gate and publishes the installer
ZIP, its dedicated checksum, the `git archive` source ZIP, its dedicated
checksum and the combined checksum manifest. Maintainer instructions and
reproducibility details are in [`docs/RELEASING.md`](docs/RELEASING.md).

## Secure Boot

Check the state before installing:

```bash
mokutil --sb-state
```

Ubuntu DKMS signs the module during build. If `modprobe` reports
`Key was rejected by service`, enroll the distribution DKMS key (commonly
`/var/lib/shim-signed/mok/MOK.der`) and complete **Enroll MOK** after reboot:

```bash
sudo mokutil --import /var/lib/shim-signed/mok/MOK.der
modinfo -F signer asense_rgb
```

Key paths differ between distributions. Use the path printed by DKMS rather
than disabling Secure Boot to hide a signing failure.

## Firmware boundary

ASense binds only to the exact PHN16-72 DMI model. A BIOS update does not by
itself disable the application: required interfaces are capability-probed and
every mutation is verified by firmware readback with rollback on disagreement.

## Author and license / Autor a licence

**Fladirmacht** — <fladirmacht@gmail.com>

ASense, including the Rust application, packaging and kernel module, is
licensed **GPL-2.0-only**. The complete license text is included in
[`LICENSE`](LICENSE) and mirrored in [`kernel/LICENSE`](kernel/LICENSE) for the
standalone DKMS payload.
