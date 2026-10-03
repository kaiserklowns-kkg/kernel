//! Kernel objects and the capabilities that name them (ADR-0011).
//!
//! Every resource a process can act on is a kernel object, reachable only
//! through a capability in that process's [`CapTable`]. Object types are
//! added here as their subsystems arrive (threads, address spaces, IPC
//! endpoints, IRQs, MMIO ranges); the capability rules stay the same.

mod memory;

use alloc::sync::Arc;

pub use memory::MemoryObject;

use crate::ipc::{ClientEnd, Notification, ServerEnd};
use oceans_capability::{CapError, Revoker, Rights};

/// Capabilities a process may hold unless its resource limits say otherwise.
pub const DEFAULT_CAP_LIMIT: usize = 4096;

pub type Capability = oceans_capability::Capability<KernelObject>;
pub type CapTable = oceans_capability::CapTable<KernelObject>;

/// A reference to a kernel object. Cloning shares the object.
#[derive(Clone, Debug)]
pub enum KernelObject {
    Memory(Arc<MemoryObject>),
    /// Authority to revoke a capability derived with `derive_revocable`.
    Revoker(Revoker),
    /// Receiving side of an IPC endpoint.
    EndpointServer(Arc<ServerEnd>),
    /// Calling side of an IPC endpoint.
    EndpointClient(Arc<ClientEnd>),
    Notification(Arc<Notification>),
    /// Permission to write to the kernel log (`WRITE`).
    Log,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Memory,
    Revoker,
    EndpointServer,
    EndpointClient,
    Notification,
    Log,
}

impl KernelObject {
    pub fn kind(&self) -> ObjectKind {
        match self {
            Self::Memory(_) => ObjectKind::Memory,
            Self::Revoker(_) => ObjectKind::Revoker,
            Self::EndpointServer(_) => ObjectKind::EndpointServer,
            Self::EndpointClient(_) => ObjectKind::EndpointClient,
            Self::Notification(_) => ObjectKind::Notification,
            Self::Log => ObjectKind::Log,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectError {
    Capability(CapError),
    WrongType {
        expected: ObjectKind,
        found: ObjectKind,
    },
    OutOfMemory,
    /// An offset or length outside the object.
    OutOfBounds,
}

impl From<CapError> for ObjectError {
    fn from(err: CapError) -> Self {
        Self::Capability(err)
    }
}

/// Rights a freshly created object's first capability carries.
pub const fn default_rights(kind: ObjectKind) -> Rights {
    match kind {
        ObjectKind::Memory => Rights::READ
            .union(Rights::WRITE)
            .union(Rights::MAP)
            .union(Rights::MANAGE)
            .union(Rights::DUPLICATE)
            .union(Rights::TRANSFER),
        ObjectKind::Revoker => Rights::MANAGE.union(Rights::TRANSFER),
        ObjectKind::EndpointServer => Rights::RECEIVE
            .union(Rights::DUPLICATE)
            .union(Rights::TRANSFER),
        ObjectKind::EndpointClient => Rights::SEND
            .union(Rights::DUPLICATE)
            .union(Rights::TRANSFER),
        ObjectKind::Notification => Rights::SIGNAL
            .union(Rights::WAIT)
            .union(Rights::DUPLICATE)
            .union(Rights::TRANSFER),
        ObjectKind::Log => Rights::WRITE
            .union(Rights::DUPLICATE)
            .union(Rights::TRANSFER),
    }
}

/// Looks up `handle`, checks `required` rights and that it names memory.
pub fn memory(
    table: &mut CapTable,
    handle: oceans_capability::Handle,
    required: Rights,
) -> Result<Arc<MemoryObject>, ObjectError> {
    let capability = table.get(handle, Rights::NONE)?;
    match capability.object() {
        KernelObject::Memory(memory) => {
            capability.check(required)?;
            Ok(memory.clone())
        }
        other => Err(ObjectError::WrongType {
            expected: ObjectKind::Memory,
            found: other.kind(),
        }),
    }
}

macro_rules! typed_lookup {
    ($(#[$doc:meta])* $name:ident, $variant:ident, $ty:ty) => {
        $(#[$doc])*
        pub fn $name(
            table: &mut CapTable,
            handle: oceans_capability::Handle,
            required: Rights,
        ) -> Result<Arc<$ty>, ObjectError> {
            // Type before rights: a capability of the wrong type is reported
            // as such, whatever rights it carries.
            let capability = table.get(handle, Rights::NONE)?;
            match capability.object() {
                KernelObject::$variant(object) => {
                    capability.check(required)?;
                    Ok(object.clone())
                }
                other => Err(ObjectError::WrongType {
                    expected: ObjectKind::$variant,
                    found: other.kind(),
                }),
            }
        }
    };
}

typed_lookup!(
    /// Looks up an endpoint server end (`RECEIVE` to receive).
    server_end, EndpointServer, ServerEnd
);
typed_lookup!(
    /// Looks up an endpoint client end (`SEND` to call).
    client_end, EndpointClient, ClientEnd
);
typed_lookup!(
    /// Looks up a notification (`SIGNAL` to signal, `WAIT` to wait).
    notification, Notification, Notification
);

/// Revokes through the revoker capability under `handle` (needs MANAGE).
pub fn revoke(table: &mut CapTable, handle: oceans_capability::Handle) -> Result<(), ObjectError> {
    match table.get(handle, Rights::MANAGE)?.object() {
        KernelObject::Revoker(revoker) => {
            revoker.revoke();
            Ok(())
        }
        other => Err(ObjectError::WrongType {
            expected: ObjectKind::Revoker,
            found: other.kind(),
        }),
    }
}

/// End-to-end capability check with real objects, for smoke-test boots.
pub fn self_test() {
    use oceans_capability::transfer;

    let mut server = CapTable::new(DEFAULT_CAP_LIMIT);
    let mut client = CapTable::new(DEFAULT_CAP_LIMIT);

    let object = MemoryObject::new(3 * 4096).expect("self-test: create memory object");
    assert_eq!(object.size(), 3 * 4096);
    let alive = Arc::downgrade(&object);
    let root = server
        .insert(Capability::new(
            KernelObject::Memory(object),
            default_rights(ObjectKind::Memory),
        ))
        .expect("self-test: insert");

    let memory_rw = memory(&mut server, root, Rights::WRITE).expect("self-test: lookup");
    memory_rw
        .write(4090, b"oceans!")
        .expect("self-test: write across a page boundary");
    drop(memory_rw);

    // Lend a read-only, revocable view to the client.
    let (lent, revoker) = server
        .derive_revocable(root, Rights::READ | Rights::TRANSFER)
        .expect("self-test: derive");
    let lent = transfer(&mut server, lent, &mut client).expect("self-test: transfer");
    let revoker = server
        .insert(Capability::new(
            KernelObject::Revoker(revoker),
            default_rights(ObjectKind::Revoker),
        ))
        .expect("self-test: insert revoker");

    let view = memory(&mut client, lent, Rights::READ).expect("self-test: client read");
    let mut buffer = [0u8; 7];
    view.read(4090, &mut buffer).expect("self-test: read");
    assert_eq!(&buffer, b"oceans!");
    drop(view);
    assert!(matches!(
        memory(&mut client, lent, Rights::WRITE),
        Err(ObjectError::Capability(CapError::MissingRights { .. }))
    ));

    revoke(&mut server, revoker).expect("self-test: revoke");
    assert_eq!(
        memory(&mut client, lent, Rights::READ).err(),
        Some(ObjectError::Capability(CapError::Revoked(lent)))
    );

    // Closing the last capability destroys the object, whose `Drop` returns
    // its frames (and panics if the frame allocator rejects any).
    server.remove(root).expect("self-test: close");
    server.remove(revoker).expect("self-test: close revoker");
    assert!(server.is_empty() && client.is_empty());
    assert!(
        alive.upgrade().is_none(),
        "memory object outlived its last capability"
    );
    crate::klog::info!("capability self-test passed");
}
