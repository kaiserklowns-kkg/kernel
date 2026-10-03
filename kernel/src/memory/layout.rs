//! Virtual address-space layout (ADR-0009), x86_64 4-level paging.
//!
//! ```text
//! 0x0000_0000_0000_0000 ┐ user space (128 TiB); page 0 never mapped
//! 0x0000_7fff_ffff_ffff ┘
//!            ... non-canonical hole ...
//! 0xffff_8000_0000_0000   direct map of RAM          64 TiB  (PML4 256–383)
//! 0xffff_c000_0000_0000   kernel heap               512 GiB  (PML4 384)
//! 0xffff_c080_0000_0000   kernel stacks             512 GiB  (PML4 385)
//! 0xffff_c100_0000_0000   MMIO mappings             512 GiB  (PML4 386)
//!            ... unused ...
//! 0xffff_ffff_8000_0000   kernel image                2 GiB  (PML4 511)
//! ```

use core::ops::Range;

pub const USER: Range<u64> = 0x0000_0000_0000_1000..0x0000_8000_0000_0000;
pub const DIRECT_MAP: Range<u64> = 0xffff_8000_0000_0000..0xffff_c000_0000_0000;
pub const KERNEL_HEAP: Range<u64> = 0xffff_c000_0000_0000..0xffff_c080_0000_0000;
pub const KERNEL_STACKS: Range<u64> = 0xffff_c080_0000_0000..0xffff_c100_0000_0000;
pub const MMIO: Range<u64> = 0xffff_c100_0000_0000..0xffff_c180_0000_0000;
pub const KERNEL_IMAGE: Range<u64> = 0xffff_ffff_8000_0000..0xffff_ffff_ffff_f000;
