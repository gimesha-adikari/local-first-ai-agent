use std::fs;

use agent_core::chat::ChatStore;

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "agent-database-startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn path(&self) -> PathBuf {
        self.0.join("chat/test.sqlite3")
    }

    fn claim(&self) -> Profile {
        Profile::claim(&self.path()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn absent_key_never_replaces_an_existing_encrypted_database() {
    let fixture = Fixture::new();
    let original = Arc::new(FixtureKeyring::default());
    drop(open(fixture.claim(), original).unwrap());
    let before = fs::read(fixture.path()).unwrap();

    let missing = Arc::new(FixtureKeyring::default());
    assert!(matches!(
        open(fixture.claim(), missing.clone()),
        Err(StartupError::Key(CredentialError::Missing))
    ));
    assert!(matches!(
        missing.read(&fixture.path()),
        Err(CredentialError::Missing)
    ));
    assert_eq!(fs::read(fixture.path()).unwrap(), before);
}

#[test]
fn unavailable_or_locked_keyring_stops_setup_and_migration() {
    struct UnusableKeyring(CredentialError);

    impl DatabaseKeyring for UnusableKeyring {
        fn read(&self, _path: &Path) -> Result<DatabaseKey, CredentialError> {
            Err(self.0)
        }

        fn save(&self, _path: &Path, _key: &DatabaseKey) -> Result<(), CredentialError> {
            panic!("must not try to replace a key when the keyring cannot be read");
        }
    }

    let fixture = Fixture::new();
    for error in [
        CredentialError::Unavailable,
        CredentialError::Locked,
        CredentialError::Invalid,
    ] {
        assert!(matches!(
            open(fixture.claim(), Arc::new(UnusableKeyring(error))),
            Err(StartupError::Key(actual)) if actual == error
        ));
        assert!(!fixture.path().exists());
    }

    let profile = fixture.claim();
    drop(SqliteStore::open_test_fixture(profile.path()).unwrap());
    let original = fs::read(profile.path()).unwrap();
    assert!(matches!(
        open(
            profile,
            Arc::new(UnusableKeyring(CredentialError::Unavailable))
        ),
        Err(StartupError::Key(CredentialError::Unavailable))
    ));
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
}

#[test]
fn setup_requires_confirmed_keyring_storage_before_writing_a_database() {
    struct UnconfirmedKeyring;

    impl DatabaseKeyring for UnconfirmedKeyring {
        fn read(&self, _path: &Path) -> Result<DatabaseKey, CredentialError> {
            Err(CredentialError::Missing)
        }

        fn save(&self, _path: &Path, _key: &DatabaseKey) -> Result<(), CredentialError> {
            Ok(())
        }
    }

    let fixture = Fixture::new();
    assert!(matches!(
        open(fixture.claim(), Arc::new(UnconfirmedKeyring)),
        Err(StartupError::Key(CredentialError::Missing))
    ));
    assert!(!fixture.path().exists());
}

#[test]
fn recovery_input_is_validated_before_replacing_a_missing_or_incorrect_key() {
    let fixture = Fixture::new();
    let keys = Arc::new(FixtureKeyring::default());
    let (mut db, access) = open(fixture.claim(), keys.clone()).unwrap();
    let chat = db.create().unwrap().id;
    db.rename(chat, "Synthetic recovery chat").unwrap();

    let key = access.recovery_key().unwrap();
    let encoding = encode_key(&key);
    assert!(decode_key("unknown").is_none());
    let decoded = decode_key(std::str::from_utf8(&encoding[..64]).unwrap()).unwrap();

    let wrong = DatabaseKey::generate().unwrap();
    keys.save(&fixture.path(), &wrong).unwrap();
    assert!(matches!(
        access.recovery_key(),
        Err(StartupError::Key(CredentialError::Invalid))
    ));
    drop(db);
    assert!(matches!(
        open(fixture.claim(), keys.clone()),
        Err(StartupError::Database(ProfileError::KeyRejected))
    ));

    let profile = fixture.claim();
    assert!(matches!(
        restore(&profile, &*keys, &wrong),
        Err(StartupError::Database(ProfileError::KeyRejected))
    ));
    restore(&profile, &*keys, &decoded).unwrap();
    assert_eq!(
        open(profile, keys).unwrap().0.get(chat).unwrap().title,
        "Synthetic recovery chat"
    );
}

#[test]
#[ignore = "requires an isolated D-Bus session with a disposable persistent default keyring"]
fn database_keyring_and_encrypted_profile_round_trip() {
    assert_eq!(std::env::var("AGENT_ISOLATED_KEYRING").as_deref(), Ok("1"));
    let keys = Arc::new(LinuxKeyring);
    let fixture = Fixture::new();
    let another = Fixture::new();
    let (mut db, access) = open(fixture.claim(), keys.clone()).unwrap();
    let id = db.create().unwrap().id;
    db.rename(id, "Synthetic keyring conversation").unwrap();
    drop(db);
    assert!(matches!(
        keys.read(&another.path()),
        Err(CredentialError::Missing)
    ));
    assert!(access.recovery_key().is_ok());

    let (db, _) = open(fixture.claim(), keys.clone()).unwrap();
    assert_eq!(db.get(id).unwrap().title, "Synthetic keyring conversation");
    drop(db);

    let recovery = access.recovery_key().unwrap();
    keys.save(&fixture.path(), &DatabaseKey::generate().unwrap())
        .unwrap();
    assert!(matches!(
        open(fixture.claim(), keys.clone()),
        Err(StartupError::Database(ProfileError::KeyRejected))
    ));
    let profile = fixture.claim();
    restore(&profile, &*keys, &recovery).unwrap();
    assert_eq!(
        open(profile, keys).unwrap().0.get(id).unwrap().title,
        "Synthetic keyring conversation"
    );
}
