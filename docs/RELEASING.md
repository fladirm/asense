# ASense release procedure

This document is for maintainers working from a complete source checkout. End
users should follow the prebuilt installation path in the root README.

## Release baseline

- Ubuntu 26.04 LTS, x86_64;
- Rust 1.96.0 as pinned by `rust-toolchain.toml`;
- matching headers for every locally installed kernel checked by the gate;
- a completely clean Git worktree for packaging.

Run the full release gate before committing:

```bash
scripts/verify-release.sh
```

It checks Rust formatting and tests, the kernel protocol integration test,
strict Clippy for the full application and the GUI-free daemon, shell syntax,
systemd units, the exact Predator-key HWDB entry and the DKMS module against
every installed header tree. Before release, also confirm that protocol 2
negotiation and `CAPS` fixtures, dynamic profile/hwmon paths, zoned WMI,
ENEK5130 discovery and the sanitized probe privacy tests are included in the
green test run.

Commit the intended state and run the gate once more. Local assets can then be
created from the clean commit with:

```bash
scripts/package-release.sh
```

The script builds both Rust binaries in a fresh Cargo target directory and
creates:

- `asense-v<VERSION>-ubuntu-26.04-x86_64-installer-<COMMIT>.zip`;
- a matching installer `.zip.sha256` file;
- `asense-v<VERSION>-source-<COMMIT>.zip`, produced by `git archive`;
- a matching source `.zip.sha256` file;
- one combined `SHA256SUMS` manifest.

Those five files are the minimum custom assets on the GitHub Release. GitHub's
automatic “Source code” links are additional and do not replace the custom
source ZIP or installer.

The installer ZIP contains the GUI, GUI-free daemon, installer/uninstaller,
DKMS source, systemd/socket/HWDB integration, full license,
screenshots and a payload checksum/provenance manifest. Entry timestamps are
normalized to the release commit. Local HOME and workspace paths are remapped
out of the release binaries and verified absent before packaging. Packaging
refuses tracked, staged or untracked worktree changes.

## GitHub release candidate first

Push the reviewed release commit, but do not create the final tag yet. Run the
`Release` workflow manually against `main`. Its `workflow_dispatch` path runs
the complete Ubuntu 26.04 gate and publishes the exact installer, source and
checksum files as a short-lived Actions artifact; it does not create a GitHub
Release.

Download that artifact, then verify all checksums:

```bash
sha256sum --check asense-v0.2.1-ubuntu-26.04-x86_64-installer-*.zip.sha256
sha256sum --check asense-v0.2.1-source-*.zip.sha256
sha256sum --check SHA256SUMS
unzip -t asense-v0.2.1-ubuntu-26.04-x86_64-installer-*.zip
unzip -t asense-v0.2.1-source-*.zip
```

Install the downloaded installer on the reference PHN16-72 as the logged-in
desktop user and exercise profiles, Auto/Manual/Maximum, RGB and rear logo,
Battery/APGE, suspend/resume and protocol 2/CAPS. With no external dGPU user,
leave the GUI open for at least 65 seconds and confirm that
`power/runtime_status` remains `suspended`; opening ASense must not create an
NVML session or wake the GPU.

Only after that exact asset passes, create and push the annotated tag:

```bash
version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n1)"
test -n "$version"
git tag -a "v$version" -m "ASense v$version"
git push origin "v$version"
```

The tag path of `.github/workflows/release.yml` repeats the complete gate,
requires the tag to match `Cargo.toml`, rebuilds from a clean target directory
and publishes the final GitHub Release. No binary ZIP is committed to the
repository. The hosted Ubuntu 26.04 image is currently a public-preview runner,
so its result complements rather than replaces the clean local and physical RC
gates. Keep `uname -r`, `getconf GNU_LIBC_VERSION`, `rustc -V`, `cargo -V`, the
workflow URL and downloaded SHA-256 results with the RC evidence.

Release notes must use the hardware terms from the README accurately:
`Reference tested`, `Kernel backed`, `Detected` and `Community confirmed`.
Do not describe a model as physically tested merely because a standard kernel
node or known controller passed discovery.

Before considering the release complete:

1. download the final tagged installer ZIP and checksum from GitHub;
2. verify that it has the accepted commit suffix and matching checksum;
3. confirm protocol 2, `CAPS`, profiles, fan sessions, lighting, Battery/APGE,
   exact NVIDIA and hotkey behaviour, suspend/resume and uninstall;
4. run `asense probe`, review the JSON for the documented privacy boundary and
   confirm the client sends only `HELLO`/`CAPS`, never a mutation command;
5. confirm the provenance file identifies the tagged commit.

Community reports on other Acer systems are valuable fixtures and may justify
the `Community confirmed` label for the reported capability. They do not
replace the PHN16-72 reference regression run.
