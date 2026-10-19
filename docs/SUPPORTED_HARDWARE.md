# Capability evidence and limits

ASense discovers each capability separately. A model name alone never grants
arbitrary firmware access. The PHN16-72 remains the reference platform; the
following reports extend particular capabilities, not every feature on every
BIOS revision. New v0.4.0 code paths require fresh reports from those machines.

| Model / report | Implemented path | Evidence and remaining limit |
| --- | --- | --- |
| PHN16-72 | Kernel fan modes remain discoverable when the initial mode read returns ENXIO; control re-discovers changed hwmon paths. GUI and passive probe can connect together. | Local reproduction. Missing RPM or temperatures never disables Auto/Maximum. Manual still requires both watchdog temperatures and readable setpoints. Physical maximum speed is not established by a mode readback. |
| PHN16S-71, FTYPE_ARX, BIOS V1.26 | Kernel `acer_wmi` with opt-in `predator_v4=1`; Turbo label follows the identified handler. | [Issue #1](https://github.com/fladirm/asense/issues/1), [PR #5](https://github.com/fladirm/asense/pull/5). Reporter confirmed RPM, temperatures, Manual and profiles with the signed Ubuntu driver. No alias for Gaming-WMI raw `02` is added. |
| AN17-51 | Exact DMI quirk for the four-zone engine and bounded stale static fields. | [PR #11](https://github.com/fladirm/asense/pull/11). Dynamic records retain strict validation. Effect color shadows are tolerated only during a transaction that separately verifies every active zone's color. |
| AN14-41 | Exact documented boot-wave records can be read; subsequent writes and rollback use canonical validated records. | [Issues #8](https://github.com/fladirm/asense/issues/8) and [#9](https://github.com/fladirm/asense/issues/9). Four independent colors were reported with a local patch. No thermal fallback or unsupported claim of audible Maximum is added. |
| AN16-51 | One logical global static-color zone uses and verifies effect-level RGB, including power and resume. | [Issue #10](https://github.com/fladirm/asense/issues/10). Independent physical zone writes failed in the report and are not advertised. RPM/temperature availability depends on the kernel configuration. Invalid state left by another controller is not silently accepted. |
| PH18-73, BIOS V1.54 | Mode-only Auto/Maximum and passive diagnostics remain usable independently of profile decoding. | [Issue #2](https://github.com/fladirm/asense/issues/2). Raw profile `02` stays unknown. ENEK target `65` has no universal semantic mapping; its reported 12-byte A3 layout needs a versioned decoder and physical correlation before writes. |
| PH16-72 with Darfon `0003:0d62:ba51` | Passive USB descriptor/hash/usage inventory only. | [Issue #3](https://github.com/fladirm/asense/issues/3). This USB controller receives no ENEK feature request. It is distinct from the WMI rear-logo control. Active Darfon lighting needs independent packet and readback evidence. |
| PHN16-72, Fedora report | Concurrent daemon clients fix the independently reproduced GUI/probe timeout. | [Issue #6](https://github.com/fladirm/asense/issues/6). APGE timeout/EPROTO is separate; its decoder remains strict until an exact reproducible payload is available. |
| PT315-52 | Generic fan discovery and actionable backend errors. | [Issue #4](https://github.com/fladirm/asense/issues/4). The local ENXIO cause has not been reproduced on this exact model; collect a new synchronized report before claiming closure. |
| ANV15-52, BIOS V1.53 | Normal independent capability discovery and passive probe. | [Issue #7](https://github.com/fladirm/asense/issues/7). No complete report yet; no model-specific firmware writes are enabled. |

## Profiles

The named `acer-wmi` kernel handler and ASense's Gaming-WMI ABI use
`balanced-performance` for Performance and `performance` for Turbo. The latter
ABI keeps raw `00 Quiet`, `01 Balanced`, `04 Performance`, `05 Turbo`, `06 Eco`.
The reference PHN16-72 retains its established interpretation. An unidentified
legacy or other kernel handler uses the generic Performance label instead of
assuming Turbo. NVIDIA offset reconciliation remains restricted to the
reference model. Raw `02` never becomes a selectable profile by inference.

## Report a capability

Capture `asense probe > asense-probe.json` and `asense probe --summary` with the
same build, loaded modules, power source and observed failure. The GUI may
remain open. Include the exact model, BIOS, kernel and distribution, and state
whether a fan/lighting effect was physically observed or only acknowledged by
firmware. Review the JSON before sharing it. Raw EC dumps, personal logs and
unbounded hardware executors are not needed.

The default probe is passive. USB inventory does not imply permission to send
the ENEK protocol or another controller's packet layout. Unknown transports
can be added as bounded diagnostic candidates before active support is ready.

## Distribution scope

Ubuntu/Debian packages and the standalone Ubuntu build share the same source.
Arch uses the source-package templates and its distribution toolchain. The
first public AUR push still requires an authorized AUR account/key.

[NixOS / Helios 500 support (#13)](https://github.com/fladirm/asense/issues/13)
is not yet a tested packaging target. A native package also needs a NixOS
service, restricted desktop-user socket, optional module built against the
selected kernel and service lifecycle checks; a development shell alone does
not provide installation support. No untested Nix expression is shipped as a
working package.
