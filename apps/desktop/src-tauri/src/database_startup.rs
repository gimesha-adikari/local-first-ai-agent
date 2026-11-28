//! Native encrypted startup and recovery. The webview is created only after
//! the profile is ready. The sole recovery IPC command requests a GTK dialog.
use std::{
    cell::RefCell,
    ffi::CStr,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
};

use engine::credentials::CredentialError;
use gtk::{
    glib::{self, translate::ToGlibPtr},
    prelude::*,
};
use sha2::{Digest, Sha256};
use storage::{
    SqliteStore,
    profile::{DatabaseKey, Profile, ProfileError, ProfileKind},
};
use tauri::{AppHandle, Manager};
use zeroize::Zeroizing;

use crate::database_keyring::{DatabaseKeyring, LinuxKeyring};

const RECOVERY_WARNING: &str = "If you lose both the system keyring entry and this recovery key, your database may be permanently inaccessible. Keep the key in a password manager or another secure place, separate from the database.";

pub struct DatabaseAccess {
    path: PathBuf,
    backup: PathBuf,
    // Confirm that a subsequent keyring read still corresponds to the open database.
    fingerprint: Zeroizing<[u8; 32]>,
    keys: Arc<dyn DatabaseKeyring>,
    recovery_dialog: Mutex<()>,
}

impl DatabaseAccess {
    pub fn plaintext_backup(&self) -> Option<String> {
        self.backup
            .symlink_metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|_| self.backup.to_string_lossy().into_owned())
    }

    fn recovery_key(&self) -> Result<DatabaseKey, StartupError> {
        let key = self.keys.read(&self.path)?;
        if fingerprint(&key) != self.fingerprint {
            return Err(StartupError::Key(CredentialError::Invalid));
        }
        Ok(key)
    }
}

#[derive(Debug)]
enum StartupError {
    Key(CredentialError),
    Database(ProfileError),
}

impl From<CredentialError> for StartupError {
    fn from(error: CredentialError) -> Self {
        Self::Key(error)
    }
}

impl From<ProfileError> for StartupError {
    fn from(error: ProfileError) -> Self {
        Self::Database(error)
    }
}

impl StartupError {
    fn message(&self) -> &'static str {
        match self {
            Self::Key(CredentialError::Missing) => {
                "The database key is missing from your system keyring."
            }
            Self::Key(CredentialError::Invalid) => {
                "The database key in your system keyring is invalid or there are duplicate entries."
            }
            Self::Key(CredentialError::Locked) => {
                "Your system keyring is locked. Unlock it in your desktop's keyring app, then try again."
            }
            Self::Key(_) => {
                "Your system keyring is unavailable. Make sure your desktop has an unlocked, persistent Secret Service keyring and try again."
            }
            Self::Database(ProfileError::KeyRejected) => {
                "The key could not unlock this database. Use its recovery key if you have it."
            }
            Self::Database(ProfileError::Busy) => {
                "Another instance is using this database. Close it and try again."
            }
            Self::Database(ProfileError::DiskFull) => {
                "The database disk is full. Free disk space and try again."
            }
            Self::Database(ProfileError::MissingActiveDatabase) => {
                "The active database is missing. Keep the remaining backup and database files while you restore the profile."
            }
            Self::Database(ProfileError::UnsupportedSchema) => {
                "This database uses an unsupported schema version. Open it with a compatible version of the app."
            }
            Self::Database(ProfileError::CipherUnavailable) => {
                "SQLCipher is unavailable. Reinstall an application build with encrypted storage."
            }
            Self::Database(ProfileError::Validation) => {
                "The database failed its integrity check. Keep all profile files for recovery."
            }
            Self::Database(_) => {
                "The encrypted database could not be prepared. Check disk space and permissions, and keep the existing profile files."
            }
        }
    }

    fn allows_recovery(&self) -> bool {
        matches!(
            self,
            Self::Key(CredentialError::Missing | CredentialError::Invalid)
                | Self::Database(ProfileError::KeyRejected)
        )
    }
}

fn fingerprint(key: &DatabaseKey) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(Sha256::digest(key.as_bytes()).into())
}

fn save_and_confirm(
    path: &Path,
    keys: &dyn DatabaseKeyring,
    key: &DatabaseKey,
) -> Result<(), StartupError> {
    keys.save(path, key)?;
    if keys.read(path)?.as_bytes() != key.as_bytes() {
        return Err(CredentialError::Invalid.into());
    }
    Ok(())
}

fn open(
    profile: Profile,
    keys: Arc<dyn DatabaseKeyring>,
) -> Result<(SqliteStore, DatabaseAccess), StartupError> {
    let kind = profile.kind()?;
    let key = match keys.read(profile.path()) {
        Err(CredentialError::Missing)
            if matches!(kind, ProfileKind::New | ProfileKind::Plaintext) =>
        {
            let key = DatabaseKey::generate()?;
            save_and_confirm(profile.path(), &*keys, &key)?;
            key
        }
        other => other?,
    };
    let access = DatabaseAccess {
        path: profile.path().to_owned(),
        backup: profile.backup_path().to_owned(),
        fingerprint: fingerprint(&key),
        keys,
        recovery_dialog: Mutex::new(()),
    };
    Ok((profile.open(&key)?, access))
}

fn restore(
    profile: &Profile,
    keys: &dyn DatabaseKeyring,
    key: &DatabaseKey,
) -> Result<(), StartupError> {
    profile.validate_recovery_key(key)?;
    save_and_confirm(profile.path(), keys, key)
}

pub fn open_native(path: &Path) -> Option<(SqliteStore, DatabaseAccess)> {
    let keys: Arc<dyn DatabaseKeyring> = Arc::new(LinuxKeyring);
    loop {
        let (kind, result) = match Profile::claim(path)
            .and_then(|profile| Ok((profile.kind()?, profile)))
        {
            Ok((kind, profile)) => {
                let worker_keys = Arc::clone(&keys);
                let work = move || open(profile, worker_keys);
                let result = match kind {
                    ProfileKind::Plaintext => with_progress(
                        "Encrypting your database",
                        "Your existing data is being copied and verified. The original will be kept as a plaintext backup. Its location will appear in Privacy & Storage.",
                        work,
                    ),
                    ProfileKind::New | ProfileKind::InterruptedSetup => with_progress(
                        "Setting up encrypted storage",
                        "Preparing the local database and its system keyring entry.",
                        work,
                    ),
                    ProfileKind::Encrypted => work(),
                };
                (Some(kind), result)
            }
            Err(error) => (None, Err(error.into())),
        };
        let error = match result {
            Ok(opened) => return Some(opened),
            Err(error) => error,
        };
        let title = match kind {
            Some(ProfileKind::Plaintext) => "Database migration could not finish",
            Some(ProfileKind::New | ProfileKind::InterruptedSetup) => {
                "Encrypted storage setup could not finish"
            }
            _ => "Database could not open",
        };
        let allow_recovery = kind == Some(ProfileKind::Encrypted) && error.allows_recovery();
        match startup_choice(title, error.message(), allow_recovery) {
            gtk::ResponseType::Accept => {}
            gtk::ResponseType::Apply => restore_native(path, Arc::clone(&keys)),
            _ => return None,
        }
    }
}

fn with_progress<T: Send + 'static>(
    title: &str,
    explanation: &str,
    work: impl FnOnce() -> Result<T, StartupError> + Send + 'static,
) -> Result<T, StartupError> {
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Info,
        gtk::ButtonsType::None,
        title,
    );
    dialog.set_title(title);
    dialog.set_secondary_text(Some(explanation));
    dialog.set_deletable(false);
    let prevent_close = dialog.connect_delete_event(|_, _| glib::Propagation::Stop);
    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);
    let progress_loop = glib::MainLoop::new(None, false);
    let completion = progress_loop.clone();
    glib::MainContext::default().spawn_local(async move {
        *response.borrow_mut() = gtk::gio::spawn_blocking(work).await.ok();
        completion.quit();
    });
    dialog.show_all();
    progress_loop.run();
    dialog.disconnect(prevent_close);
    dialog.close();
    result
        .take()
        .unwrap_or(Err(CredentialError::Unavailable.into()))
}

fn startup_choice(title: &str, message: &str, allow_recovery: bool) -> gtk::ResponseType {
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::None,
        title,
    );
    dialog.set_title(title);
    dialog.set_secondary_text(Some(message));
    dialog.add_button("Quit", gtk::ResponseType::Cancel);
    if allow_recovery {
        dialog.add_button("Enter recovery key…", gtk::ResponseType::Apply);
    }
    dialog.add_button("Try again", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Accept);
    let response = dialog.run();
    dialog.close();
    response
}

fn key_dialog(parent: Option<&gtk::ApplicationWindow>, title: &str) -> gtk::Dialog {
    let dialog = gtk::Dialog::with_buttons(
        Some(title),
        parent,
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[("Close", gtk::ResponseType::Close)],
    );
    dialog.set_resizable(false);
    let content = dialog.content_area();
    content.set_border_width(16);
    content.set_spacing(10);
    let label = gtk::Label::new(Some(RECOVERY_WARNING));
    label.set_line_wrap(true);
    label.set_max_width_chars(54);
    label.set_xalign(0.0);
    content.add(&label);
    dialog
}

fn restore_native(path: &Path, keys: Arc<dyn DatabaseKeyring>) {
    let dialog = key_dialog(None, "Recover encrypted storage");
    dialog.add_button("Unlock and save to keyring", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Accept);
    let entry = gtk::Entry::new();
    entry.set_width_chars(44);
    entry.set_max_length(128);
    entry.set_visibility(false);
    entry.set_input_purpose(gtk::InputPurpose::Password);
    entry.set_activates_default(true);
    entry.set_placeholder_text(Some("64-character recovery key"));
    dialog.content_area().add(&entry);
    dialog.show_all();
    let response = dialog.run();
    let key = if response == gtk::ResponseType::Accept {
        // SAFETY: GTK owns the NUL-terminated text until the entry is changed.
        // Read it synchronously to avoid making another allocation of the key.
        let text = unsafe { CStr::from_ptr(gtk::ffi::gtk_entry_get_text(entry.to_glib_none().0)) };
        text.to_str().ok().and_then(|text| decode_key(text.trim()))
    } else {
        None
    };
    entry.set_text("");
    dialog.close();
    if response != gtk::ResponseType::Accept {
        return;
    }

    let result = match key {
        Some(key) => {
            let path = path.to_owned();
            with_progress(
                "Validating recovery key",
                "Checking the encrypted database before saving the key to your keyring.",
                move || restore(&Profile::claim(&path)?, &*keys, &key),
            )
        }
        None => Err(CredentialError::Invalid.into()),
    };
    if let Err(error) = result {
        let message = match error {
            StartupError::Key(CredentialError::Invalid) => {
                "Use the 64 hexadecimal characters from your saved recovery key. If the keyring contains duplicate entries, resolve them in your system keyring."
            }
            StartupError::Database(ProfileError::KeyRejected) => {
                "This recovery key could not unlock the database."
            }
            _ => error.message(),
        };
        show_message::<gtk::Window>(
            None,
            gtk::MessageType::Error,
            "Recovery could not finish",
            message,
        );
    }
}

fn encode_key(key: &DatabaseKey) -> Zeroizing<[u8; 65]> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = Zeroizing::new([0; 65]);
    for (pair, byte) in encoded[..64]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(key.as_bytes())
    {
        pair[0] = HEX[(byte >> 4) as usize];
        pair[1] = HEX[(byte & 15) as usize];
    }
    encoded
}

fn decode_key(text: &str) -> Option<DatabaseKey> {
    let mut key = Zeroizing::new([0; 32]);
    if text.len() != key.len() * 2 {
        return None;
    }
    for (output, pair) in key.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        *output =
            ((pair[0] as char).to_digit(16)? as u8) << 4 | (pair[1] as char).to_digit(16)? as u8;
    }
    DatabaseKey::from_bytes(key.as_ref())
}

fn show_message<W: IsA<gtk::Window>>(
    parent: Option<&W>,
    message_type: gtk::MessageType,
    title: &str,
    text: &str,
) {
    let dialog = gtk::MessageDialog::new(
        parent,
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        message_type,
        gtk::ButtonsType::Close,
        title,
    );
    dialog.set_title(title);
    dialog.set_secondary_text(Some(text));
    dialog.run();
    dialog.close();
}

#[tauri::command]
pub async fn database_recovery_key(app: AppHandle) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let Some(parent) = handle
            .get_webview_window("main")
            .and_then(|window| window.gtk_window().ok())
        else {
            return;
        };
        let access = handle.state::<DatabaseAccess>();
        let Ok(_dialog) = access.recovery_dialog.try_lock() else {
            return;
        };
        let confirm = gtk::MessageDialog::new(
            Some(&parent),
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            gtk::MessageType::Warning,
            gtk::ButtonsType::Cancel,
            "Reveal the database recovery key?",
        );
        confirm.set_title("Database recovery");
        confirm.set_secondary_text(Some(RECOVERY_WARNING));
        confirm.add_button("Reveal recovery key", gtk::ResponseType::Accept);
        confirm.set_default_response(gtk::ResponseType::Cancel);
        let response = confirm.run();
        confirm.close();
        if response != gtk::ResponseType::Accept {
            return;
        }
        match access.recovery_key() {
            Ok(key) => {
                let dialog = key_dialog(Some(&parent), "Database recovery key");
                let label = gtk::Label::new(None);
                let font = gtk::pango::AttrList::new();
                font.insert(gtk::pango::AttrFontDesc::new(
                    &gtk::pango::FontDescription::from_string("monospace"),
                ));
                label.set_attributes(Some(&font));
                dialog.content_area().add(&label);
                let text = encode_key(&key);
                // SAFETY: the buffer is NUL-terminated and stays alive until GTK copies it.
                unsafe {
                    gtk::ffi::gtk_label_set_text(label.to_glib_none().0, text.as_ptr().cast())
                };
                drop(text);
                drop(key);
                dialog.show_all();
                dialog.run();
                label.set_text("");
                dialog.close();
            }
            Err(error) => show_message(
                Some(&parent),
                gtk::MessageType::Error,
                "Recovery key unavailable",
                error.message(),
            ),
        }
    })
    .map_err(|_| "The native recovery dialog could not open.".to_owned())
}

#[cfg(test)]
#[derive(Default)]
struct FixtureKeyring(Mutex<Option<DatabaseKey>>);

#[cfg(test)]
impl DatabaseKeyring for FixtureKeyring {
    fn read(&self, _path: &Path) -> Result<DatabaseKey, CredentialError> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|key| DatabaseKey::from_bytes(key.as_bytes()))
            .ok_or(CredentialError::Missing)
    }

    fn save(&self, _path: &Path, key: &DatabaseKey) -> Result<(), CredentialError> {
        *self.0.lock().unwrap() = DatabaseKey::from_bytes(key.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
pub fn open_fixture(path: &Path) -> Option<(SqliteStore, DatabaseAccess)> {
    let keys = FixtureKeyring::default();
    // Desktop fixtures run under a disposable profile and can restart without a host keyring.
    keys.save(path, &DatabaseKey::from_bytes(&[42; 32]).unwrap())
        .unwrap();
    Some(open(Profile::claim(path).unwrap(), Arc::new(keys)).unwrap())
}

#[cfg(test)]
#[path = "database_startup_tests.rs"]
mod tests;
