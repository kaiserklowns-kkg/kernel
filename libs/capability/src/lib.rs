//! Capabilities: the only form of authority in Oceans (ADR-0006, ADR-0011).
//!
//! A [`Capability`] pairs a reference to a kernel object with [`Rights`].
//! Processes hold capabilities in a [`CapTable`] and name them by
//! [`Handle`]. The rules:
//!
//! - **Unforgeable.** A handle is only meaningful in its own table, and a
//!   generation counter makes handles to closed slots fail, never alias.
//! - **Monotonic rights.** Deriving a capability can only keep or drop
//!   rights, never add them, and requires [`Rights::DUPLICATE`].
//! - **Move-only transfer.** [`transfer`] removes a capability from one table
//!   exactly when it enters another, and requires [`Rights::TRANSFER`].
//! - **Revocation.** [`Capability::derive_revocable`] returns a [`Revoker`].
//!   Revoking it invalidates that capability and everything derived from it,
//!   in every table, without the kernel tracking where copies went. Revoked
//!   entries are purged when next touched or by [`CapTable::sweep`].
//!
//! The crate is generic over the object type and contains no locking; the
//! kernel instantiates it with its object enum and locks each table.

#![no_std]

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::BitOr;
use core::sync::atomic::{AtomicBool, Ordering};

/// What a capability permits. Object types define which rights they honour.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const MAP: Self = Self(1 << 3);
    pub const SEND: Self = Self(1 << 4);
    pub const RECEIVE: Self = Self(1 << 5);
    pub const SIGNAL: Self = Self(1 << 6);
    pub const WAIT: Self = Self(1 << 7);
    /// Change the object's configuration (e.g. resize, set limits).
    pub const MANAGE: Self = Self(1 << 8);
    /// Derive new capabilities from this one.
    pub const DUPLICATE: Self = Self(1 << 9);
    /// Move this capability to another table (over IPC).
    pub const TRANSFER: Self = Self(1 << 10);
    pub const ALL: Self = Self((1 << 11) - 1);

    const NAMES: [&'static str; 11] = [
        "READ",
        "WRITE",
        "EXECUTE",
        "MAP",
        "SEND",
        "RECEIVE",
        "SIGNAL",
        "WAIT",
        "MANAGE",
        "DUPLICATE",
        "TRANSFER",
    ];

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Rights from raw bits (e.g. a syscall argument). Unknown bits are an
    /// error rather than ignored, so new rights cannot be smuggled in by old
    /// callers.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::ALL.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl BitOr for Rights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl fmt::Debug for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("NONE");
        }
        let mut first = true;
        for (bit, name) in Self::NAMES.iter().enumerate() {
            if self.0 & (1 << bit) != 0 {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

/// Names a slot in one [`CapTable`]: index in the low 32 bits, generation in
/// the high 32. Never 0 (generations start at 1), so 0 can mean "no handle"
/// in the syscall ABI.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Handle(u64);

impl Handle {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    const fn new(index: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | index as u64)
    }

    const fn index(self) -> u32 {
        self.0 as u32
    }

    const fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle({}#{})", self.index(), self.generation())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    /// No capability under this handle: never issued, closed, or stale.
    InvalidHandle(Handle),
    /// The capability was revoked (and has now been removed).
    Revoked(Handle),
    /// The capability lacks rights the operation requires.
    MissingRights { required: Rights, held: Rights },
    /// A derived capability would hold rights its source does not.
    RightsEscalation { requested: Rights, held: Rights },
    /// The table has reached its limit.
    TableFull,
}

struct RevocationNode {
    revoked: AtomicBool,
    parent: Option<Arc<RevocationNode>>,
}

impl RevocationNode {
    fn is_revoked(node: &Option<Arc<Self>>) -> bool {
        let mut cursor = node.as_deref();
        while let Some(node) = cursor {
            if node.revoked.load(Ordering::Acquire) {
                return true;
            }
            cursor = node.parent.as_deref();
        }
        false
    }
}

/// Revokes one revocable capability and everything derived from it.
#[derive(Clone)]
pub struct Revoker(Arc<RevocationNode>);

impl Revoker {
    /// Irreversibly revokes. Holders find out on their next use.
    pub fn revoke(&self) {
        self.0.revoked.store(true, Ordering::Release);
    }

    pub fn is_revoked(&self) -> bool {
        self.0.revoked.load(Ordering::Acquire)
    }
}

impl fmt::Debug for Revoker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Revoker")
            .field("revoked", &self.is_revoked())
            .finish()
    }
}

/// Authority over one kernel object.
pub struct Capability<O> {
    object: O,
    rights: Rights,
    revocation: Option<Arc<RevocationNode>>,
}

impl<O> Capability<O> {
    /// A capability for a newly created object. Only the kernel creates
    /// these; everything else is derived.
    pub fn new(object: O, rights: Rights) -> Self {
        Self {
            object,
            rights,
            revocation: None,
        }
    }

    pub fn object(&self) -> &O {
        &self.object
    }

    pub fn rights(&self) -> Rights {
        self.rights
    }

    pub fn is_revoked(&self) -> bool {
        RevocationNode::is_revoked(&self.revocation)
    }

    /// Checks that the capability is live and holds `required`.
    pub fn check(&self, required: Rights) -> Result<(), CapError> {
        if !self.rights.contains(required) {
            return Err(CapError::MissingRights {
                required,
                held: self.rights,
            });
        }
        Ok(())
    }

    fn check_derive(&self, rights: Rights) -> Result<(), CapError> {
        self.check(Rights::DUPLICATE)?;
        if !self.rights.contains(rights) {
            return Err(CapError::RightsEscalation {
                requested: rights,
                held: self.rights,
            });
        }
        Ok(())
    }
}

impl<O: Clone> Capability<O> {
    /// A copy with `rights` (a subset of ours), revoked together with us.
    pub fn derive(&self, rights: Rights) -> Result<Self, CapError> {
        self.check_derive(rights)?;
        Ok(Self {
            object: self.object.clone(),
            rights,
            revocation: self.revocation.clone(),
        })
    }

    /// A copy with `rights` that can additionally be revoked on its own.
    pub fn derive_revocable(&self, rights: Rights) -> Result<(Self, Revoker), CapError> {
        self.check_derive(rights)?;
        let node = Arc::new(RevocationNode {
            revoked: AtomicBool::new(false),
            parent: self.revocation.clone(),
        });
        let capability = Self {
            object: self.object.clone(),
            rights,
            revocation: Some(node.clone()),
        };
        Ok((capability, Revoker(node)))
    }
}

impl<O: fmt::Debug> fmt::Debug for Capability<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Capability")
            .field("object", &self.object)
            .field("rights", &self.rights)
            .field("revoked", &self.is_revoked())
            .finish()
    }
}

struct Slot<O> {
    generation: u32,
    capability: Option<Capability<O>>,
}

/// A process's capabilities.
pub struct CapTable<O> {
    slots: Vec<Slot<O>>,
    free: Vec<u32>,
    len: usize,
    limit: usize,
}

impl<O> CapTable<O> {
    /// An empty table holding at most `limit` capabilities (a per-process
    /// resource limit; at most `u32::MAX`).
    pub const fn new(limit: usize) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
            limit,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len >= self.limit.min(u32::MAX as usize)
    }

    pub fn insert(&mut self, capability: Capability<O>) -> Result<Handle, CapError> {
        if self.is_full() {
            return Err(CapError::TableFull);
        }
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                self.slots.push(Slot {
                    generation: 1,
                    capability: None,
                });
                (self.slots.len() - 1) as u32
            }
        };
        let slot = &mut self.slots[index as usize];
        slot.capability = Some(capability);
        self.len += 1;
        Ok(Handle::new(index, slot.generation))
    }

    /// The live capability under `handle`, if it holds `required`. A revoked
    /// capability is removed and reported as [`CapError::Revoked`].
    pub fn get(&mut self, handle: Handle, required: Rights) -> Result<&Capability<O>, CapError> {
        let revoked = self.slot(handle)?.is_revoked();
        if revoked {
            self.remove(handle)?;
            return Err(CapError::Revoked(handle));
        }
        let capability = self.slot(handle)?;
        capability.check(required)?;
        Ok(capability)
    }

    /// Closes `handle`, returning its capability (revoked or not).
    pub fn remove(&mut self, handle: Handle) -> Result<Capability<O>, CapError> {
        self.slot(handle)?;
        let slot = &mut self.slots[handle.index() as usize];
        let capability = slot.capability.take().expect("checked by slot()");
        // Skip 0 on wrap-around so handles are never 0.
        slot.generation = slot.generation.checked_add(1).unwrap_or(1);
        self.free.push(handle.index());
        self.len -= 1;
        Ok(capability)
    }

    /// Removes every revoked capability. Returns how many were removed.
    pub fn sweep(&mut self) -> usize {
        let revoked: Vec<Handle> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.capability.as_ref().is_some_and(Capability::is_revoked))
            .map(|(index, slot)| Handle::new(index as u32, slot.generation))
            .collect();
        for &handle in &revoked {
            let _ = self.remove(handle);
        }
        revoked.len()
    }

    fn slot(&self, handle: Handle) -> Result<&Capability<O>, CapError> {
        self.slots
            .get(handle.index() as usize)
            .filter(|slot| slot.generation == handle.generation())
            .and_then(|slot| slot.capability.as_ref())
            .ok_or(CapError::InvalidHandle(handle))
    }
}

impl<O: Clone> CapTable<O> {
    /// Derives a capability with `rights` into this table.
    pub fn derive(&mut self, handle: Handle, rights: Rights) -> Result<Handle, CapError> {
        if self.is_full() {
            return Err(CapError::TableFull);
        }
        let derived = self.get(handle, Rights::NONE)?.derive(rights)?;
        self.insert(derived)
    }

    /// Derives a separately revocable capability with `rights` into this table.
    pub fn derive_revocable(
        &mut self,
        handle: Handle,
        rights: Rights,
    ) -> Result<(Handle, Revoker), CapError> {
        if self.is_full() {
            return Err(CapError::TableFull);
        }
        let (derived, revoker) = self.get(handle, Rights::NONE)?.derive_revocable(rights)?;
        Ok((self.insert(derived)?, revoker))
    }
}

/// Moves the capability under `handle` from `from` to `to`. Requires
/// [`Rights::TRANSFER`]. On error neither table changes.
pub fn transfer<O>(
    from: &mut CapTable<O>,
    handle: Handle,
    to: &mut CapTable<O>,
) -> Result<Handle, CapError> {
    from.get(handle, Rights::TRANSFER)?;
    if to.is_full() {
        return Err(CapError::TableFull);
    }
    let capability = from.remove(handle)?;
    Ok(to.insert(capability).expect("checked for room above"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;

    #[derive(Debug)]
    struct Object;

    type Table = CapTable<Arc<Object>>;

    fn object() -> Arc<Object> {
        Arc::new(Object)
    }

    const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

    #[test]
    fn rights_algebra_and_parsing() {
        let rw_dup = RW | Rights::DUPLICATE;
        assert!(rw_dup.contains(RW));
        assert!(!RW.contains(rw_dup));
        assert_eq!(rw_dup.without(Rights::DUPLICATE), RW);
        assert_eq!(Rights::from_bits(RW.bits()), Some(RW));
        assert_eq!(Rights::from_bits(1 << 20), None, "unknown bits rejected");
        assert_eq!(alloc::format!("{RW:?}"), "READ|WRITE");
    }

    #[test]
    fn handles_are_checked_and_never_alias() {
        let mut table = Table::new(16);
        let a = table
            .insert(Capability::new(object(), Rights::ALL))
            .unwrap();
        assert_ne!(a.raw(), 0);
        table.remove(a).unwrap();
        let b = table
            .insert(Capability::new(object(), Rights::READ))
            .unwrap();
        assert_eq!(a.index(), b.index(), "slot reused");
        assert_eq!(
            table.get(a, Rights::NONE).err(),
            Some(CapError::InvalidHandle(a)),
            "stale handle"
        );
        assert!(table.get(b, Rights::READ).is_ok());
        let forged = Handle::from_raw(0xdead_0000_0007);
        assert_eq!(
            table.get(forged, Rights::NONE).err(),
            Some(CapError::InvalidHandle(forged))
        );
        assert_eq!(
            table.remove(a).err(),
            Some(CapError::InvalidHandle(a)),
            "no double close"
        );
    }

    #[test]
    fn required_rights_are_enforced() {
        let mut table = Table::new(16);
        let h = table
            .insert(Capability::new(object(), Rights::READ))
            .unwrap();
        assert_eq!(
            table.get(h, RW).err(),
            Some(CapError::MissingRights {
                required: RW,
                held: Rights::READ
            })
        );
    }

    #[test]
    fn derive_only_drops_rights_and_needs_duplicate() {
        let mut table = Table::new(16);
        let full = table
            .insert(Capability::new(object(), RW | Rights::DUPLICATE))
            .unwrap();
        let read = table.derive(full, Rights::READ).unwrap();
        assert_eq!(
            table.get(read, Rights::NONE).unwrap().rights(),
            Rights::READ
        );

        assert_eq!(
            table.derive(full, RW | Rights::TRANSFER).err(),
            Some(CapError::RightsEscalation {
                requested: RW | Rights::TRANSFER,
                held: RW | Rights::DUPLICATE
            })
        );
        assert_eq!(
            table.derive(read, Rights::READ).err(),
            Some(CapError::MissingRights {
                required: Rights::DUPLICATE,
                held: Rights::READ
            }),
            "no DUPLICATE right"
        );
    }

    #[test]
    fn revocation_cuts_off_descendants_in_all_tables() {
        let mut owner = Table::new(16);
        let mut client = Table::new(16);
        let root = owner
            .insert(Capability::new(object(), Rights::ALL))
            .unwrap();

        let (lent, revoker) = owner.derive_revocable(root, Rights::ALL).unwrap();
        let grandchild = owner.derive(lent, Rights::READ | Rights::TRANSFER).unwrap();
        let moved = transfer(&mut owner, grandchild, &mut client).unwrap();
        let (nested, nested_revoker) = owner.derive_revocable(lent, Rights::ALL).unwrap();

        revoker.revoke();
        assert_eq!(
            client.get(moved, Rights::NONE).err(),
            Some(CapError::Revoked(moved))
        );
        assert_eq!(client.len(), 0, "revoked entry purged on access");
        assert_eq!(
            owner.get(lent, Rights::NONE).err(),
            Some(CapError::Revoked(lent))
        );
        assert!(!nested_revoker.is_revoked());
        assert_eq!(
            owner.get(nested, Rights::NONE).err(),
            Some(CapError::Revoked(nested)),
            "nested"
        );
        assert!(
            owner.get(root, Rights::ALL).is_ok(),
            "the source is unaffected"
        );

        let (derived, _) = owner.derive_revocable(root, Rights::READ).unwrap();
        revoker.revoke();
        assert!(
            owner.get(derived, Rights::READ).is_ok(),
            "revoking twice is harmless"
        );
    }

    #[test]
    fn revoked_capabilities_cannot_be_derived_or_transferred() {
        let mut a = Table::new(16);
        let mut b = Table::new(16);
        let root = a.insert(Capability::new(object(), Rights::ALL)).unwrap();
        let (lent, revoker) = a.derive_revocable(root, Rights::ALL).unwrap();
        let lent2 = a.derive(lent, Rights::ALL).unwrap();
        revoker.revoke();
        assert_eq!(
            a.derive(lent, Rights::READ).err(),
            Some(CapError::Revoked(lent))
        );
        assert_eq!(
            transfer(&mut a, lent2, &mut b).err(),
            Some(CapError::Revoked(lent2))
        );
        assert!(b.is_empty());
    }

    #[test]
    fn transfer_moves_and_is_atomic() {
        let mut a = Table::new(16);
        let mut full = Table::new(1);
        full.insert(Capability::new(object(), Rights::NONE))
            .unwrap();
        let mut b = Table::new(16);

        let h = a
            .insert(Capability::new(object(), Rights::READ | Rights::TRANSFER))
            .unwrap();
        assert_eq!(
            transfer(&mut a, h, &mut full).err(),
            Some(CapError::TableFull)
        );
        assert!(
            a.get(h, Rights::READ).is_ok(),
            "source untouched on failure"
        );

        let moved = transfer(&mut a, h, &mut b).unwrap();
        assert_eq!(
            a.get(h, Rights::NONE).err(),
            Some(CapError::InvalidHandle(h)),
            "moved out"
        );
        assert!(b.get(moved, Rights::READ).is_ok());

        let pinned = a.insert(Capability::new(object(), Rights::READ)).unwrap();
        assert_eq!(
            transfer(&mut a, pinned, &mut b).err(),
            Some(CapError::MissingRights {
                required: Rights::TRANSFER,
                held: Rights::READ
            })
        );
    }

    #[test]
    fn table_limit_and_sweep() {
        let mut table = Table::new(3);
        let root = table
            .insert(Capability::new(object(), Rights::ALL))
            .unwrap();
        let (a, revoker) = table.derive_revocable(root, Rights::ALL).unwrap();
        let _b = table.derive(a, Rights::READ).unwrap();
        assert_eq!(
            table.derive(root, Rights::READ).err(),
            Some(CapError::TableFull)
        );
        revoker.revoke();
        assert_eq!(table.sweep(), 2);
        assert_eq!(table.len(), 1);
        assert!(table.derive(root, Rights::READ).is_ok());
    }

    #[test]
    fn object_lifetime_follows_capabilities() {
        let obj = object();
        let mut table = Table::new(16);
        let h = table
            .insert(Capability::new(obj.clone(), Rights::ALL))
            .unwrap();
        let d = table.derive(h, Rights::READ).unwrap();
        assert_eq!(Arc::strong_count(&obj), 3);
        drop(table.remove(h).unwrap());
        drop(table.remove(d).unwrap());
        assert_eq!(
            Arc::strong_count(&obj),
            1,
            "closing all handles releases the object"
        );
    }
}
