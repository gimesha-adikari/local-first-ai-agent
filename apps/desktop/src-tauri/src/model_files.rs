//! Private, descriptor-relative storage for public model bytes on Linux.

use std::{
    ffi::CString,
    fs::{self, DirBuilder, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use engine::downloads::ModelFiles;

pub struct PrivateModelFiles {
    directory: File,
    location: PathBuf,
}

impl PrivateModelFiles {
    pub fn open(path: &Path) -> io::Result<Box<dyn ModelFiles>> {
        let parent = path.parent().ok_or_else(invalid)?;
        let directory_options = || {
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
            options
        };
        let parent = directory_options().open(parent)?;
        let meta = parent.metadata()?;
        if meta.uid() != user() || meta.mode() & 0o022 != 0 {
            return Err(invalid());
        }
        let entry = fd_path(&parent).join(path.file_name().ok_or_else(invalid)?);
        match DirBuilder::new().mode(0o700).create(&entry) {
            Ok(()) => parent.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let directory = directory_options().open(entry)?;
        let meta = directory.metadata()?;
        if meta.uid() != user() || meta.mode() & 0o077 != 0 {
            return Err(invalid());
        }
        let location = fd_path(&directory);
        Ok(Box::new(Self {
            directory,
            location,
        }))
    }
}

impl ModelFiles for PrivateModelFiles {
    fn inspection_path(&self, name: &str) -> PathBuf {
        self.location.join(name)
    }

    fn open(&self, name: &str, writable: bool, create: bool) -> io::Result<File> {
        let name = filename(name)?;
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .create_new(create)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.location.join(name))?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 || meta.uid() != user() || meta.mode() & 0o077 != 0
        {
            return Err(invalid());
        }
        Ok(file)
    }

    fn free_bytes(&self) -> io::Result<u64> {
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: The descriptor stays open and statvfs writes to allocated storage.
        if unsafe { libc::fstatvfs(self.directory.as_raw_fd(), info.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatvfs initialized the structure on success.
        let info = unsafe { info.assume_init() };
        Ok(info.f_bavail.saturating_mul(info.f_frsize))
    }

    fn install(&self, partial: &str, final_name: &str) -> io::Result<()> {
        let verified = self.open(partial, false, false)?;
        verified.set_permissions(fs::Permissions::from_mode(0o400))?;
        verified.sync_all()?;
        let from = CString::new(filename(partial)?)?;
        let to = CString::new(filename(final_name)?)?;
        let dir = self.directory.as_raw_fd();
        // SAFETY: Both names are valid C strings and dir is an open directory.
        let result = unsafe {
            libc::renameat2(dir, from.as_ptr(), dir, to.as_ptr(), libc::RENAME_NOREPLACE)
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        self.directory.sync_all()
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        match self.open(name, false, false) {
            Ok(_) => {
                fs::remove_file(self.location.join(filename(name)?))?;
                self.directory.sync_all()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn filename(name: &str) -> io::Result<&str> {
    if name.is_empty()
        || name.len() > 192
        || matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(invalid());
    }
    Ok(name)
}

fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn user() -> u32 {
    // SAFETY: geteuid has no arguments or memory effects.
    unsafe { libc::geteuid() }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Unsafe model storage entry",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "agent-private-model-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn private_model_files_reject_symlinks_hard_links_and_name_traversal() {
        use std::os::unix::fs::symlink;

        let root = Scratch::new();
        let outside = root.0.join("outside.gguf");
        fs::write(&outside, b"user weights").unwrap();
        let storage = root.0.join("models");
        symlink(&root.0, &storage).unwrap();
        assert!(PrivateModelFiles::open(&storage).is_err());
        fs::remove_file(&storage).unwrap();

        let files = PrivateModelFiles::open(&storage).unwrap();
        assert_eq!(storage.metadata().unwrap().mode() & 0o777, 0o700);
        assert!(files.free_bytes().unwrap() > 0);
        assert!(files.open("../outside.gguf", true, false).is_err());
        symlink(&outside, storage.join("partial.gguf")).unwrap();
        assert!(files.open("partial.gguf", true, false).is_err());
        assert!(files.remove("partial.gguf").is_err());
        fs::remove_file(storage.join("partial.gguf")).unwrap();
        fs::hard_link(&outside, storage.join("partial.gguf")).unwrap();
        assert!(files.open("partial.gguf", true, false).is_err());
        assert!(files.remove("partial.gguf").is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"user weights");
    }

    #[test]
    fn installation_does_not_clobber_an_existing_file() {
        use std::io::Write;

        let root = Scratch::new();
        let storage = root.0.join("models");
        let files = PrivateModelFiles::open(&storage).unwrap();
        files
            .open("partial.gguf", true, true)
            .unwrap()
            .write_all(b"verified")
            .unwrap();
        files
            .open("final.gguf", true, true)
            .unwrap()
            .write_all(b"existing")
            .unwrap();
        assert_eq!(
            files
                .install("partial.gguf", "final.gguf")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(storage.join("final.gguf")).unwrap(), b"existing");
        files.remove("final.gguf").unwrap();
        files.install("partial.gguf", "final.gguf").unwrap();
        assert!(!storage.join("partial.gguf").exists());
        assert_eq!(fs::read(storage.join("final.gguf")).unwrap(), b"verified");
    }
}
