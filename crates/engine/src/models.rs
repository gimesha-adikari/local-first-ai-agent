//! Chat model registration, settings and worker lifecycle.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use agent_core::{
    chat::{ChatStore, ModelId, ModelReference, ModelSettings},
    inference::{ErrorCode, InferenceError, ModelInfo, Result},
};
use inference::Status;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{Engine, OperationGuard, chat::wait_for, database};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendedChatSettings {
    settings: ModelSettings,
    trained_context: Option<u32>,
}

/// Recommended settings for a model of `file_bytes` with the given trained
/// context, leaving room for the inference context and other allocations.
/// `available` is the memory currently available, including reclaimable
/// cache such as mapped model pages.
pub fn recommended_for_model(
    trained_context: u32,
    file_bytes: u64,
    available: Option<u64>,
) -> ModelSettings {
    let context = match available {
        Some(bytes) if bytes >= file_bytes.saturating_add(2 << 30) => 8192,
        Some(bytes) if bytes < file_bytes.saturating_add(1 << 30) => 2048,
        _ => 4096,
    };
    ModelSettings::recommended(trained_context.min(context))
}

/// Full SHA-256 of a file as lowercase hex.
pub fn sha256_hex(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Sampled content identity: file size and the SHA-256 of the first and last
/// MiB. Equal identities are treated as the same weights at different paths.
/// Reads at most 2 MiB and never modifies the file.
pub fn content_key(path: &Path) -> std::io::Result<String> {
    use std::io::{Seek, SeekFrom};
    const SAMPLE: u64 = 1 << 20;
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; SAMPLE as usize];
    let head = file.read(&mut buffer)?;
    hasher.update(&buffer[..head]);
    if size > SAMPLE {
        file.seek(SeekFrom::Start(size.saturating_sub(SAMPLE).max(SAMPLE)))?;
        let mut tail = Vec::with_capacity(SAMPLE as usize);
        file.take(SAMPLE).read_to_end(&mut tail)?;
        hasher.update(&tail);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("s1:{size}:{digest}"))
}

/// Reuse the listed row for the same weights. A row whose own file is gone,
/// such as a formerly bundled model after a package upgrade, moves to the
/// newly selected file and keeps its settings.
fn reuse_row(
    db: &mut storage::SqliteStore,
    id: ModelId,
    path: &Path,
    name: &str,
) -> Result<ModelReference> {
    if !db.model(id)?.path.is_file() {
        db.relocate_model(id, path, name)?;
    }
    db.mark_model_used(id)?;
    db.model(id).map(|model| model.reference)
}

fn foreign_lease() -> InferenceError {
    InferenceError::new(
        ErrorCode::Busy,
        "The operation lease belongs to another engine.",
    )
}

fn inspection_failed() -> InferenceError {
    InferenceError::new(ErrorCode::InvalidModel, "Could not inspect the GGUF model.")
}

impl Engine {
    /// Recommended settings for a model on this machine.
    pub fn recommended_for_model(&self, trained_context: u32, file_bytes: u64) -> ModelSettings {
        recommended_for_model(
            trained_context,
            file_bytes,
            self.platform.available_memory(),
        )
    }

    pub fn inference_status(&self) -> Result<Status> {
        self.worker.status()
    }

    /// Listed models after collapsing copies of the same weights.
    pub fn list_models(&self) -> Result<Vec<ModelReference>> {
        let db = database(&self.database)?;
        let mut listed = db.recent_models()?;
        for model in &mut listed {
            let saved = db.model(model.id)?;
            model.managed = self.managed_path(&saved.path);
            model.available = if model.managed {
                self.managed_model_available(&db, &saved.path)?
            } else {
                saved.path.is_file()
            };
        }
        Ok(listed)
    }

    /// Register a model file the user selected natively. The caller holds the
    /// operation lease for the whole selection.
    pub async fn register_model(
        &self,
        lease: &OperationGuard,
        path: PathBuf,
    ) -> Result<ModelReference> {
        if !self.owns(lease) {
            return Err(foreign_lease());
        }
        self.check_manual_model_path(&path)?;
        let name = path
            .file_name()
            .ok_or_else(|| {
                InferenceError::new(
                    ErrorCode::PickerFailed,
                    "The picker did not return a file name.",
                )
            })?
            .to_string_lossy()
            .into_owned();
        let inspected = path.clone();
        let available = self.platform.available_memory();
        let (key, recommended, file_bytes) = self
            .blocking(move || {
                let key = content_key(&inspected).map_err(|_| {
                    InferenceError::new(ErrorCode::InvalidModel, "Could not read the GGUF model.")
                })?;
                let (trained_context, bytes) = inference::trained_context(&inspected)?;
                Ok::<_, InferenceError>((
                    key,
                    recommended_for_model(trained_context, bytes, available),
                    bytes,
                ))
            })
            .await
            .ok_or_else(inspection_failed)??;
        // A sampled match only nominates candidates. The same weights at another
        // path reuse the listed row and its settings only when full digests match.
        let candidates = database(&self.database)?.models_with_content_key(&key)?;
        let mut digest = None;
        if !candidates.is_empty() {
            let chosen = path.clone();
            let unknown: Vec<_> = candidates
                .iter()
                .filter(|(_, _, sha)| sha.is_none())
                .map(|(id, path, _)| (*id, path.clone()))
                .collect();
            let (chosen, known) = self
                .blocking(move || {
                    let chosen = sha256_hex(&chosen).ok();
                    let known: Vec<_> = unknown
                        .into_iter()
                        .filter_map(|(id, path)| sha256_hex(&path).ok().map(|sha| (id, sha)))
                        .collect();
                    (chosen, known)
                })
                .await
                .ok_or_else(inspection_failed)?;
            let mut db = database(&self.database)?;
            for (id, sha) in &known {
                db.set_model_sha256(*id, sha)?;
            }
            if let Some(chosen) = &chosen
                && let Some(existing) = db.model_with_sha256(chosen)?
            {
                return reuse_row(&mut db, existing.id, &path, &name);
            }
            digest = chosen;
        }
        // Only a file whose size matches a curated entry needs a full digest
        // to decide whether it is the verified artifact.
        if digest.is_none() && crate::manifest::candidates(file_bytes).next().is_some() {
            let hashed = path.clone();
            digest = self
                .blocking(move || sha256_hex(&hashed).ok())
                .await
                .ok_or_else(inspection_failed)?;
        }
        let artifact = digest
            .as_deref()
            .and_then(|digest| crate::manifest::identify(file_bytes, digest))
            .filter(|artifact| {
                artifact
                    .capabilities
                    .contains(&agent_core::capability::GENERATE)
            });
        let mut db = database(&self.database)?;
        if let Some(digest) = &digest
            && let Some(existing) = db.model_with_sha256(digest)?
        {
            return reuse_row(&mut db, existing.id, &path, &name);
        }
        let reference = db.remember_model(&path, &name, recommended)?;
        db.set_model_content_key(reference.id, &key)?;
        if let Some(digest) = digest {
            db.set_model_sha256(reference.id, &digest)?;
        }
        db.set_model_artifact(reference.id, artifact.map(|artifact| artifact.key))?;
        db.model(reference.id).map(|model| model.reference)
    }

    /// Recommended settings for a model file on this machine.
    pub(crate) async fn recommended_for_file(
        &self,
        path: PathBuf,
        current: ModelSettings,
    ) -> Result<ModelSettings> {
        let available = self.platform.available_memory();
        let mut settings = self
            .blocking(move || {
                let (trained_context, bytes) = inference::trained_context(&path)?;
                Ok::<_, InferenceError>(recommended_for_model(trained_context, bytes, available))
            })
            .await
            .ok_or_else(inspection_failed)??;
        // Thinking is a per-request choice; keep whatever the row already holds.
        settings.enable_thinking = current.enable_thinking;
        Ok(settings)
    }

    /// Loads a model. Settings equal to the saved settings mean "use this
    /// model's settings": automatic models are re-recommended for the current
    /// hardware. Different settings are an explicit choice and become custom.
    pub async fn load_model(
        &self,
        model_id: ModelId,
        settings: ModelSettings,
    ) -> Result<ModelInfo> {
        let _operation = self.reserve()?;
        settings.validate()?;
        let saved = {
            let db = database(&self.database)?;
            let saved = db.model(model_id)?;
            if self.managed_path(&saved.path) && !self.managed_model_available(&db, &saved.path)? {
                return Err(InferenceError::new(
                    ErrorCode::InvalidModel,
                    "This managed model is missing or needs to finish installation.",
                ));
            }
            saved
        };
        let managed_file = if let Some(artifact) = self.artifact_for_managed_path(&saved.path) {
            Some(self.verified_chat_artifact(artifact).await?)
        } else {
            None
        };
        let path = managed_file
            .as_ref()
            .map_or(saved.path, |(path, _)| path.clone());
        let explicit = settings != saved.reference.settings;
        let automatic = saved.reference.automatic_settings && !explicit;
        let settings = if automatic {
            self.recommended_for_file(path.clone(), saved.reference.settings)
                .await?
        } else {
            settings
        };
        settings.validate()?;
        let info = wait_for(self.worker.load(path, settings.model)?).await?;
        let stored = {
            let mut db = database(&self.database)?;
            db.set_model_settings(model_id, settings, automatic)
                .and_then(|()| db.mark_model_used(model_id))
                .and_then(|()| db.set_last_chat_model(model_id))
        };
        if let Err(error) = stored {
            let _ = wait_for(self.worker.unload()?).await;
            return Err(error);
        }
        self.active_model.store(model_id, Ordering::Release);
        Ok(info)
    }

    /// Saves settings for later loads. Changed settings become custom.
    pub fn save_model_settings(&self, model_id: ModelId, settings: ModelSettings) -> Result<()> {
        let _operation = self.reserve()?;
        settings.validate()?;
        if self.active_model() == Some(model_id)
            && let Some(model) = self.worker.status()?.model
            && model.config != settings.model
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Unload the model before changing context length or CPU threads.",
            ));
        }
        let mut db = database(&self.database)?;
        let saved = db.model(model_id)?.reference;
        let automatic = saved.automatic_settings && saved.settings == settings;
        db.set_model_settings(model_id, settings, automatic)
    }

    /// Returns a model to automatic settings and stores the current
    /// recommendation. A loaded model keeps its context and threads until it is
    /// loaded again.
    pub async fn reset_model_settings(&self, model_id: ModelId) -> Result<ModelReference> {
        let _operation = self.reserve()?;
        let saved = database(&self.database)?.model(model_id)?;
        let mut settings = self
            .recommended_for_file(saved.path, saved.reference.settings)
            .await?;
        if self.active_model() == Some(model_id)
            && let Some(model) = self.worker.status()?.model
        {
            // Sampling applies now; context and threads apply at the next load.
            settings.model = model.config;
            settings.max_tokens = settings.max_tokens.min(model.config.context_length - 1);
        }
        let mut db = database(&self.database)?;
        db.set_model_settings(model_id, settings, true)?;
        Ok(db.model(model_id)?.reference)
    }

    pub async fn unload_model(&self) -> Result<()> {
        let _operation = self.reserve()?;
        wait_for(self.worker.unload()?).await?;
        self.active_model.store(0, Ordering::Release);
        database(&self.database)?.set_chat_model_unloaded(true)?;
        Ok(())
    }

    pub fn recommended_chat_settings(&self) -> Result<RecommendedChatSettings> {
        let model = self.worker.status()?.model;
        let trained_context = model.as_ref().map(|model| model.trained_context);
        let settings = model.map_or_else(
            || ModelSettings::recommended(8192),
            |model| self.recommended_for_model(model.trained_context, model.file_bytes),
        );
        Ok(RecommendedChatSettings {
            settings,
            trained_context,
        })
    }
}
