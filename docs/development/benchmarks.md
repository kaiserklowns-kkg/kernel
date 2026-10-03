# Benchmarks

Master spec §51: measure, do not assume. Each entry records what was
measured, how, on what, and the result. Numbers from QEMU TCG (pure
emulation) are regression baselines only, not performance claims.

Run the measurements with `cargo xtask smoke --release` (they are printed in
the serial log).

| Date | Commit | What | Setup | Result |
|---|---|---|---|---|
| 2026-10-03 | ADR-0013 | IPC empty call/reply round trip, 10 000 iterations, kernel threads | QEMU 11.0 TCG, q35, release | ~9 000 cycles |
| 2026-10-03 | ADR-0013 | same | QEMU TCG, debug | ~390 000 cycles |
| 2026-10-03 | ADR-0012 | APIC timer frequency calibrated against PIT | QEMU TCG | ~64 MHz bus/16 |
| 2026-10-04 | ADR-0040 | `cp` 1 MiB, virtio disk → NVMe (`/nvme`), including sync | QEMU TCG, release, host loaded by other QEMU runs | 120 ms |
| 2026-10-04 | ADR-0039 | `cp` 1 MiB, virtio disk → Oceans volume on a USB stick, including sync | same | 200–310 ms |
| 2026-10-03 | ADR-0039 | `cp` 1 MiB, virtio disk → FAT16 USB stick, including sync | QEMU TCG, release | 560 ms |
| 2026-10-04 | ADR-0028 | `fetch` 1 MiB over HTTP from the host → system disk | QEMU TCG, release, user-mode network | 260 ms |
| 2026-10-04 | ADR-0028 | `fetch` 1 MiB over HTTP → Oceans USB stick / FAT16 USB stick | same | 340 ms / 1 720 ms |
| 2026-10-04 | ADR-0031 | `fetch` 256 KiB over HTTPS (TLS 1.3) → system disk | same | 180 ms |

Observations: writing FAT through `fetch` (16 KiB writes, each extending
the cluster chain with ordered barriers, ADR-0037) is about five times
slower than one `cp` of the same file (128 KiB writes); larger write
batches would help FAT most.

To add: boot time to `OCEANS KERNEL ONLINE`, thread spawn cost, context
switch cost, frame allocation and heap allocation cost; KVM and Tier 1
hardware columns.
