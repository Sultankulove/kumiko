//! `file:` provider on top of the local file system.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use kumiko_service_api::{
    BoxFuture, DirEntry, FileKind, FileStat, FileSystem, FsError, WriteOptions,
};
use kumiko_types::Uri;

/// The local file system, addressed by `file:` URIs.
#[derive(Debug, Default)]
pub struct LocalFileSystem {
    temp_counter: AtomicU64,
}

impl LocalFileSystem {
    pub fn new() -> Self {
        Self::default()
    }
}

fn path_of(uri: &Uri) -> Result<PathBuf, FsError> {
    if uri.scheme() != "file" {
        return Err(FsError::Unsupported(uri.clone()));
    }
    uri.to_file_path()
        .ok_or_else(|| FsError::Unsupported(uri.clone()))
}

fn kind_of(file_type: std::fs::FileType) -> FileKind {
    if file_type.is_symlink() {
        FileKind::Symlink
    } else if file_type.is_dir() {
        FileKind::Directory
    } else {
        FileKind::File
    }
}

async fn exists(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok()
}

impl LocalFileSystem {
    async fn write_atomic(
        &self,
        uri: &Uri,
        path: &Path,
        data: Vec<u8>,
        options: WriteOptions,
    ) -> Result<(), FsError> {
        let err = |e: io::Error| FsError::from_io(uri, &e);
        let existing = tokio::fs::metadata(path).await.ok();
        match (&existing, options) {
            (None, WriteOptions { create: false, .. }) => {
                return Err(FsError::NotFound(uri.clone()));
            }
            (
                Some(_),
                WriteOptions {
                    overwrite: false, ..
                },
            ) => {
                return Err(FsError::AlreadyExists(uri.clone()));
            }
            _ => {}
        }
        let parent = path.parent().ok_or_else(|| FsError::Io {
            uri: uri.clone(),
            message: "path has no parent directory".into(),
        })?;
        let name = path
            .file_name()
            .map_or_else(Default::default, |n| n.to_string_lossy());
        let temp = parent.join(format!(
            ".{name}.kumiko-{}-{}.tmp",
            std::process::id(),
            self.temp_counter.fetch_add(1, Ordering::Relaxed)
        ));
        let result = async {
            tokio::fs::write(&temp, &data).await?;
            if let Some(metadata) = &existing {
                tokio::fs::set_permissions(&temp, metadata.permissions()).await?;
            }
            tokio::fs::rename(&temp, path).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        result.map_err(err)
    }
}

impl FileSystem for LocalFileSystem {
    fn read<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<Vec<u8>, FsError>> {
        Box::pin(async move {
            let path = path_of(uri)?;
            tokio::fs::read(&path)
                .await
                .map_err(|e| FsError::from_io(uri, &e))
        })
    }

    fn write<'a>(
        &'a self,
        uri: &'a Uri,
        data: Vec<u8>,
        options: WriteOptions,
    ) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let path = path_of(uri)?;
            self.write_atomic(uri, &path, data, options).await
        })
    }

    fn stat<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<FileStat, FsError>> {
        Box::pin(async move {
            let path = path_of(uri)?;
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|e| FsError::from_io(uri, &e))?;
            Ok(FileStat {
                kind: kind_of(metadata.file_type()),
                size: metadata.len(),
                mtime_ms: metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64),
                readonly: metadata.permissions().readonly(),
            })
        })
    }

    fn read_dir<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<Vec<DirEntry>, FsError>> {
        Box::pin(async move {
            let path = path_of(uri)?;
            let err = |e: io::Error| FsError::from_io(uri, &e);
            let mut reader = tokio::fs::read_dir(&path).await.map_err(err)?;
            let mut entries = Vec::new();
            while let Some(entry) = reader.next_entry().await.map_err(err)? {
                let kind = kind_of(entry.file_type().await.map_err(err)?);
                entries.push(DirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    kind,
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        })
    }

    fn create_dir<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let path = path_of(uri)?;
            tokio::fs::create_dir_all(&path)
                .await
                .map_err(|e| FsError::from_io(uri, &e))
        })
    }

    fn delete<'a>(&'a self, uri: &'a Uri, recursive: bool) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let path = path_of(uri)?;
            let err = |e: io::Error| FsError::from_io(uri, &e);
            let metadata = tokio::fs::symlink_metadata(&path).await.map_err(err)?;
            if metadata.is_dir() {
                if recursive {
                    tokio::fs::remove_dir_all(&path).await.map_err(err)
                } else {
                    tokio::fs::remove_dir(&path).await.map_err(err)
                }
            } else {
                tokio::fs::remove_file(&path).await.map_err(err)
            }
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a Uri,
        to: &'a Uri,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let (source, target) = (path_of(from)?, path_of(to)?);
            if !exists(&source).await {
                return Err(FsError::NotFound(from.clone()));
            }
            if !overwrite && exists(&target).await {
                return Err(FsError::AlreadyExists(to.clone()));
            }
            tokio::fs::rename(&source, &target)
                .await
                .map_err(|e| FsError::from_io(to, &e))
        })
    }

    fn copy<'a>(
        &'a self,
        from: &'a Uri,
        to: &'a Uri,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let (source, target) = (path_of(from)?, path_of(to)?);
            let metadata = tokio::fs::metadata(&source)
                .await
                .map_err(|e| FsError::from_io(from, &e))?;
            if metadata.is_dir() {
                return Err(FsError::Io {
                    uri: from.clone(),
                    message: "copying directories is not supported".into(),
                });
            }
            if !overwrite && exists(&target).await {
                return Err(FsError::AlreadyExists(to.clone()));
            }
            tokio::fs::copy(&source, &target)
                .await
                .map(drop)
                .map_err(|e| FsError::from_io(to, &e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(path: &Path) -> Uri {
        Uri::from_file_path(path).unwrap()
    }

    #[tokio::test]
    async fn file_operations() {
        let dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new();
        let a = uri(&dir.path().join("a.txt"));
        let b = uri(&dir.path().join("sub/b.txt"));
        let sub = uri(&dir.path().join("sub"));

        let create_only = WriteOptions {
            create: true,
            overwrite: false,
        };
        fs.write(&a, b"one".to_vec(), create_only).await.unwrap();
        assert_eq!(
            fs.write(&a, b"two".to_vec(), create_only).await,
            Err(FsError::AlreadyExists(a.clone()))
        );
        fs.write(&a, b"two".to_vec(), WriteOptions::UPSERT)
            .await
            .unwrap();
        assert_eq!(fs.read(&a).await.unwrap(), b"two");

        let replace_only = WriteOptions {
            create: false,
            overwrite: true,
        };
        assert_eq!(
            fs.write(&b, vec![], replace_only).await,
            Err(FsError::NotFound(b.clone()))
        );

        fs.create_dir(&sub).await.unwrap();
        fs.copy(&a, &b, false).await.unwrap();
        assert_eq!(
            fs.copy(&a, &b, false).await,
            Err(FsError::AlreadyExists(b.clone()))
        );

        let stat = fs.stat(&b).await.unwrap();
        assert_eq!((stat.kind, stat.size), (FileKind::File, 3));
        assert!(stat.mtime_ms.is_some());

        let root = uri(dir.path());
        let names: Vec<_> = fs
            .read_dir(&root)
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("a.txt".to_string(), FileKind::File),
                ("sub".to_string(), FileKind::Directory)
            ]
        );

        let c = uri(&dir.path().join("c.txt"));
        fs.rename(&a, &c, false).await.unwrap();
        assert_eq!(fs.read(&a).await, Err(FsError::NotFound(a.clone())));
        assert!(fs.delete(&sub, false).await.is_err(), "non-empty directory");
        fs.delete(&sub, true).await.unwrap();
        fs.delete(&c, false).await.unwrap();
        assert!(
            fs.read_dir(&root).await.unwrap().is_empty(),
            "no temp files left"
        );
    }

    #[tokio::test]
    async fn other_schemes_are_unsupported() {
        let fs = LocalFileSystem::new();
        let virtual_doc = Uri::parse("untitled:1").unwrap();
        assert_eq!(
            fs.read(&virtual_doc).await,
            Err(FsError::Unsupported(virtual_doc.clone()))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn overwrite_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
        let fs = LocalFileSystem::new();
        fs.write(
            &uri(&path),
            b"#!/bin/sh\necho\n".to_vec(),
            WriteOptions::UPSERT,
        )
        .await
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }
}
