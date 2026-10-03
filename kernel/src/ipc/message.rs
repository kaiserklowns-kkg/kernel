//! IPC messages: a label, a small inline payload and transferred
//! capabilities. Bulk data travels in memory objects passed by capability.

use alloc::vec::Vec;

use super::IpcError;
use crate::object::Capability;

/// Inline payload limit: large enough for typical requests and replies,
/// small enough to copy cheaply on every call.
pub const MAX_INLINE_BYTES: usize = 256;
/// Capabilities one message may carry.
pub const MAX_CAPABILITIES: usize = 4;

pub struct Message {
    /// Protocol-defined operation or status code.
    pub label: u64,
    data: [u8; MAX_INLINE_BYTES],
    len: usize,
    capabilities: Vec<Capability>,
}

impl Message {
    pub fn new(label: u64, data: &[u8]) -> Result<Self, IpcError> {
        if data.len() > MAX_INLINE_BYTES {
            return Err(IpcError::MessageTooLarge);
        }
        let mut message = Self {
            label,
            data: [0; MAX_INLINE_BYTES],
            len: data.len(),
            capabilities: Vec::new(),
        };
        message.data[..data.len()].copy_from_slice(data);
        Ok(message)
    }

    /// Attaches a capability; it moves to the receiver with the message.
    pub fn attach(&mut self, capability: Capability) -> Result<(), IpcError> {
        if self.capabilities.len() >= MAX_CAPABILITIES {
            return Err(IpcError::TooManyCapabilities);
        }
        self.capabilities
            .try_reserve(1)
            .map_err(|_| IpcError::OutOfMemory)?;
        self.capabilities.push(capability);
        Ok(())
    }

    pub fn data(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Takes the attached capabilities (the receiver inserts them into its
    /// table).
    pub fn take_capabilities(&mut self) -> Vec<Capability> {
        core::mem::take(&mut self.capabilities)
    }
}
