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
systemd units, the Predator-key HWDB entry and the DKMS module against every
installed header tree.

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
- one combined `SHA256SUMS.txt` manifest.

The installer ZIP contains the GUI, GUI-free daemon, installer/uninstaller,
DKMS source, systemd/socket/HWDB integration, full license, screenshots and a
payload checksum/provenance manifest. Entry timestamps are normalized to the
release commit. Local HOME and workspace paths are remapped out of the release
binaries and verified absent before packaging. Packaging refuses tracked,
staged or untracked worktree changes.

## GitHub Release

Review the clean commit, then create and push the matching annotated tag:

```bash
git tag -a v0.1.0 -m "ASense v0.1.0"
git push origin main v0.1.0
```

`.github/workflows/release.yml` independently runs the gate on Ubuntu 26.04,
requires the tag to match `Cargo.toml`, builds the assets from a clean target
directory and attaches them to the GitHub Release. No binary ZIP is committed
to the repository.

Before considering the release complete, download its installer ZIP and
checksum from GitHub, verify them on the target PHN16-72, install over the
previous version and confirm the provenance file identifies the tagged commit.
