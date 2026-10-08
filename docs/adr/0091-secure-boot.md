# ADR-0091: Secure Boot

- Status: Accepted
- Date: 2026-10-07
- Depends on: ADR-0003 (Limine), ADR-0068 (the USB image), ADR-0071
  (system updates), ADR-0072 and ADR-0075 (release keys)
- Part of Phase 10 (Alpha: security). Updates on a Secure Boot image
  follow in ADR-0092, which also changes the image's layout: a selector
  and a partition per slot.

## Context

Oceans asked users to switch Secure Boot off (ADR-0068, ADR-0072). With it
off, whoever can write to the boot stick can replace the bootloader, the
kernel or the boot archive, and the machine runs it. Release keys
(ADR-0072) protect updates and apps, but not the first steps of the boot.

Secure Boot closes that gap. Firmware runs only executables signed by a
key in its `db`, and the bootloader must then check everything it loads.

- **What firmware checks:** an Authenticode signature (PKCS#7
  `SignedData`, RSA) on a PE32+ executable. Ed25519, the key type Oceans
  uses everywhere else, is not accepted.
- **What Limine offers** (9.x, Oceans' bootloader):
  - a BLAKE2B hash after any path in its configuration (`path#hash`),
    checked when the file is loaded;
  - the configuration's own hash written into its executable
    (`enroll-config`). With that, the editor is off, and a different
    configuration makes Limine stop with "CHECKSUM MISMATCH FOR CONFIG
    FILE".
- **Limine's limit:** it finds its configuration at fixed paths on a
  partition, not next to the executable. So one partition holds one
  enrolled configuration.

## Decision

### The key

- **The Oceans Secure Boot key** is RSA-2048, with a self-signed X.509
  certificate:
  - subject Oceans;
  - key usage digital signature, extended key usage code signing;
  - valid 2025–2055.
- `oceans secure-boot-key NAME --out FILE` makes one. It writes the key
  file and `FILE.cer`, the certificate users enrol in `db`.
- **The development key** (`tools/keys/oceans-dev-secure-boot.key` and
  `.cer`) is public, like the development seed. It lets a fresh checkout
  build and test signed images.
- **A release key** lives outside the repository. `cargo xtask release`
  uses it when `OCEANS_SECURE_BOOT_KEY` names it, and refuses:
  - a key inside the repository;
  - the development key;
  - a key whose certificate is not the one published in
    `tools/keys/oceans-secure-boot.cer` (committed once, like
    `oceans-release.pub`).

### Signing (in `oceans-dev`, used by xtask)

- **Authenticode** for PE32+:
  - the image is padded to 8 bytes;
  - it is hashed with SHA-256: the headers without the checksum and the
    certificate table entry, the sections in file order, then the rest
    without the certificate table;
  - an `SpcIndirectDataContent` names that hash;
  - a PKCS#7 `SignedData` carries the certificate and a signer, whose
    signed attributes (content type, message digest, `SpcSpOpusInfo`) are
    signed with RSA PKCS#1 v1.5 and SHA-256;
  - it is appended as a `WIN_CERTIFICATE`, and the certificate table entry
    points at it.
- The DER is written by hand (a few fixed structures) on RustCrypto's
  `rsa` and `sha2`, the first RSA in the project.

### A Secure Boot image

`cargo xtask usb-secure-boot` (development key), or a release with
`OCEANS_SECURE_BOOT_KEY`, builds it:

1. Every `path:` and `module_path:` in Limine's configuration gets its
   file's BLAKE2B, and `serial: yes` is added. Both copies of the
   configuration are made the same text.
2. That configuration's BLAKE2B is written into `BOOTX64.EFI`, after
   Limine's marker, as `enroll-config` does, without needing Limine's
   host tool.
3. `BOOTX64.EFI` is signed.
4. `/boot/secure-boot` (a marker naming the certificate) and
   `/oceans-secure-boot.cer` (the certificate, to enrol from the
   firmware's own setup screens) go on the partition.

The chain of checks:
- firmware checks Limine's signature;
- Limine checks the configuration against its enrolled hash;
- the configuration's hashes check the kernel and the boot archive;
- the boot archive is the system (ADR-0025).

### The kernel and hashed modules

The test found a kernel bug. Limine keeps a module it checked against a
hash in the buffer it read it into, in **bootloader-reclaimable** memory,
not in kernel+modules memory as before. The kernel assumed the latter:
- it mapped the module again over the direct map, which already covers
  reclaimable memory with large pages, and panicked
  (`HugePageConflict`);
- it would then have handed the initrd's pages to the frame allocator
  when reclaiming bootloader memory.

Now the kernel maps only the module pages the direct map does not cover,
and reclaims bootloader memory except the modules.

### Updates

`update apply` refuses on a partition with `/boot/secure-boot`: writing a
new configuration there would stop the machine from starting. Such a
machine is updated by writing the new release's Secure Boot image. Signed
in-place updates, with each slot on a partition of its own carrying its
own signed Limine and configuration, are ADR-0092.

### The test: `cargo xtask smoke-secure-boot`

It runs on QEMU's Secure Boot firmware (OVMF built with SMM, with a
secure pflash and a fresh variable store), four boots:
1. **Enrol:** `oceans-enroll`, a small UEFI program (`tools/enroll`, built
   for `x86_64-unknown-uefi`), finds the firmware in Setup Mode. It writes
   the development certificate as `db`, `KEK` and finally `PK`, which
   switches the firmware to User Mode, then powers off.
   - The three are time-based authenticated variables, signed with the
     development key on the host (its `build.rs`, through
     `oceans-dev::secure_boot`).
   - QEMU's OVMF wants even the first `PK` self-signed (edk2's
     `PcdRequireSelfSignedPk`): an unsigned one is refused as a security
     violation. So the firmware's own PKCS#7 check passes on this code's
     DER before any image is signed.
2. **The signed image boots** to the shell, and `update apply` refuses
   with the Secure Boot message.
3. **An unsigned Limine must not start** the kernel: the firmware refuses
   it.
4. **The signed Limine with one byte of the kernel changed must not start
   it:** Limine's hash check refuses it.

With the FAT tools (`mkfs.fat`, `mcopy`; CI has them) each boot uses the
real USB image as a USB stick, so the system has it at `/usb` and the
update refusal is checked. Without them the folder is served by QEMU as an
IDE disk: the whole boot chain is still checked, and the test says that
the refusal was not. QEMU's folder emulation does not boot over
usb-storage. CI runs the test after the other smoke tests.

## Consequences

- An Oceans machine can boot with Secure Boot on, and then nothing before
  the kernel's own checks can be swapped unnoticed: bootloader,
  configuration, kernel, boot archive.
- The development key makes this testable everywhere, in CI included.
- **Limits:**
  - **Users enrol the certificate** in their firmware (`db`, from the
    setup screens, with `oceans-secure-boot.cer` from the stick) before
    turning Secure Boot on. Machines whose firmware does not allow custom
    keys cannot use it. Microsoft's CA (through shim) was set aside: it
    would mean shipping another distribution's shim and trusting its
    keys.
  - **No updates in place on a Secure Boot image** until ADR-0092.
  - **No revocation:** a leaked Secure Boot key has to be removed from
    `db` by hand, or its hash added to `dbx`.
  - **The kernel does not know** whether Secure Boot is on: reading
    `SecureBoot` needs UEFI runtime services, which the kernel does not
    call.
  - **No rollback protection:** an older signed image still boots.

## Alternatives considered

- **shim and MOK:** boots where only Microsoft's keys are present. But it
  ships and trusts another vendor's signed shim, and enrolling a MOK is
  as manual as enrolling in `db`.
- **`sbsign` and `sbverify` (sbsigntool):** less code here, but an outside
  tool for every release, run through WSL on Windows. The signing code is
  small, tested, and checked end to end by firmware in CI.
- **Limine's `limine enroll-config`:** its host tool would have to be
  built (CI fetches Limine's binaries only). The operation is a 128-byte
  copy after a marker.

## Checklist (master spec §48)

- **Purpose:** a boot chain that firmware and Limine verify.
- **Architecture:**
  - `oceans-dev::secure_boot` (the key, the certificate, Authenticode);
  - `xtask::secure_boot` (hashed configuration, enrolment, the image, the
    test);
  - `tools/enroll` (Setup Mode enrolment for the test);
  - `update`'s refusal;
  - the kernel's handling of modules in reclaimable memory.
- **API:**
  - `oceans secure-boot-key` and `oceans secure-boot-sign` (any UEFI
    executable);
  - `cargo xtask usb-secure-boot | smoke-secure-boot`;
  - `OCEANS_SECURE_BOOT_KEY` for `release`.
- **Dependencies:** `rsa` 0.9 and `blake2` 0.10 (RustCrypto, MIT or
  Apache-2.0); `sha2` gains its `oid` feature.
- **Security:**
  - signatures firmware verifies;
  - hashes Limine verifies;
  - the release key kept out of the repository and matched against the
    published certificate;
  - the development key marked public.
- **Testing:**
  - unit: DER encodings, the key file round trip and the certificate's
    self-signature, signing and verifying a PE image (and its failures),
    hashing the configuration, enrolment, signing the real Limine;
  - firmware: the four boots of `smoke-secure-boot`.
- **Failure behaviour:**
  - firmware refuses an unsigned or changed Limine;
  - Limine stops on a changed configuration or file;
  - `update` refuses rather than make the machine unbootable.
