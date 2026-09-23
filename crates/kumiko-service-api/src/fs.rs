use std::io;

use kumiko_protocol::methods::fs::{DirEntry, FileStat};
use kumiko_types::Uri;

use crate::BoxFuture;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsError {
    #[error("{0} does not exist")]
    NotFound(Uri),
    #[error("{0} already exists")]
    AlreadyExists(Uri),
    #[error("permission denied: {0}")]
    PermissionDenied(Uri),
    #[error("no file system provider for {0}")]
    Unsupported(Uri),
    #[error("{uri}: {message}")]
    Io { uri: Uri, message: String },
}

impl FsError {
    pub fn from_io(uri: &Uri, error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::NotFound(uri.clone()),
            io::ErrorKind::AlreadyExists => Self::AlreadyExists(uri.clone()),
            io::ErrorKind::PermissionDenied => Self::PermissionDenied(uri.clone()),
            _ => Self::Io {
                uri: uri.clone(),
                message: error.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions {
    /// Create the file if it does not exist.
    pub create: bool,
    /// Replace an existing file.
    pub overwrite: bool,
}

impl WriteOptions {
    /// Create or replace.
    pub const UPSERT: Self = Self {
        create: true,
        overwrite: true,
    };
}

/// File access by URI. The core provides `file:`; other schemes may get providers later.
pub trait FileSystem: Send + Sync {
    fn read<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<Vec<u8>, FsError>>;
    /// Replaces the whole file atomically.
    fn write<'a>(
        &'a self,
        uri: &'a Uri,
        data: Vec<u8>,
        options: WriteOptions,
    ) -> BoxFuture<'a, Result<(), FsError>>;
    fn stat<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<FileStat, FsError>>;
    fn read_dir<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<Vec<DirEntry>, FsError>>;
    /// Creates the directory and its missing parents.
    fn create_dir<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<(), FsError>>;
    fn delete<'a>(&'a self, uri: &'a Uri, recursive: bool) -> BoxFuture<'a, Result<(), FsError>>;
    fn rename<'a>(
        &'a self,
        from: &'a Uri,
        to: &'a Uri,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<(), FsError>>;
    fn copy<'a>(
        &'a self,
        from: &'a Uri,
        to: &'a Uri,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<(), FsError>>;
}
