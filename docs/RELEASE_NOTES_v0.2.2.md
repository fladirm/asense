## ASense v0.2.2 — telemetry stability hotfix

This stabilization release fixes a long-running telemetry failure that could
freeze or grey out the interface on systems with active NVIDIA telemetry. It
contains no new hardware features and is recommended for all v0.2.1 users.

### Fixed

- NVIDIA telemetry now keeps one NVML session for each live dGPU epoch instead
  of repeatedly running `nvmlInit_v2()` and `nvmlShutdown()` at the 1 Hz sample
  rate. This prevents linear file-descriptor exhaustion on affected NVIDIA
  drivers while passive PCI runtime-power checks still avoid waking a
  suspended hybrid dGPU.
- A failed Refresh keeps the last verified capability snapshot and controls
  available, while reporting the transient error.
- Control completions are retained in order and the worker survives a stale or
  poisoned completion slot instead of silently stopping.
- Native resize correction now has bounded acknowledgement and drag-state
  recovery, preventing an interrupted resize from remaining in flight.
- Privileged hardware mutations use a bounded lock acquisition instead of
  waiting indefinitely behind another writer.
- USB-off charging now decodes the firmware's separate enabled flag and stored
  threshold. Turning the feature off therefore remains readable after 20% or
  30% was selected, and disabling it preserves the last selected threshold.
- Post-resume reconciliation now follows systemd's actual sleep-hook argument
  order, so fan safety, lighting and optional NVIDIA state are restored after
  resume.
- Switching from the Debian package to the standalone installer now requires
  a package purge, preventing a delayed Debian purge from removing standalone
  state.
- Standalone uninstall no longer strands root-owned services or DKMS state if
  the desktop account selected during installation has since been deleted.
- Standalone upgrades clean stale DKMS records for removed kernels instead of
  failing because those kernels no longer have headers installed.

### Supported release baseline

- Ubuntu 26.04 LTS
- x86_64
- GNOME
- GPL-2.0-only

Ubuntu users can upgrade through `ppa:fladirmacht/asense`:

```bash
sudo apt update
sudo apt install asense
```

The standalone installer ZIP and its matching checksum remain available from
the GitHub Release page.
