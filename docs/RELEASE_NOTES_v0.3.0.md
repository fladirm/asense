## ASense v0.3.0 — capability evidence and localization

ASense v0.3.0 expands compatibility evidence without guessing unsupported
firmware behavior. Predator PHN16-72 remains the fully reference-tested
platform; controls on other Acer laptops continue to be exposed capability by
capability from the Linux, Acer WMI or supported HID interfaces actually
present.

### Passive compatibility probe

- `asense probe > asense-probe.json` writes the authoritative bounded,
  read-only schema-3 compatibility report.
- `asense probe --summary` prints a compact view of a fresh passive capture.
- Missing interfaces, unknown values, read failures and decode failures remain
  distinct instead of becoming false support claims.
- The report records synchronized power-source, driver ownership, profile,
  fan/RPM, platform-field, zoned-WMI and allow-listed HID evidence.
- The default path sends only protocol negotiation and `DIAG PASSIVE`; it does
  not call the older general capability discovery path, select an ENEK target,
  change hardware or upload data.

The report excludes serials, UUIDs, hostname, user and network identity, boot
and storage IDs, HID serials/physical paths, journals, raw ACPI tables,
absolute device paths and the process environment. Users can inspect the JSON
before attaching it to a compatibility report.

### Complete UI catalogs

- The desktop UI and embedded help now use one typed catalog with stable
  message IDs.
- English, Czech and Simplified Chinese are complete selectable catalogs; no
  enabled locale falls back to English.
- Locale preference is stored in the user's configuration directory and has
  no privileged daemon or hardware-control path.
- Simplified Chinese received two complete internal consistency reviews. It is
  not represented as native-reviewed; community corrections remain welcome
  and will be versioned.

### Packaging and safety boundary

- Release, DKMS, Debian/PPA and Arch/AUR metadata are checked against the Cargo
  package version.
- Standalone and Debian ownership guards remain fail-closed, and the Arch
  source package requires explicit selection of the desktop account allowed
  to use the private control socket.
- Release builds use the system Rust toolchain already supplied by the build
  environment; ASense does not install, pin, switch or downgrade Rust.
- Raw Gaming-WMI profile value `02`, EC-HID power control, ENEK target meaning
  and platform read-error semantics remain diagnostic evidence only unless a
  synchronized physical receipt proves the corresponding behavior.
