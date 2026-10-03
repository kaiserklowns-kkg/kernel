//! Copying files and whole trees, and removing trees (ADR-0039).
//!
//! A copy moves its data through one [`Shared`] buffer attached to both the
//! source and the destination handle: the source's service reads into it,
//! the destination's service writes from it, and the bytes never pass
//! through the caller, even between two filesystems.

use crate::{FsError, Kind, MAX_NAME, Node, Shared, Status, flags};

/// How deep a tree copy or removal descends below its starting directory.
pub const MAX_DEPTH: usize = 32;

/// Which side of a copy failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Source,
    Destination,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyError {
    pub side: Side,
    pub error: FsError,
}

impl CopyError {
    fn source(error: FsError) -> Self {
        Self {
            side: Side::Source,
            error,
        }
    }

    fn destination(error: FsError) -> Self {
        Self {
            side: Side::Destination,
            error,
        }
    }
}

/// What a copy has done so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub files: u64,
    pub directories: u64,
    pub bytes: u64,
}

/// Copies files and trees through one shared buffer.
pub struct Copier {
    shared: Shared,
    pub totals: Totals,
}

impl Copier {
    /// A copier moving up to `buffer` bytes per request (see
    /// [`crate::MIN_SHARED`] and [`crate::MAX_SHARED`]).
    pub fn new(buffer: usize) -> Result<Self, FsError> {
        Ok(Self {
            shared: Shared::new(buffer)?,
            totals: Totals::default(),
        })
    }

    /// Replaces the contents of file `destination` (opened for writing)
    /// with those of file `source`; returns the bytes copied. The source
    /// is read until its end, so a file growing meanwhile is copied as far
    /// as it had grown.
    pub fn file(&mut self, source: &Node, destination: &Node) -> Result<u64, CopyError> {
        source.share(&self.shared).map_err(CopyError::source)?;
        destination
            .share(&self.shared)
            .map_err(CopyError::destination)?;
        destination.truncate(0).map_err(CopyError::destination)?;
        let mut offset = 0;
        loop {
            let read = source
                .read_buffer(&self.shared, offset, self.shared.size())
                .map_err(CopyError::source)?;
            if read == 0 {
                break;
            }
            destination
                .write_buffer(&self.shared, offset, read)
                .map_err(CopyError::destination)?;
            offset += read as u64;
        }
        self.totals.files += 1;
        self.totals.bytes += offset;
        Ok(offset)
    }

    /// Copies everything in directory `source` into directory
    /// `destination` (opened for writing): missing entries are created,
    /// existing files replaced, existing directories merged into.
    pub fn directory(&mut self, source: &Node, destination: &Node) -> Result<(), CopyError> {
        self.copy_entries(source, destination, 0)
    }

    fn copy_entries(
        &mut self,
        source: &Node,
        destination: &Node,
        depth: usize,
    ) -> Result<(), CopyError> {
        if depth >= MAX_DEPTH {
            return Err(CopyError::destination(FsError::Status(Status::NoSpace)));
        }
        let mut name = [0u8; MAX_NAME];
        for index in 0.. {
            let Some((kind, len)) = source.entry(index, &mut name).map_err(CopyError::source)?
            else {
                break;
            };
            let name = core::str::from_utf8(&name[..len])
                .map_err(|_| CopyError::source(FsError::Status(Status::InvalidName)))?;
            let (from, _) = source.open(name, 0).map_err(CopyError::source)?;
            let create = match kind {
                Kind::File => flags::CREATE_FILE,
                Kind::Directory => flags::CREATE_DIRECTORY,
            };
            let result = match destination.open(name, create | flags::WRITE) {
                Ok((to, found)) => {
                    let result = match (kind, found) {
                        (Kind::File, Kind::File) => self.file(&from, &to).map(drop),
                        (Kind::Directory, Kind::Directory) => {
                            self.totals.directories += 1;
                            self.copy_entries(&from, &to, depth + 1)
                        }
                        (Kind::File, Kind::Directory) => Err(CopyError::destination(
                            FsError::Status(Status::IsADirectory),
                        )),
                        (Kind::Directory, Kind::File) => Err(CopyError::destination(
                            FsError::Status(Status::NotADirectory),
                        )),
                    };
                    to.close();
                    result
                }
                Err(error) => Err(CopyError::destination(error)),
            };
            from.close();
            result?;
        }
        Ok(())
    }
}

/// Removes entry `name` of `parent` (opened for writing) and, if it is a
/// directory, everything in it.
pub fn remove_tree(parent: &Node, name: &str) -> Result<(), FsError> {
    remove_at(parent, name, 0)
}

fn remove_at(parent: &Node, name: &str, depth: usize) -> Result<(), FsError> {
    match parent.remove(name) {
        Err(FsError::Status(Status::NotEmpty)) if depth < MAX_DEPTH => {}
        done => return done,
    }
    let (directory, kind) = parent.open(name, flags::WRITE)?;
    if kind != Kind::Directory {
        directory.close();
        return Err(FsError::Status(Status::NotEmpty));
    }
    // Each removal shifts the rest down, so the first entry is always next.
    let mut child = [0u8; MAX_NAME];
    let emptied = loop {
        match directory.entry(0, &mut child) {
            Ok(None) => break Ok(()),
            Ok(Some((_, len))) => {
                let removed = core::str::from_utf8(&child[..len])
                    .map_err(|_| FsError::Status(Status::InvalidName))
                    .and_then(|child| remove_at(&directory, child, depth + 1));
                if let Err(error) = removed {
                    break Err(error);
                }
            }
            Err(error) => break Err(error),
        }
    };
    directory.close();
    emptied?;
    parent.remove(name)
}
