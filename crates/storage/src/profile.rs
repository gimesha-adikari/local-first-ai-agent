//! Encrypted database bootstrap and crash-safe cutover from legacy SQLite.
//! Only the desktop adapter can supply a key from the OS keyring.
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

use agent_core::inference::InferenceError;
use rusqlite::{Connection, OpenFlags, ffi, params};
use zeroize::Zeroizing;

use crate::{SCHEMA_VERSION, SqliteStore};

const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";
const SIDECARS: [&str; 3] = ["-journal", "-wal", "-shm"];
const HEX: &[u8; 16] = b"0123456789abcdef";

/// A raw SQLCipher key. It is deliberately neither printable nor serializable.
pub struct DatabaseKey(Zeroizing<[u8; 32]>);

impl DatabaseKey {
    pub fn generate() -> Result<Self, ProfileError> {
        let mut bytes = Zeroizing::new([0; 32]);
        getrandom::fill(bytes.as_mut()).map_err(|_| ProfileError::RandomUnavailable)?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut key = Zeroizing::new([0; 32]);
        if bytes.len() != key.len() {
            return None;
        }
        key.copy_from_slice(bytes);
        Some(Self(key))
    }

    /// For use by the trusted native keyring and recovery UI only.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn sqlcipher_literal(&self) -> Zeroizing<[u8; 67]> {
        let mut raw = Zeroizing::new([0; 67]);
        raw[..2].copy_from_slice(b"x'");
        for (output, byte) in raw[2..66]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(self.0.iter())
        {
            output[0] = HEX[(byte >> 4) as usize];
            output[1] = HEX[(byte & 15) as usize];
        }
        raw[66] = b'\'';
        raw
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileKind {
    New,
    InterruptedSetup,
    Plaintext,
    Encrypted,
}

#[derive(Debug)]
pub enum ProfileError {
    Busy,
    DiskFull,
    Io(io::ErrorKind),
    CipherUnavailable,
    RandomUnavailable,
    KeyRejected,
    Validation,
    UnsupportedSchema,
    MissingActiveDatabase,
    UnexpectedFiles,
    Sqlite(Option<rusqlite::ErrorCode>),
    Initialize(InferenceError),
}

impl From<io::Error> for ProfileError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => Self::DiskFull,
            kind => Self::Io(kind),
        }
    }
}

impl From<rusqlite::Error> for ProfileError {
    fn from(error: rusqlite::Error) -> Self {
        // Discard SQL text and engine diagnostics. Some SQL operations bind key material.
        match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                Self::Busy
            }
            Some(rusqlite::ErrorCode::DiskFull) => Self::DiskFull,
            code => Self::Sqlite(code),
        }
    }
}

/// Owns the profile across keyring access, migration, recovery and SQLite startup.
pub struct Profile {
    path: PathBuf,
    stage: PathBuf,
    backup: PathBuf,
    lock: Arc<File>,
}

impl Profile {
    pub fn claim(path: &Path) -> Result<Self, ProfileError> {
        let parent = path.parent().ok_or(ProfileError::UnexpectedFiles)?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(parent)?;
        if !parent.symlink_metadata()?.is_dir() {
            return Err(ProfileError::UnexpectedFiles);
        }
        let path = parent
            .canonicalize()?
            .join(path.file_name().ok_or(ProfileError::UnexpectedFiles)?);
        let lock_path = suffixed(&path, ".lock");
        regular_file(&lock_path)?;
        let lock = private_options().create(true).open(lock_path)?;
        match lock.try_lock() {
            Ok(()) => Ok(Self {
                stage: suffixed(&path, ".encrypting"),
                backup: suffixed(&path, ".plaintext-backup"),
                path,
                lock: Arc::new(lock),
            }),
            Err(TryLockError::WouldBlock) => Err(ProfileError::Busy),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn backup_path(&self) -> &Path {
        &self.backup
    }

    pub fn kind(&self) -> Result<ProfileKind, ProfileError> {
        let backup = regular_file(&self.backup)?;
        let stage = regular_file(&self.stage)?;
        let active = regular_file(&self.path)?;
        let has_active_sidecars = has_sidecars(&self.path)?;
        if active.is_none() {
            if backup.is_some() || has_active_sidecars {
                return Err(ProfileError::MissingActiveDatabase);
            }
            return Ok(if stage.is_some() || has_sidecars(&self.stage)? {
                ProfileKind::InterruptedSetup
            } else {
                ProfileKind::New
            });
        }

        let mut header = [0; 16];
        match File::open(&self.path)?.read_exact(&mut header) {
            Ok(()) if header == SQLITE_HEADER => Ok(ProfileKind::Plaintext),
            Ok(()) => Ok(ProfileKind::Encrypted),
            // A truncated file could have been encrypted. Never classify it as a new profile.
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                Err(ProfileError::Validation)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn open(self, key: &DatabaseKey) -> Result<SqliteStore, ProfileError> {
        match self.kind()? {
            ProfileKind::Encrypted => {}
            ProfileKind::Plaintext => self.migrate(key)?,
            ProfileKind::New | ProfileKind::InterruptedSetup => self.create(key)?,
        }
        let mut store = initialize(keyed_connection(&self.path, key)?)?;
        store._profile_lock = Some(Arc::clone(&self.lock));
        Ok(store)
    }

    /// Authenticate an explicitly supplied recovery key before replacing the keyring item.
    pub fn validate_recovery_key(&self, key: &DatabaseKey) -> Result<(), ProfileError> {
        if self.kind()? != ProfileKind::Encrypted {
            return Err(ProfileError::Validation);
        }
        validate(&keyed_connection(&self.path, key)?)
    }

    fn create(&self, key: &DatabaseKey) -> Result<(), ProfileError> {
        self.create_stage()?;
        drop(initialize(keyed_connection(&self.stage, key)?)?);
        self.verify_stage(key)?;
        self.activate_stage()
    }

    fn migrate(&self, key: &DatabaseKey) -> Result<(), ProfileError> {
        // Keep this descriptor open as long as SQLite holds POSIX locks on the inode.
        let source_file = File::open(&self.path)?;
        let source = open_existing(&self.path)?;
        require_cipher(&source)?;
        configure(&source)?;
        // Recover a hot journal and hold the SQLite lock until this connection closes.
        // Checkpoint a legacy WAL without changing the plaintext database's journal mode.
        source.pragma_update(None, "journal_size_limit", 0)?;
        source.execute_batch("BEGIN EXCLUSIVE; COMMIT;")?;
        checkpoint_legacy_wal(&source)?;
        let version: i64 = source.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !(1..=SCHEMA_VERSION).contains(&version) {
            return Err(ProfileError::UnsupportedSchema);
        }
        self.check_backup()?;
        self.create_stage()?;

        let raw = key.sqlcipher_literal();
        source.execute(
            "ATTACH DATABASE CAST(?1 AS TEXT) AS encrypted KEY ?2",
            params![self.stage.as_os_str().as_encoded_bytes(), &raw[..]],
        )?;
        drop(raw);
        source.execute_batch("PRAGMA encrypted.synchronous = EXTRA;")?;
        export(&source, version)?;
        source.execute_batch("DETACH DATABASE encrypted;")?;

        // Exercise the ordinary schema upgrades and interrupted-operation recovery before
        // replacing the active pathname. The plaintext original stays at its original schema.
        self.verify_stage(key)?;
        drop(initialize(keyed_connection(&self.stage, key)?)?);
        self.verify_stage(key)?;

        source_file.sync_all()?;
        // The source is standalone after recovery and the WAL checkpoint. Its remaining
        // sidecars must not be associated with the new inode under the active filename.
        for suffix in SIDECARS {
            let path = suffixed(&self.path, suffix);
            if let Some(metadata) = regular_file(&path)? {
                if suffix == "-wal" && metadata.len() != 0 {
                    return Err(ProfileError::UnexpectedFiles);
                }
                if suffix == "-journal" && metadata.len() != 0 {
                    // An old exclusive-mode connection can leave an invalidated journal.
                    // SQLite's hot-journal check treats a zero first byte as inactive.
                    let mut first = [0];
                    File::open(&path)?.read_exact(&mut first)?;
                    if first[0] != 0 {
                        return Err(ProfileError::UnexpectedFiles);
                    }
                }
                fs::remove_file(path)?;
            }
        }
        if regular_file(&self.backup)?.is_none() {
            fs::hard_link(&self.path, &self.backup)?;
        }
        sync_directory(&self.path)?;
        self.activate_stage()?;
        drop(source);
        Ok(())
    }

    fn check_backup(&self) -> Result<(), ProfileError> {
        if let Some(backup) = regular_file(&self.backup)? {
            // After a crash between the backup link and rename, both names refer to
            // the same inode. Never overwrite an independent copy.
            let active = self.path.metadata()?;
            #[cfg(unix)]
            if (backup.dev(), backup.ino()) == (active.dev(), active.ino()) {
                return Ok(());
            }
            #[cfg(not(unix))]
            let _ = (backup, active);
            return Err(ProfileError::UnexpectedFiles);
        }
        Ok(())
    }

    fn create_stage(&self) -> Result<(), ProfileError> {
        for path in SIDECARS
            .iter()
            .map(|suffix| suffixed(&self.stage, suffix))
            .chain(std::iter::once(self.stage.clone()))
        {
            if regular_file(&path)?.is_some() {
                fs::remove_file(path)?;
            }
        }
        private_options().create_new(true).open(&self.stage)?;
        Ok(())
    }

    fn verify_stage(&self, key: &DatabaseKey) -> Result<(), ProfileError> {
        let mut header = [0; 16];
        File::open(&self.stage)?.read_exact(&mut header)?;
        if header == SQLITE_HEADER {
            return Err(ProfileError::Validation);
        }
        validate(&keyed_connection(&self.stage, key)?)
    }

    fn activate_stage(&self) -> Result<(), ProfileError> {
        if has_sidecars(&self.stage)? {
            return Err(ProfileError::UnexpectedFiles);
        }
        File::open(&self.stage)?.sync_all()?;
        fs::rename(&self.stage, &self.path)?;
        sync_directory(&self.path)
    }
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
}

fn regular_file(path: &Path) -> Result<Option<fs::Metadata>, ProfileError> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(_) => Err(ProfileError::UnexpectedFiles),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn has_sidecars(path: &Path) -> Result<bool, ProfileError> {
    let mut present = false;
    for suffix in SIDECARS {
        present |= regular_file(&suffixed(path, suffix))?.is_some();
    }
    Ok(present)
}

fn sync_directory(path: &Path) -> Result<(), ProfileError> {
    File::open(path.parent().ok_or(ProfileError::UnexpectedFiles)?)?.sync_all()?;
    Ok(())
}

fn open_existing(path: &Path) -> Result<Connection, ProfileError> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?)
}

fn require_cipher(connection: &Connection) -> Result<(), ProfileError> {
    let version: String = connection
        .pragma_query_value(None, "cipher_version", |row| row.get(0))
        .map_err(|_| ProfileError::CipherUnavailable)?;
    if version.is_empty() {
        return Err(ProfileError::CipherUnavailable);
    }
    connection.execute_batch(
        "PRAGMA cipher_log_source = NONE;
         PRAGMA cipher_memory_security = ON;",
    )?;
    Ok(())
}

fn configure(connection: &Connection) -> Result<(), ProfileError> {
    connection.busy_timeout(Duration::from_millis(250))?;
    connection.execute_batch(
        "PRAGMA temp_store = MEMORY;
         PRAGMA locking_mode = EXCLUSIVE;
         PRAGMA synchronous = EXTRA;",
    )?;
    Ok(())
}

fn keyed_connection(path: &Path, key: &DatabaseKey) -> Result<Connection, ProfileError> {
    let connection = open_existing(path)?;
    require_cipher(&connection)?;
    connection.busy_timeout(Duration::from_millis(250))?;
    let raw = key.sqlcipher_literal();
    // SAFETY: The handle belongs to this live connection. SQLCipher copies the supplied
    // raw-key expression during the call and we clear the Rust buffer immediately after.
    let code =
        unsafe { ffi::sqlite3_key(connection.handle(), raw.as_ptr().cast(), raw.len() as i32) };
    drop(raw);
    if code != ffi::SQLITE_OK {
        return Err(ProfileError::CipherUnavailable);
    }
    // Setting the key does not authenticate it until a page is read.
    connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(|error| {
            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::NotADatabase) {
                ProfileError::KeyRejected
            } else {
                error.into()
            }
        })?;
    configure(&connection)?;
    Ok(connection)
}

fn checkpoint_legacy_wal(connection: &Connection) -> Result<(), ProfileError> {
    let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if mode == "wal" {
        let (busy, pages): (i64, i64) =
            connection.query_row("PRAGMA main.wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        if busy != 0 || pages > 0 {
            return Err(ProfileError::Busy);
        }
    }
    Ok(())
}

fn export(source: &Connection, version: i64) -> Result<(), ProfileError> {
    let application_id: i64 =
        source.pragma_query_value(None, "application_id", |row| row.get(0))?;
    source
        .query_row("SELECT sqlcipher_export('encrypted')", [], |_| Ok(()))
        .map_err(|error| match &error {
            // SQLCipher wraps some nested SQLite failures in SQLITE_ERROR.
            rusqlite::Error::SqliteFailure(_, Some(message))
                if message == "database or disk is full" =>
            {
                ProfileError::DiskFull
            }
            _ => error.into(),
        })?;
    // sqlcipher_export copies schema and rows but omits these header fields.
    source.execute_batch(&format!(
        "PRAGMA encrypted.user_version = {version};
         PRAGMA encrypted.application_id = {application_id};"
    ))?;
    Ok(())
}

fn validate(connection: &Connection) -> Result<(), ProfileError> {
    if connection
        .prepare("PRAGMA cipher_integrity_check")?
        .exists([])?
        || connection.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?
            != "ok"
        || connection.prepare("PRAGMA foreign_key_check")?.exists([])?
    {
        return Err(ProfileError::Validation);
    }
    Ok(())
}

fn initialize(connection: Connection) -> Result<SqliteStore, ProfileError> {
    SqliteStore::initialize(connection).map_err(ProfileError::Initialize)
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
