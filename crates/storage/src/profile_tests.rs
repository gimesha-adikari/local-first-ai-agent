use agent_core::{
    chat::{ChatStore, GenerationState, TurnAction},
    knowledge::{DocumentFormat, append_chunks},
};

use crate::knowledge::{DocumentImport, KnowledgeSearch};

use super::*;

const PERSONAL_TEXT: &str = "SyntheticPersonalAlpineMarker";

fn key() -> DatabaseKey {
    DatabaseKey::from_bytes(&[63; 32]).unwrap()
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let bytes = getrandom::u64().unwrap();
        let path = std::env::temp_dir().join(format!("agent-encryption-{bytes:016x}/chat/profile"));
        Self(path)
    }

    fn claim(&self) -> Profile {
        Profile::claim(&self.0).unwrap()
    }

    fn open(&self) -> SqliteStore {
        self.claim().open(&key()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap().parent().unwrap());
    }
}

fn assert_ciphertext(path: &Path) {
    let bytes = fs::read(path).unwrap();
    assert!(
        !bytes.starts_with(SQLITE_HEADER),
        "{} is plaintext",
        path.display()
    );
    assert!(
        !bytes
            .windows(PERSONAL_TEXT.len())
            .any(|window| window == PERSONAL_TEXT.as_bytes()),
        "{} contains the plaintext marker",
        path.display()
    );
}

#[test]
fn opens_encrypted_profile_and_rejects_wrong_or_absent_key() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let id = store.create().unwrap().id;
    store.rename(id, PERSONAL_TEXT).unwrap();
    assert_ciphertext(&fixture.0);
    assert!(matches!(
        Profile::claim(&fixture.0),
        Err(ProfileError::Busy)
    ));
    drop(store);

    let profile = fixture.claim();
    let unkeyed = open_existing(profile.path()).unwrap();
    assert!(
        unkeyed
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .is_err()
    );
    drop(unkeyed);
    assert!(matches!(
        profile.open(&DatabaseKey::generate().unwrap()),
        Err(ProfileError::KeyRejected)
    ));
    assert_eq!(fixture.open().get(id).unwrap().title, PERSONAL_TEXT);
}

#[test]
fn authenticates_recovery_against_the_existing_database() {
    let fixture = Fixture::new();
    drop(fixture.open());
    let profile = fixture.claim();
    assert!(matches!(
        profile.validate_recovery_key(&DatabaseKey::generate().unwrap()),
        Err(ProfileError::KeyRejected)
    ));
    profile.validate_recovery_key(&key()).unwrap();
}

#[test]
fn encrypts_rollback_journal_and_wal_frames_with_memory_temporary_storage() {
    let fixture = Fixture::new();
    drop(fixture.open());
    for mode in ["DELETE", "WAL"] {
        let connection = keyed_connection(&fixture.0, &key()).unwrap();
        let temp_store: i64 = connection
            .pragma_query_value(None, "temp_store", |r| r.get(0))
            .unwrap();
        assert_eq!(temp_store, 2);
        connection
            .pragma_update(None, "journal_mode", mode)
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS test_contents(text); DELETE FROM test_contents;",
            )
            .unwrap();
        connection
            .execute("INSERT INTO test_contents VALUES (?1)", [PERSONAL_TEXT])
            .unwrap();
        connection
            .execute_batch("BEGIN EXCLUSIVE; UPDATE test_contents SET text = 'new value';")
            .unwrap();
        let suffix = if mode == "WAL" { "-wal" } else { "-journal" };
        let sidecar = suffixed(&fixture.0, suffix);
        assert!(sidecar.exists());
        assert_ciphertext(&sidecar);
        assert_ciphertext(&fixture.0);
        connection.execute_batch("ROLLBACK;").unwrap();
    }
}

#[test]
fn migrates_schema_fifteen_and_retries_after_backup_link_without_cutover() {
    let fixture = Fixture::new();
    let profile = fixture.claim();
    let mut legacy = SqliteStore::open_test_fixture(profile.path()).unwrap();
    legacy
        .connection
        .execute_batch("PRAGMA application_id=912;")
        .unwrap();
    let conversation = legacy.create().unwrap().id;
    let turn = legacy
        .begin_chat(conversation, TurnAction::Send(PERSONAL_TEXT.into()), None)
        .unwrap();
    let memory = legacy.create_memory(PERSONAL_TEXT).unwrap().id;
    let mut chunks = Vec::new();
    append_chunks(&mut chunks, PERSONAL_TEXT, None, || Ok(())).unwrap();
    legacy
        .save_knowledge(
            &DocumentImport {
                name: "alpine.txt",
                format: DocumentFormat::Txt,
                fingerprint: &[36; 32],
                file_bytes: PERSONAL_TEXT.len(),
                pdf_coverage: None,
                chunks: &chunks,
            },
            None,
            |_| Ok(()),
        )
        .unwrap();
    legacy.connection.execute_batch(
        "INSERT INTO providers (id,key,settings,created_at,updated_at) VALUES (84,'synthetic-provider','{}',1,1);
         UPDATE intelligence_settings SET active_provider=84;
         INSERT INTO provider_usage (id,provider_id,message_id,period,created_at,state,reserved_tokens)
            SELECT 60,84,id,'2026-11',1,'settled',1 FROM messages WHERE role='assistant';
         INSERT INTO intelligence_receipts (id,message_id,receipt)
            SELECT 75,id,'{\"synthetic\":true}' FROM messages WHERE role='assistant';
         UPDATE sqlite_sequence SET seq=99 WHERE name='conversations';",
    ).unwrap();
    drop(legacy);
    // SQLite may also leave a zeroed header on a legacy exclusive-mode journal.
    let mut legacy_journal = vec![0; 28];
    legacy_journal.extend_from_slice(PERSONAL_TEXT.as_bytes());
    fs::write(suffixed(profile.path(), "-journal"), legacy_journal).unwrap();
    // Model the durable files left immediately before the atomic rename.
    fs::hard_link(profile.path(), profile.backup_path()).unwrap();
    fs::write(&profile.stage, b"incomplete staged file").unwrap();
    fs::write(suffixed(&profile.stage, "-journal"), b"incomplete journal").unwrap();
    let plaintext_before = fs::read(profile.path()).unwrap();

    let mut store = profile.open(&key()).unwrap();
    assert_ciphertext(&fixture.0);
    assert_eq!(
        fs::read(suffixed(&fixture.0, ".plaintext-backup")).unwrap(),
        plaintext_before
    );
    assert!(!suffixed(&fixture.0, ".encrypting").exists());
    assert!(!suffixed(&fixture.0, "-journal").exists());

    let snapshot = store.get(conversation).unwrap();
    assert_eq!(snapshot.messages[0].content, PERSONAL_TEXT);
    assert_eq!(snapshot.messages[1].id, turn.assistant.id);
    assert_eq!(snapshot.messages[1].state, GenerationState::Interrupted);
    assert_eq!(store.list_memories("", None, 10).unwrap()[0].id, memory);
    let hits = store
        .search_knowledge(&KnowledgeSearch {
            query: PERSONAL_TEXT.into(),
            document_ids: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(hits[0].document_id, 1);
    let usage: (i64, i64, i64) = store.connection.query_row(
        "SELECT provider_usage.provider_id, intelligence_receipts.message_id, intelligence_receipts.id
         FROM provider_usage JOIN intelligence_receipts USING (message_id)", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(usage, (84, turn.assistant.id, 75));
    let application_id: i64 = store
        .connection
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    assert_eq!(application_id, 912);
    assert_eq!(store.create().unwrap().id, 100);
}

#[test]
fn failed_validation_leaves_the_legacy_database_active() {
    let fixture = Fixture::new();
    let profile = fixture.claim();
    let legacy = SqliteStore::open_test_fixture(profile.path()).unwrap();
    legacy
        .connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
         INSERT INTO intelligence_receipts (id,message_id,receipt) VALUES (75,999,'{}');",
        )
        .unwrap();
    drop(legacy);
    let original = fs::read(profile.path()).unwrap();
    assert!(matches!(
        profile.open(&key()),
        Err(ProfileError::Validation)
    ));
    let profile = fixture.claim();
    assert_eq!(profile.kind().unwrap(), ProfileKind::Plaintext);
    assert_eq!(fs::read(&fixture.0).unwrap(), original);
    assert!(!profile.backup_path().exists());
    assert_ciphertext(&profile.stage);
}

#[test]
fn migrates_a_legacy_wal_without_losing_committed_frames() {
    let fixture = Fixture::new();
    let profile = fixture.claim();
    drop(SqliteStore::open_test_fixture(profile.path()).unwrap());
    let legacy = open_existing(profile.path()).unwrap();
    legacy.pragma_update(None, "journal_mode", "WAL").unwrap();
    // Keep WAL files on close to model a committed transaction in a crashed session.
    let mut persist = 1i32;
    // SAFETY: SQLite expects a pointer to a live int and the main handle here.
    let result = unsafe {
        ffi::sqlite3_file_control(
            legacy.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut persist as *mut i32).cast(),
        )
    };
    assert_eq!(result, ffi::SQLITE_OK);
    legacy
        .execute(
            "INSERT INTO conversations (id,title,created_at,updated_at) VALUES (12,?1,1,1)",
            [PERSONAL_TEXT],
        )
        .unwrap();
    drop(legacy);
    assert!(has_sidecars(profile.path()).unwrap());
    let store = profile.open(&key()).unwrap();
    assert_eq!(store.get(12).unwrap().title, PERSONAL_TEXT);
    assert!(!has_sidecars(&fixture.0).unwrap());
}

#[test]
fn a_full_migration_target_can_be_discarded_and_retried() {
    let fixture = Fixture::new();
    let profile = fixture.claim();
    let mut legacy = SqliteStore::open_test_fixture(profile.path()).unwrap();
    let id = legacy.create().unwrap().id;
    legacy.rename(id, PERSONAL_TEXT).unwrap();
    drop(legacy);
    let before = fs::read(profile.path()).unwrap();

    // Limit SQLite's target pages to exercise the same SQLITE_FULL error that
    // an actual full device returns, without consuming space on a real drive.
    let source = open_existing(profile.path()).unwrap();
    configure(&source).unwrap();
    profile.create_stage().unwrap();
    let key = key();
    let raw = key.sqlcipher_literal();
    source
        .execute(
            "ATTACH DATABASE CAST(?1 AS TEXT) AS encrypted KEY ?2",
            params![profile.stage.as_os_str().as_encoded_bytes(), &raw[..]],
        )
        .unwrap();
    source
        .execute_batch("PRAGMA encrypted.max_page_count=1;")
        .unwrap();
    assert!(matches!(
        export(&source, SCHEMA_VERSION),
        Err(ProfileError::DiskFull)
    ));
    drop(source);
    assert_eq!(fs::read(profile.path()).unwrap(), before);
    assert!(!profile.backup_path().exists());
    let store = profile.open(&key).unwrap();
    assert_eq!(store.get(id).unwrap().title, PERSONAL_TEXT);
}
