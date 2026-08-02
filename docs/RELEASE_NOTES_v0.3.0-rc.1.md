## ASense v0.3.0-rc.1 — diagnostic compatibility candidate

This is a diagnostic release candidate for evidence collection on recent Acer
Predator and Nitro laptops. It is not the stable v0.3.0 release and is not
published to the stable Ubuntu PPA or AUR channel.

### Probe schema 3

- `asense probe > asense-probe.json` creates the authoritative, bounded
  schema-3 compatibility report.
- `asense probe --summary` prints a compact human-readable view of a fresh
  passive capture.
- Missing interfaces, unknown firmware values and read/decode/protocol errors
  remain distinct instead of collapsing into a false supported/unsupported
  flag.
- The report records power-source state, driver ownership, profile evidence,
  independent fan/RPM evidence, per-field platform reads, zoned-WMI transport
  geometry and exact allow-listed HID descriptor/A1 evidence.

The default path negotiates `HELLO 2` and requests only `DIAG PASSIVE`. It does
not call general `CAPS`, send an ENEK A2 selector, change lighting, write a
profile/fan/platform value, query NVIDIA hardware or upload the report.

### Privacy and support evidence

The report excludes serials, UUIDs, hostname, user, network, boot and storage
identity, HID serial/physical paths, journals, raw ACPI tables, absolute device
paths and the process environment. Review the JSON before sharing it. The full
allow-list and workflow are in `docs/PROBE_PRIVACY.md`.

Compatibility reports must capture observed behavior and the probe under the
same ASense build, boot, loaded-module state and relevant AC/battery and control
state. The GitHub issue forms now request that synchronization explicitly.

### Build authority

Source verification and release packaging use the Rust toolchain already
installed in the build environment and record its exact executable paths and
versions. ASense no longer contains a repository toolchain pin and its scripts
do not install, switch or downgrade Rust.

### Hardware-support boundary

This candidate does not assign a semantic meaning to raw Gaming-WMI profile
value `02`, does not expose EC-HID power control and does not infer a physical
lighting target from an ENEK numeric ID. New controls remain gated on exact
transport fixtures and independent physical evidence. Predator PHN16-72
remains the reference-tested machine; other models remain capability-specific
community evidence until their receipts are complete.
