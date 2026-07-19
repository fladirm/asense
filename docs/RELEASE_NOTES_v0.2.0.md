## ASense v0.2.0

ASense is a native Linux control center for Acer Predator and Nitro laptops.

v0.2 replaces the original PHN16-72-only architecture with runtime capability
discovery. Controls appear only when the corresponding Linux, Acer WMI or HID
interface is present and responds correctly.

### Highlights

- dynamic Linux platform profiles;
- kernel PWM → Acer Gaming-WMI fan fallback;
- Auto, Manual and Maximum fan modes with verified readback;
- optional RPM and temperature channels;
- 1–4-zone Acer WMI keyboard lighting;
- ENEK5130 keyboard and cover-logo lighting;
- independent Battery, APGE and Gaming-WMI capabilities;
- observer-neutral NVIDIA telemetry that does not query a suspended dGPU;
- sanitized `asense probe` compatibility report;
- protected typed GUI ↔ daemon protocol;
- transactional installer, DKMS rollback and checksummed release assets.

### Hardware status

- Reference tested: Acer Predator PHN16-72
- Other systems: features are detected independently at runtime
- NVIDIA PHN16-72 +100/+200 MHz preset remains exact-model and exact-GPU gated

ASense does not claim that every detected Acer model has been physically tested.

### Supported release baseline

- Ubuntu 26.04 LTS
- x86_64
- GNOME
- GPL-2.0-only

### Installation

Download the installer ZIP and its matching checksum file from this release.
Do not download GitHub's automatically generated source archive when you want
the prebuilt application.

See the README for prerequisites, Secure Boot instructions and installation.
