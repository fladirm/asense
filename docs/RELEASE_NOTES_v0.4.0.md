# ASense v0.4.0

This release repairs fan discovery and simultaneous GUI/probe access, and
adds narrowly scoped Acer lighting compatibility from community reports.

- Kernel fan-control modes remain available when an initial mode read returns
  ENXIO or RPM sensors are missing. Manual mode still requires readable speed
  setpoints and valid CPU/GPU watchdog temperatures. Hardware is re-discovered
  before fan/profile changes so stale hwmon paths do not persist.
- The daemon serves up to eight authenticated clients while serializing
  firmware operations. Manual fan ownership, disconnect recovery and thermal
  watchdogs remain enforced; another client can explicitly override with Auto
  or Maximum. Passive probes work with the GUI open.
- Acer-owned profile labels distinguish Performance and Turbo. The documented
  PHN16S-71 kernel configuration remains opt-in, and NVIDIA tuning remains
  restricted to the reference PHN16-72.
- Exact Nitro AN17-51 static-lighting, AN14-41 boot-state and AN16-51 global
  color paths preserve canonical writes and independent readback checks.
  AN16-51 exposes one logical global-color zone rather than unconfirmed
  independent zones.
- Passive probe schema 4 adds descriptor-only Darfon USB discovery and
  accurate generic profile interpretation. Original schema-3 reports remain
  readable. Runtime control protocol stays at `HELLO 2`.
- Unknown adapter, battery and firmware-control readings remain unavailable
  rather than appearing as false, zero or actionable defaults. Uninstall
  distinguishes verified Auto recovery from an absent fan-control backend.

The firmware mode acknowledgement is separate from physical fan speed. PH18-73
profile `0x02` and ENEK target `65` remain unmapped; Darfon active lighting,
undecoded APGE payloads and NixOS packaging are not claimed as working. See
[capability evidence and limits](SUPPORTED_HARDWARE.md).

Upgrade the GUI, daemon and DKMS source together. APT/DEB installations must
use their package upgrade path; do not copy standalone binaries over files
owned by dpkg. First AUR publication is pending account/key authorization;
check the public package page before using AUR clone commands.

Thanks to Philippe-Augusto, guplaterbg-prog and the issue reporters for exact
machine evidence and proposed fixes. Readback, native C policy tests and
module compilation do not replace reporter verification on each BIOS/model.
