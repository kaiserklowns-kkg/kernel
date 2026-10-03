//! Kernel randomness (ADR-0026): what `RANDOM` hands out.
//!
//! Sources, all hashed together into a ChaCha20 generator (`oceans-random`):
//! - the CPU's random-number instructions (RDSEED, else RDRAND), when
//!   present;
//! - timing jitter measured at boot;
//! - the timestamp counter at every timer and device interrupt, kept in a
//!   small ring and mixed in at each request.
//!
//! No source is trusted alone: hashing means one good source is enough,
//! and a bad one cannot cancel the others. Without RDRAND (e.g. QEMU's
//! default CPU) the jitter and interrupt timing still seed the generator;
//! the boot log says which sources were found.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use oceans_random::{Rng, blake2s};
use spin::Mutex;

use crate::{arch, klog};

const SAMPLES: usize = 64;
const BOOT_JITTER_SAMPLES: usize = 1024;

static RNG: Mutex<Rng> = Mutex::new(Rng::new());
static RING: [AtomicU64; SAMPLES] = [const { AtomicU64::new(0) }; SAMPLES];
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Records interrupt timing (interrupt context; cheap).
pub fn sample() {
    let index = NEXT.fetch_add(1, Ordering::Relaxed) % SAMPLES;
    let previous = RING[index].load(Ordering::Relaxed);
    RING[index].store(previous.rotate_left(17) ^ arch::cycles(), Ordering::Relaxed);
}

/// Timing jitter of a small, memory-dependent loop.
fn boot_jitter() -> [u8; 32] {
    let mut deltas = [0u8; 2 * BOOT_JITTER_SAMPLES];
    let mut scratch = [0u64; 64];
    let mut index = 0usize;
    for pair in deltas.as_chunks_mut::<2>().0 {
        let start = arch::cycles();
        for _ in 0..16 {
            index = (index + scratch[index % 64] as usize + 7) % 64;
            scratch[index] = scratch[index].wrapping_add(start);
        }
        let delta = arch::cycles().wrapping_sub(start);
        pair.copy_from_slice(&(delta as u16).to_le_bytes());
    }
    blake2s(&[&deltas, &index.to_le_bytes()])
}

/// Seeds the generator at boot.
pub fn init() {
    let mut hardware = [0u8; 32];
    let mut found = 0;
    for chunk in hardware.as_chunks_mut::<8>().0 {
        if let Some(value) = arch::hardware_random() {
            chunk.copy_from_slice(&value.to_le_bytes());
            found += 1;
        }
    }
    let jitter = boot_jitter();
    let ticks = crate::time::ticks().to_le_bytes();
    arch::without_interrupts(|| RNG.lock().reseed(&blake2s(&[&hardware, &jitter, &ticks])));
    if found == 4 {
        klog::info!("entropy: CPU random numbers, boot jitter and interrupt timing");
    } else {
        klog::warn!(
            "entropy: no CPU random-number instructions; seeded from boot jitter and interrupt timing only"
        );
    }
}

/// Fills `out` with random bytes, mixing in fresh interrupt timing first.
pub fn fill(out: &mut [u8]) {
    let mut fresh = [0u8; 8 * SAMPLES + 16];
    for (chunk, sample) in fresh.as_chunks_mut::<8>().0.iter_mut().zip(&RING) {
        chunk.copy_from_slice(&sample.load(Ordering::Relaxed).to_le_bytes());
    }
    let tail = 8 * SAMPLES;
    fresh[tail..tail + 8].copy_from_slice(&arch::cycles().to_le_bytes());
    if let Some(value) = arch::hardware_random() {
        fresh[tail + 8..].copy_from_slice(&value.to_le_bytes());
    }
    arch::without_interrupts(|| {
        let mut rng = RNG.lock();
        rng.reseed(&fresh);
        rng.fill(out);
    });
}

/// Smoke-test check: outputs differ and are not trivially patterned.
pub fn self_test() {
    let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
    fill(&mut a);
    fill(&mut b);
    assert_ne!(a, b, "two requests returned the same bytes");
    assert!(a.iter().any(|&x| x != 0) && a.iter().any(|&x| x != a[0]));
    klog::info!("entropy self-test passed");
}
