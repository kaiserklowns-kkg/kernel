# Signing keys

`oceans-dev.seed` is the **development** package signing key (ADR-0046):
the 32-byte Ed25519 seed, in hex. It signs the example packages that
`cargo xtask` builds into `build/packages/`, and every image this
repository builds trusts its public key, under the publisher name
`Oceans Examples` (the image's `trust.keys`).

It is public, since it is in this repository: **anyone can sign packages
with it**. It exists so that a fresh checkout can build, install and test
apps without any secret. A release image must not trust it.

## The release key (ADR-0072)

Release images trust only the **release key**, for apps and for system
updates. It is never in this repository. Whoever publishes releases makes
it once, keeps it secret, and keeps an offline backup:

```bash
cargo run -p oceans-dev -- keygen "Oceans" --out ~/keys/oceans-release.key
```

`keygen` creates the folder, and reads a leading `~` as the home folder
itself (PowerShell passes `~` through unexpanded).

`cargo xtask release` builds a release with it. In a POSIX shell:

```bash
OCEANS_RELEASE_KEY=~/keys/oceans-release.key cargo xtask release
```

In PowerShell:

```powershell
$env:OCEANS_RELEASE_KEY = "$HOME\keys\oceans-release.key"; cargo xtask release
```

On Windows without `mkfs.fat` and `mtools` on the PATH, also set
`OCEANS_FAT_TOOLS_WSL` to the WSL folder that holds them (`usr/sbin/mkfs.fat`,
`usr/bin/mcopy`).

It refuses the key if the file is inside the repository, if it is this
development key, if it is not the key published in `oceans-release.pub`,
or if the working tree has changes. Losing the release key ends updates
for that release line. Leaking it lets anyone sign updates.

## Secure Boot keys (ADR-0091)

UEFI firmware checks RSA signatures only, so Secure Boot has keys of its
own: an RSA-2048 key and its self-signed certificate, which users enrol
in their firmware's `db`.

- `oceans-dev-secure-boot.key` and `.cer` are the **development** Secure
  Boot key: public, like the development seed. `cargo xtask
  usb-secure-boot` and `cargo xtask smoke-secure-boot` use it. Never enrol
  it on a machine you care about: anyone can sign with it.
- The **release** Secure Boot key is never in this repository. Make it
  once, keep it secret with an offline backup, and commit its certificate
  as `oceans-secure-boot.cer` (what users enrol, and what releases are
  checked against):

```bash
cargo run -p oceans-dev -- secure-boot-key "Oceans" --out ~/keys/oceans-secure-boot.key
```

`cargo xtask release` then also builds the image signed for Secure Boot
when `OCEANS_SECURE_BOOT_KEY` names the key file. It refuses a key inside
the repository, the development key, or a key whose certificate is not
`oceans-secure-boot.cer`.

## The published release key (ADR-0075)

`oceans-release.pub` is the release key's public half. Releases sign their
`SHA256SUMS` with the private half (`SHA256SUMS.sig`), and anyone checks a
download against this file, never against a key from the download:

```bash
cargo run -p oceans-dev -- verify DOWNLOADS --key KEY-HEX
```
