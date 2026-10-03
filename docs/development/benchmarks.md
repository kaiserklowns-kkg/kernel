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

To add: boot time to `OCEANS KERNEL ONLINE`, thread spawn cost, context
switch cost, frame allocation and heap allocation cost; KVM and Tier 1
hardware columns.
