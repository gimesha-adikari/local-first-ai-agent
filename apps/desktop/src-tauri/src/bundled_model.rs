//! Discovery and integrity verification of the application-bundled model.
//!
//! The GGUF ships as a separate Tauri resource. It is verified against a
//! pinned size and SHA-256 before it is registered, and a missing or
//! corrupted resource never prevents the application from starting.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use agent_core::{
    chat::{ChatStore, ModelId, ModelReference},
    inference::{ErrorCode, InferenceError, Result},
};
use engine::{
    database,
    models::{content_key, recommended_for_model, sha256_hex},
};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::commands::Desktop;

/// Pinned identity of a bundled model resource.
#[derive(Debug, Clone, Copy)]
pub struct BundledSpec {
    /// Stable database key, independent of the install location.
    pub key: &'static str,
    /// Path relative to the Tauri resource directory.
    pub resource: &'static str,
    pub file_name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// LFM2.5-1.2B-Instruct Q4_K_M from Hugging Face `LiquidAI/LFM2.5-1.2B-Instruct-GGUF`
/// at revision `8ed288026e23958ad9dfa92d53ed773a8eee7125`. LFM Open License v1.0.
pub const LFM25: BundledSpec = BundledSpec {
    key: "lfm2.5-1.2b-instruct-q4_k_m",
    resource: "models/LFM2.5-1.2B-Instruct-Q4_K_M.gguf",
    file_name: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf",
    bytes: 730_895_168,
    sha256: "b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5",
};

const VERIFIED_STAMP: &str = "bundled-model.verified";
const LFM25_TRAINED_CONTEXT: u32 = 128_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BundledState {
    /// Verified and registered.
    Ready,
    /// This build or installation does not contain the resource.
    Missing,
    /// The resource exists but failed size or checksum verification.
    Corrupted,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundledModel {
    pub state: BundledState,
    /// The registered row. Present only when verification passed.
    pub model: Option<ModelReference>,
    /// True when no user-imported model exists, so the UI may load it.
    pub auto_load: bool,
    /// The last Chat model loaded successfully, to load at launch. Absent after
    /// an explicit unload.
    pub restore: Option<ModelRestore>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRestore {
    pub model: ModelReference,
    /// False when the saved file is not readable now; nothing is loaded.
    pub available: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verification {
    Verified(PathBuf),
    Missing,
    Corrupted,
}

fn stamp_line(spec: &BundledSpec, path: &Path, metadata: &fs::Metadata) -> Option<String> {
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!(
        "{} {} {} {} {}",
        spec.key,
        spec.sha256,
        metadata.len(),
        modified,
        path.to_str()?
    ))
}

fn write_stamp(path: &Path, line: &str) {
    let temporary = path.with_extension("verified.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // The stamp only avoids rehashing. Failing to save it is harmless.
    let written = options
        .open(&temporary)
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if written.is_err() || fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
    }
}

/// Record sampled identities for listed files that can be read now, compute
/// full digests only for rows whose sampled identity matches another row, then
/// collapse rows with equal full digests. Unreadable files, such as an
/// unmounted drive, stay listed as separate models.
pub fn reconcile_models(
    db: &Mutex<storage::SqliteStore>,
    verified: Option<(ModelId, &str)>,
) -> Result<()> {
    let pending = database(db)?.models_without_content_key()?;
    let keys: Vec<_> = pending
        .into_iter()
        .filter_map(|(id, path)| content_key(&path).ok().map(|key| (id, key)))
        .collect();
    {
        let mut db = database(db)?;
        for (id, key) in keys {
            db.set_model_content_key(id, &key)?;
        }
        // The bundled file was just verified against its pinned digest.
        if let Some((id, sha256)) = verified {
            db.set_model_sha256(id, sha256)?;
        }
    }
    let candidates = database(db)?.duplicate_candidates()?;
    let digests: Vec<_> = candidates
        .into_iter()
        .filter_map(|(id, path)| sha256_hex(&path).ok().map(|digest| (id, digest)))
        .collect();
    let mut db = database(db)?;
    for (id, digest) in digests {
        db.set_model_sha256(id, &digest)?;
    }
    db.merge_duplicate_models()?;
    Ok(())
}

/// Verify the resource by size and SHA-256. A stamp keyed by path, size and
/// modification time avoids rehashing an unchanged file on every launch.
pub fn verify(spec: &BundledSpec, resource_dir: &Path, stamp_dir: &Path) -> Verification {
    let path = resource_dir.join(spec.resource);
    let Ok(metadata) = fs::metadata(&path) else {
        return Verification::Missing;
    };
    if !metadata.is_file() || metadata.len() != spec.bytes {
        return Verification::Corrupted;
    }
    let stamp_path = stamp_dir.join(VERIFIED_STAMP);
    let line = stamp_line(spec, &path, &metadata);
    if line.is_some() && fs::read_to_string(&stamp_path).ok() == line {
        return Verification::Verified(path);
    }
    match sha256_hex(&path) {
        Ok(digest) if digest == spec.sha256 => {
            if let Some(line) = line {
                write_stamp(&stamp_path, &line);
            }
            Verification::Verified(path)
        }
        Ok(_) => {
            let _ = fs::remove_file(&stamp_path);
            Verification::Corrupted
        }
        Err(_) => Verification::Corrupted,
    }
}

pub fn register(
    db: &mut storage::SqliteStore,
    spec: &BundledSpec,
    verification: Verification,
) -> Result<BundledModel> {
    let path = match verification {
        Verification::Verified(path) => path,
        Verification::Missing | Verification::Corrupted => {
            return Ok(BundledModel {
                state: if verification == Verification::Missing {
                    BundledState::Missing
                } else {
                    BundledState::Corrupted
                },
                model: None,
                auto_load: false,
                restore: None,
            });
        }
    };
    // A new row starts with automatic settings. Existing rows keep theirs.
    let mut settings = recommended_for_model(
        LFM25_TRAINED_CONTEXT,
        spec.bytes,
        crate::platform::available_memory(),
    );
    // LFM2.5-Instruct has no switchable Thinking mode.
    settings.enable_thinking = false;
    let registration = db.register_bundled_model(spec.key, &path, spec.file_name, settings)?;
    Ok(BundledModel {
        state: BundledState::Ready,
        model: Some(registration.reference),
        auto_load: registration.only_model,
        restore: None,
    })
}

/// Locate, verify and register the bundled model. Never loads it; the UI
/// decides whether to load based on `auto_load`.
#[tauri::command]
pub async fn prepare_bundled_model(
    app: AppHandle,
    state: State<'_, Desktop>,
) -> Result<BundledModel> {
    let unavailable = || {
        InferenceError::new(
            ErrorCode::InvalidPath,
            "The application resource folder is unavailable.",
        )
    };
    let resource_dir = app.path().resource_dir().map_err(|_| unavailable())?;
    let stamp_dir = state
        .engine
        .database_path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(unavailable)?;
    let verification =
        tauri::async_runtime::spawn_blocking(move || verify(&LFM25, &resource_dir, &stamp_dir))
            .await
            .map_err(|_| unavailable())?;
    let mut bundled = register(&mut *database(state.engine.store())?, &LFM25, verification)?;
    let store = Arc::clone(state.engine.store());
    let verified = bundled.model.as_ref().map(|model| model.id);
    tauri::async_runtime::spawn_blocking(move || {
        reconcile_models(&store, verified.map(|id| (id, LFM25.sha256)))
    })
    .await
    .map_err(|_| unavailable())??;
    let db = database(state.engine.store())?;
    if let Some(model) = &bundled.model {
        bundled.model = Some(db.model(model.id)?.reference);
    }
    let unloaded = db.chat_model_unloaded()?;
    // Restore the last Chat model that loaded successfully, unless the user
    // explicitly unloaded it. A missing file is reported and not loaded.
    bundled.restore = db.chat_model_to_restore()?.map(|saved| ModelRestore {
        available: saved.path.is_file(),
        model: saved.reference,
    });
    // Without a model to restore, load the bundled model automatically only
    // when it is the sole listed choice, and never after an explicit unload.
    bundled.auto_load = bundled.restore.is_none()
        && bundled.model.is_some()
        && db.recent_models()?.len() == 1
        && !unloaded;
    Ok(bundled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "agent-bundled-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("models")).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // The digest of "synthetic gguf" is computed in `fixture_spec`.
    const FIXTURE: BundledSpec = BundledSpec {
        key: "fixture",
        resource: "models/fixture.gguf",
        file_name: "fixture.gguf",
        bytes: 14,
        sha256: "",
    };

    fn fixture_spec() -> BundledSpec {
        use sha2::{Digest, Sha256};
        let digest = {
            let mut hasher = Sha256::new();
            hasher.update(b"synthetic gguf");
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        BundledSpec {
            sha256: Box::leak(digest.into_boxed_str()),
            ..FIXTURE
        }
    }

    #[test]
    fn verification_reports_missing_corrupted_and_verified() {
        let spec = fixture_spec();
        let scratch = Scratch::new();
        let file = scratch.0.join(spec.resource);
        assert_eq!(verify(&spec, &scratch.0, &scratch.0), Verification::Missing);

        fs::write(&file, b"short").unwrap();
        assert_eq!(
            verify(&spec, &scratch.0, &scratch.0),
            Verification::Corrupted
        );
        // Same size, different content.
        fs::write(&file, b"synthetic GGUF").unwrap();
        assert_eq!(
            verify(&spec, &scratch.0, &scratch.0),
            Verification::Corrupted
        );
        assert!(!scratch.0.join(VERIFIED_STAMP).exists());

        fs::write(&file, b"synthetic gguf").unwrap();
        assert_eq!(
            verify(&spec, &scratch.0, &scratch.0),
            Verification::Verified(file.clone())
        );
        assert!(scratch.0.join(VERIFIED_STAMP).exists());
        // A rewritten file changes the stamp and is hashed again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&file, b"synthetic GGUF").unwrap();
        assert_eq!(
            verify(&spec, &scratch.0, &scratch.0),
            Verification::Corrupted
        );
        fs::create_dir(scratch.0.join("models/dir.gguf")).unwrap();
        let directory = BundledSpec {
            resource: "models/dir.gguf",
            ..spec
        };
        assert_eq!(
            verify(&directory, &scratch.0, &scratch.0),
            Verification::Corrupted
        );
    }

    #[test]
    fn registration_never_registers_unverified_resources() {
        let spec = fixture_spec();
        let mut db = storage::SqliteStore::open(Path::new(":memory:")).unwrap();
        for verification in [Verification::Missing, Verification::Corrupted] {
            let result = register(&mut db, &spec, verification).unwrap();
            assert!(result.model.is_none() && !result.auto_load);
        }
        assert!(db.recent_models().unwrap().is_empty());
        let result = register(
            &mut db,
            &spec,
            Verification::Verified(PathBuf::from("/usr/lib/app/models/fixture.gguf")),
        )
        .unwrap();
        assert_eq!(result.state, BundledState::Ready);
        assert!(result.auto_load);
        let model = result.model.unwrap();
        assert!([2048, 4096, 8192].contains(&model.settings.model.context_length));
        assert_eq!(
            model.settings.max_tokens,
            model.settings.model.context_length / 2
        );
        assert_eq!(model.settings.model.threads, 4);
        assert!(!model.settings.enable_thinking);
    }

    #[test]
    fn pinned_lfm_identity_is_consistent() {
        assert!(LFM25.resource.ends_with(LFM25.file_name));
        assert_eq!(LFM25.sha256.len(), 64);
        assert!(LFM25.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    /// Reconciles a disposable copy of a real profile database. Prints the
    /// listed models before and after; never touches the original profile.
    #[test]
    #[ignore = "uses AGENT_PROFILE_DB_COPY, a scratch copy of a real profile database"]
    fn real_profile_copy_collapses_duplicate_models() {
        let path = PathBuf::from(std::env::var_os("AGENT_PROFILE_DB_COPY").unwrap());
        let store = Mutex::new(storage::SqliteStore::open(&path).unwrap());
        let describe = |store: &Mutex<storage::SqliteStore>| {
            store
                .lock()
                .unwrap()
                .recent_models()
                .unwrap()
                .into_iter()
                .map(|model| {
                    format!(
                        "{}:{}:{}:{}",
                        model.id,
                        model.file_name,
                        if model.bundled { "bundled" } else { "imported" },
                        if model.automatic_settings {
                            "automatic"
                        } else {
                            "custom"
                        }
                    )
                })
                .collect::<Vec<_>>()
        };
        println!("before: {:?}", describe(&store));
        reconcile_models(&store, None).unwrap();
        println!("after: {:?}", describe(&store));
        let listed = describe(&store);
        let lfm = listed
            .iter()
            .filter(|entry| entry.contains("LFM2.5-1.2B-Instruct"))
            .count();
        assert_eq!(lfm, 1, "copies of the same LFM2.5 weights collapse");
    }

    /// Synthetic files that share size, first and last MiB, but differ in the
    /// middle must stay separate; real copies of the same GGUF consolidate.
    #[test]
    #[ignore = "hashes AGENT_TEST_MODEL and AGENT_TEST_MODEL_COPY (two copies of one GGUF)"]
    fn full_digests_decide_consolidation() {
        let scratch = Scratch::new();
        let mut bytes = vec![7u8; 3 << 20];
        let a = scratch.0.join("models/a.gguf");
        fs::write(&a, &bytes).unwrap();
        bytes[(3 << 20) / 2] = 8;
        let b = scratch.0.join("models/b.gguf");
        fs::write(&b, &bytes).unwrap();
        assert_eq!(content_key(&a).unwrap(), content_key(&b).unwrap());
        let original = PathBuf::from(std::env::var_os("AGENT_TEST_MODEL").unwrap());
        let copy = PathBuf::from(std::env::var_os("AGENT_TEST_MODEL_COPY").unwrap());
        let store = Mutex::new(storage::SqliteStore::open(Path::new(":memory:")).unwrap());
        {
            let mut db = store.lock().unwrap();
            for (path, name) in [
                (&a, "a.gguf"),
                (&b, "b.gguf"),
                (&original, "lfm.gguf"),
                (&copy, "lfm.gguf"),
            ] {
                db.remember_model(
                    path,
                    name,
                    agent_core::chat::ModelSettings::recommended(4096),
                )
                .unwrap();
            }
        }
        reconcile_models(&store, None).unwrap();
        let listed = store.lock().unwrap().recent_models().unwrap();
        let names: Vec<_> = listed
            .iter()
            .map(|model| model.file_name.as_str())
            .collect();
        println!("listed: {names:?}");
        assert_eq!(names.iter().filter(|name| **name == "lfm.gguf").count(), 1);
        assert!(names.contains(&"a.gguf") && names.contains(&"b.gguf"));
        // The original files were only read.
        assert_eq!(fs::read(&b).unwrap(), bytes);
    }
}
