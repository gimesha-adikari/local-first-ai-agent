//! Local model acquisition. Transfers have their own slot and do not hold the
//! inference lease. Inspection, durable installation and registration are
//! separate from activation.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use agent_core::{
    capability::{EMBED, GENERATE},
    chat::{ChatStore, ModelId, ModelSettings},
    inference::{Cancellation, ErrorCode, InferenceError, Result},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use storage::model_downloads::{DownloadRecord, ManagedChatModel};

use crate::{
    Engine, database,
    manifest::{self, Artifact},
    models::{content_key, recommended_for_model},
    network::{
        Failure,
        artifact::{DownloadConsent, Representation},
    },
    policy::Denial,
    semantic::EmbeddingModelStatus,
};

/// Native file operations stay in the host. Names come only from the manifest.
pub trait ModelFiles: Send + Sync {
    fn inspection_path(&self, name: &str) -> PathBuf;
    fn open(&self, name: &str, writable: bool, create: bool) -> io::Result<File>;
    fn free_bytes(&self) -> io::Result<u64>;
    /// Rename without replacing a destination, and make it durable.
    fn install(&self, partial: &str, final_name: &str) -> io::Result<()>;
    fn remove(&self, name: &str) -> io::Result<()>;
}

#[derive(Default)]
pub(crate) struct Downloads(Arc<Mutex<Option<Arc<Transfer>>>>);

struct Transfer {
    key: &'static str,
    cancel: Cancellation,
    bytes: AtomicU64,
    verifying: AtomicBool,
}

struct Slot(Arc<Mutex<Option<Arc<Transfer>>>>);

impl Drop for Slot {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Available,
    Downloading,
    Verifying,
    Interrupted,
    Failed,
    ReadyToFinish,
    Installed,
    External,
    Missing,
    Incompatible,
    RemovalPending,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    artifact: &'static Artifact,
    status: Status,
    received: u64,
    resumable: bool,
    partial: bool,
    managed_file: bool,
    error: Option<String>,
    model_id: Option<ModelId>,
    loaded: bool,
    recommended: Option<ModelSettings>,
    supported: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    entries: Vec<Entry>,
    available_memory: Option<u64>,
    local_only: bool,
    transfer_active: bool,
}

enum Acquire {
    Download { restart: bool },
    Finish,
}

fn partial_name(artifact: &Artifact) -> String {
    format!("{}.partial.gguf", artifact.key)
}

fn final_name(artifact: &Artifact) -> &str {
    artifact.file_name
}

fn filesystem(error: io::Error) -> InferenceError {
    let message = match error.kind() {
        io::ErrorKind::StorageFull => "There is not enough free disk space for this model.",
        io::ErrorKind::AlreadyExists => "A file already occupies this model's managed location.",
        io::ErrorKind::PermissionDenied => {
            "The managed model directory contains an unsafe entry or cannot be accessed."
        }
        _ => {
            "Could not read or write the managed model directory. Check access and free disk space."
        }
    };
    InferenceError::new(ErrorCode::Storage, message)
}

fn stopped() -> InferenceError {
    InferenceError::new(
        ErrorCode::WorkerStopped,
        "The model download worker stopped.",
    )
}

fn unknown() -> InferenceError {
    InferenceError::new(ErrorCode::NotFound, "This model is not in the catalog.")
}

fn busy() -> InferenceError {
    InferenceError::new(
        ErrorCode::Busy,
        "Wait for the current model download or file operation to finish.",
    )
}

fn incompatible(message: &str) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidModel, message)
}

fn network_error(failure: Failure) -> InferenceError {
    let (code, message) = match failure {
        Failure::Cancelled => (
            ErrorCode::Cancelled,
            "Download cancelled. Partial data can be kept for retry.",
        ),
        Failure::Denied(Denial::Locality | Denial::Revoked | Denial::Scope) => (
            ErrorCode::PolicyDenied,
            "Local-only mode blocks model downloads. A new download approval is required after it is turned off.",
        ),
        Failure::TimedOut | Failure::Denied(Denial::Expired) => (
            ErrorCode::TimedOut,
            "The download timed out. Retry when your connection is ready.",
        ),
        Failure::Address | Failure::Redirect { .. } => (
            ErrorCode::NetworkFailed,
            "The model host returned an unapproved destination.",
        ),
        Failure::Protocol | Failure::TooLarge => (
            ErrorCode::ProviderInvalidResponse,
            "The model host returned an unexpected size, range, or response. Try again from the beginning.",
        ),
        Failure::Stopped => return stopped(),
        _ => (
            ErrorCode::NetworkFailed,
            "The model host could not complete the download. Try again when your connection is ready.",
        ),
    };
    InferenceError::new(code, message)
}

fn model_supported(artifact: &Artifact) -> bool {
    artifact.format == "gguf"
        && (artifact.capabilities.contains(&GENERATE)
            || (artifact.capabilities.contains(&EMBED) && inference::embedding::supported()))
}

fn regular_size(path: &Path) -> Option<u64> {
    let metadata = path.symlink_metadata().ok()?;
    metadata.is_file().then_some(metadata.len())
}

impl Engine {
    pub(crate) fn model_directory(&self) -> PathBuf {
        let mut name = self.database_path.as_os_str().to_owned();
        name.push(".models");
        PathBuf::from(name)
    }

    fn final_path(&self, artifact: &Artifact) -> PathBuf {
        self.model_directory().join(final_name(artifact))
    }

    pub(crate) fn managed_path(&self, path: &Path) -> bool {
        let root = self.model_directory();
        path.starts_with(&root)
            || root.canonicalize().is_ok_and(|managed| {
                path.canonicalize()
                    .is_ok_and(|actual| actual.starts_with(managed))
            })
    }

    pub(crate) fn check_manual_model_path(&self, path: &Path) -> Result<()> {
        if self.managed_path(path) {
            return Err(InferenceError::new(
                ErrorCode::InvalidPath,
                "Use Installed models to load files from the managed library.",
            ));
        }
        Ok(())
    }

    pub(crate) fn managed_model_available(
        &self,
        db: &storage::SqliteStore,
        path: &Path,
    ) -> Result<bool> {
        let Some(artifact) = self.artifact_for_managed_path(path) else {
            return Ok(false);
        };
        let installed = db
            .model_download(artifact.key, artifact.sha256)?
            .is_some_and(|record| record.phase == "installed");
        let private_root = self
            .model_directory()
            .symlink_metadata()
            .is_ok_and(|meta| meta.is_dir());
        Ok(installed && private_root && regular_size(path) == Some(artifact.bytes))
    }

    pub(crate) fn artifact_for_managed_path(&self, path: &Path) -> Option<&'static Artifact> {
        manifest::CURATED
            .iter()
            .find(|artifact| self.final_path(artifact) == path)
    }

    // Revalidate a managed file before passing it to native inference. The
    // open directory stays alive while callers use its descriptor-backed path.
    pub(crate) async fn verified_chat_artifact(
        &self,
        artifact: &'static Artifact,
    ) -> Result<(PathBuf, Arc<dyn ModelFiles>)> {
        let files = self.open_model_files(self.model_directory()).await?;
        let inspection = Arc::clone(&files);
        let integrity = self
            .blocking(move || {
                let file = inspection
                    .open(final_name(artifact), false, false)
                    .map_err(filesystem)?;
                verify_file(file, artifact, &Cancellation::default())
            })
            .await
            .ok_or_else(stopped)?;
        if let Err(error) = integrity {
            if error.code == ErrorCode::InvalidModel {
                database(&self.database)?.set_model_download_phase(
                    artifact.key,
                    "incompatible",
                    Some(&error.message),
                )?;
                self.platform.model_library_changed();
            }
            return Err(error);
        }
        Ok((files.inspection_path(final_name(artifact)), files))
    }

    /// Before presenting native consent, validate the curated identity and
    /// refuse Local-only operation.
    pub fn model_download_offer(&self, key: &str) -> Result<&'static Artifact> {
        let artifact = manifest::by_key(key).ok_or_else(unknown)?;
        if !model_supported(artifact) {
            return Err(incompatible(
                "This app build cannot use this catalog model.",
            ));
        }
        if self.policy.is_local_only() {
            return Err(network_error(Failure::Denied(Denial::Locality)));
        }
        Ok(artifact)
    }

    fn reserve_transfer(&self, artifact: &Artifact) -> Result<(Arc<Transfer>, Slot)> {
        let mut slot = self.downloads.0.lock().map_err(|_| stopped())?;
        if slot.is_some() {
            return Err(busy());
        }
        let transfer = Arc::new(Transfer {
            key: artifact.key,
            cancel: Cancellation::default(),
            bytes: AtomicU64::new(0),
            verifying: AtomicBool::new(false),
        });
        *slot = Some(Arc::clone(&transfer));
        Ok((transfer, Slot(Arc::clone(&self.downloads.0))))
    }

    pub fn start_model_download(self: &Arc<Self>, key: &str, restart: bool) -> Result<()> {
        let artifact = self.model_download_offer(key)?;
        self.start_acquisition(artifact, Acquire::Download { restart })
    }

    pub fn finish_model_installation(self: &Arc<Self>, key: &str) -> Result<()> {
        let artifact = manifest::by_key(key).ok_or_else(unknown)?;
        self.start_acquisition(artifact, Acquire::Finish)
    }

    fn start_acquisition(
        self: &Arc<Self>,
        artifact: &'static Artifact,
        mode: Acquire,
    ) -> Result<()> {
        let (transfer, slot) = self.reserve_transfer(artifact)?;
        let consent = match mode {
            Acquire::Download { .. } => Some(
                DownloadConsent::new(&self.policy, artifact, transfer.cancel.clone())
                    .map_err(network_error)?,
            ),
            Acquire::Finish => None,
        };
        let plan = self.download_plan(artifact)?;
        if plan.external && matches!(mode, Acquire::Download { .. }) {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "This verified model is already available.",
            ));
        }
        let final_present = self.final_path(artifact).symlink_metadata().is_ok();
        let partial_present = self
            .model_directory()
            .join(partial_name(artifact))
            .symlink_metadata()
            .is_ok();
        if partial_present && plan.record.is_none() {
            return Err(filesystem(io::ErrorKind::AlreadyExists.into()));
        }
        let phase = plan.record.as_ref().map(|record| record.phase.as_str());
        match mode {
            Acquire::Download { .. } if final_present => {
                return Err(InferenceError::new(
                    ErrorCode::InvalidConfig,
                    "A model file is already present. Use Installed models or finish its installation.",
                ));
            }
            Acquire::Download { .. } if phase == Some("ready") => {
                return Err(InferenceError::new(
                    ErrorCode::InvalidConfig,
                    "Finish the verified installation first.",
                ));
            }
            Acquire::Download { .. } => {
                database(&self.database)?.begin_model_download(artifact.key, artifact.sha256)?
            }
            Acquire::Finish if phase != Some("ready") => return Err(unknown()),
            Acquire::Finish => transfer.verifying.store(true, Ordering::Release),
        }
        let engine = Arc::clone(self);
        self.platform.spawn(Box::pin(async move {
            let slot = slot;
            let result = engine
                .acquire(artifact, mode, &transfer, consent.as_ref())
                .await;
            if let Err(error) = result
                && let Ok(mut db) = database(&engine.database)
            {
                let ready = db
                    .model_download(artifact.key, artifact.sha256)
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.phase == "ready");
                let phase = match error.code {
                    ErrorCode::InvalidModel => "incompatible",
                    _ if ready => "ready",
                    ErrorCode::Cancelled
                    | ErrorCode::PolicyDenied
                    | ErrorCode::TimedOut
                    | ErrorCode::NetworkFailed => "interrupted",
                    _ => "failed",
                };
                let _ = db.set_model_download_phase(artifact.key, phase, Some(&error.message));
            }
            drop(slot);
            engine.platform.model_library_changed();
        }));
        Ok(())
    }

    pub fn cancel_model_download(&self, key: &str) -> Result<bool> {
        let artifact = manifest::by_key(key).ok_or_else(unknown)?;
        let slot = self.downloads.0.lock().map_err(|_| stopped())?;
        Ok(slot.as_ref().is_some_and(|transfer| {
            if transfer.key != artifact.key {
                return false;
            }
            transfer.cancel.cancel();
            true
        }))
    }

    pub async fn remove_model_download(&self, key: &str, installed: bool) -> Result<()> {
        let artifact = manifest::by_key(key).ok_or_else(unknown)?;
        self.remove_artifact(artifact, installed).await
    }

    async fn remove_artifact(&self, artifact: &'static Artifact, installed: bool) -> Result<()> {
        let (_transfer, slot) = self.reserve_transfer(artifact)?;
        let _operation = installed.then(|| self.reserve()).transpose()?;
        let path = self.final_path(artifact);
        let root = self.model_directory();
        {
            let mut db = database(&self.database)?;
            let record = db
                .model_download(artifact.key, artifact.sha256)?
                .ok_or_else(unknown)?;
            if installed {
                let active = self
                    .active_model()
                    .map(|id| db.model(id))
                    .transpose()?
                    .is_some_and(|active| {
                        active.path == path
                            || active
                                .path
                                .canonicalize()
                                .ok()
                                .zip(path.canonicalize().ok())
                                .is_some_and(|(left, right)| left == right)
                    });
                let embedding_in_use =
                    db.embedding_model_choice()?
                        .is_some_and(|(choice, restore)| {
                            choice == path && (restore || self.embedding_status().available)
                        });
                if active || embedding_in_use {
                    return Err(InferenceError::new(
                        ErrorCode::Busy,
                        "Unload this model before removing its managed file.",
                    ));
                }
                db.remove_model_download(artifact.key, &path)?;
            } else if record.phase == "installed"
                || (record.phase == "ready" && regular_size(&path).is_some())
            {
                return Err(InferenceError::new(
                    ErrorCode::InvalidConfig,
                    "Remove the managed installation from Installed models.",
                ));
            } else {
                db.set_model_download_representation(artifact.key, None)?;
                db.set_model_download_phase(artifact.key, "removed", None)?;
            }
        }
        let files = self.open_model_files(root).await?;
        let result = self
            .blocking(move || {
                if installed {
                    files.remove(final_name(artifact))?;
                }
                files.remove(&partial_name(artifact))
            })
            .await
            .ok_or_else(stopped)?
            .map_err(filesystem);
        drop(slot);
        self.platform.model_library_changed();
        result
    }

    pub async fn use_downloaded_embedding(&self, key: &str) -> Result<EmbeddingModelStatus> {
        let artifact = manifest::by_key(key)
            .filter(|artifact| artifact.capabilities.contains(&EMBED))
            .ok_or_else(unknown)?;
        let (_transfer, _slot) = self.reserve_transfer(artifact)?;
        let path = self.final_path(artifact);
        if !self.managed_model_available(&*database(&self.database)?, &path)? {
            return Err(unknown());
        }
        let lease = self
            .begin_embedding_selection()
            .map_err(|error| InferenceError::new(ErrorCode::EmbeddingUnavailable, error.message))?;
        self.select_managed_embedding_model(lease, path)
            .await
            .map_err(|error| InferenceError::new(ErrorCode::EmbeddingUnavailable, error.message))
    }

    pub fn model_catalog(&self) -> Result<ModelCatalog> {
        let active = self.downloads.0.lock().map_err(|_| stopped())?.clone();
        let available_memory = self.platform.available_memory();
        let root = self.model_directory();
        let mut entries = Vec::with_capacity(manifest::CURATED.len());
        for artifact in manifest::CURATED {
            let plan = self.download_plan(artifact)?;
            let record = plan.record.as_ref();
            let partial = regular_size(&root.join(partial_name(artifact)));
            let final_file = regular_size(&self.final_path(artifact));
            let transfer = active.as_ref().filter(|state| state.key == artifact.key);
            let phase = record.map(|row| row.phase.as_str());
            let status = if let Some(transfer) = transfer {
                if transfer.verifying.load(Ordering::Acquire) {
                    Status::Verifying
                } else {
                    Status::Downloading
                }
            } else {
                match (phase, final_file, plan.external, partial) {
                    (Some("removed"), Some(_), _, _) => Status::RemovalPending,
                    (Some("installed"), Some(bytes), _, _) if bytes == artifact.bytes => {
                        Status::Installed
                    }
                    (Some("ready"), _, _, _) => Status::ReadyToFinish,
                    (Some("incompatible"), _, _, _) => Status::Incompatible,
                    (_, _, true, _) => Status::External,
                    (Some("installed"), _, _, _) => Status::Missing,
                    (Some("failed"), _, _, _) => Status::Failed,
                    (Some("interrupted" | "transferring"), _, _, _) | (_, _, _, Some(_)) => {
                        Status::Interrupted
                    }
                    (_, Some(_), _, _) => Status::Incompatible,
                    _ => Status::Available,
                }
            };
            entries.push(Entry {
                artifact,
                status,
                received: transfer
                    .map_or_else(
                        || final_file.or(partial).unwrap_or(0),
                        |state| state.bytes.load(Ordering::Acquire),
                    )
                    .min(artifact.bytes),
                resumable: record.is_some_and(|row| row.representation.is_some())
                    && partial.is_some_and(|bytes| bytes > 0 && bytes < artifact.bytes),
                partial: partial.is_some(),
                managed_file: final_file.is_some() && record.is_some(),
                error: record.and_then(|r| r.error.clone()),
                model_id: plan.model_id,
                loaded: plan.loaded,
                recommended: artifact.capabilities.contains(&GENERATE).then(|| {
                    recommended_for_model(
                        artifact.trained_context,
                        artifact.bytes,
                        available_memory,
                    )
                }),
                supported: model_supported(artifact),
            });
        }
        Ok(ModelCatalog {
            entries,
            available_memory,
            local_only: self.policy.is_local_only(),
            transfer_active: active.is_some(),
        })
    }

    fn download_plan(&self, artifact: &Artifact) -> Result<DownloadPlan> {
        let db = database(&self.database)?;
        let path = self.final_path(artifact);
        let record = db.model_download(artifact.key, artifact.sha256)?;
        let mut model_id = db.model_at_path(&path)?;
        let mut external = false;
        let mut loaded = model_id.is_some_and(|id| self.active_model() == Some(id));
        if artifact.capabilities.contains(&GENERATE) {
            if let Some(model) = db.model_with_sha256(artifact.sha256)? {
                let known = db.model(model.id)?;
                if !self.managed_path(&known.path)
                    && known
                        .path
                        .metadata()
                        .is_ok_and(|meta| meta.is_file() && meta.len() == artifact.bytes)
                {
                    model_id = Some(model.id);
                    external = true;
                    loaded = self.active_model() == model_id;
                }
            }
        } else if let Some((known, _)) = db.embedding_model_choice()? {
            let embedding = self.embedding_status();
            let verified_choice = known
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.len() == artifact.bytes);
            external = verified_choice && !self.managed_path(&known);
            loaded = (known == path || external) && embedding.available;
        }
        Ok(DownloadPlan {
            record,
            model_id,
            external,
            loaded,
        })
    }

    async fn open_model_files(&self, root: PathBuf) -> Result<Arc<dyn ModelFiles>> {
        let platform = Arc::clone(&self.platform);
        self.blocking(move || platform.model_files(&root).map(Arc::from))
            .await
            .ok_or_else(stopped)?
            .map_err(filesystem)
    }

    async fn acquire(
        &self,
        artifact: &'static Artifact,
        mode: Acquire,
        transfer: &Transfer,
        consent: Option<&DownloadConsent>,
    ) -> Result<()> {
        let files = self.open_model_files(self.model_directory()).await?;
        let partial = partial_name(artifact);
        if let Acquire::Download { restart } = mode {
            let consent = consent.expect("network start carries explicit download consent");
            match files.open(final_name(artifact), false, false) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(filesystem(error)),
                Ok(_) => return Err(filesystem(io::ErrorKind::AlreadyExists.into())),
            }
            self.transfer_artifact(
                artifact,
                &partial,
                Arc::clone(&files),
                restart,
                transfer,
                consent,
            )
            .await?;
        }
        transfer.verifying.store(true, Ordering::Release);
        let cancel = transfer.cancel.clone();
        let inspection_files = Arc::clone(&files);
        let memory = self.platform.available_memory();
        let (inspected, already_installed) = self
            .blocking(move || {
                let installed = match inspection_files.open(final_name(artifact), false, false) {
                    Ok(file) => Some(file),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(filesystem(error)),
                };
                let already_installed = installed.is_some();
                let name = if already_installed {
                    final_name(artifact)
                } else {
                    &partial
                };
                let file = installed.map_or_else(
                    || {
                        inspection_files
                            .open(name, false, false)
                            .map_err(filesystem)
                    },
                    Ok,
                )?;
                verify_file(file, artifact, &cancel)?;
                let path = inspection_files.inspection_path(name);
                let (context, _) = inference::trained_context(&path)?;
                let inspected = if artifact.capabilities.contains(&GENERATE) {
                    Some((
                        content_key(&path).map_err(filesystem)?,
                        recommended_for_model(context, artifact.bytes, memory),
                    ))
                } else if artifact.capabilities.contains(&EMBED)
                    && model_supported(artifact)
                    && context == artifact.trained_context
                {
                    None
                } else {
                    return Err(incompatible(
                        "This build could not verify compatibility with the model's runtime.",
                    ));
                };
                Ok((inspected, already_installed))
            })
            .await
            .ok_or_else(stopped)??;
        if let Some(consent) = consent {
            consent.check(&self.policy).map_err(network_error)?;
        }
        if transfer.cancel.is_cancelled() {
            return Err(network_error(Failure::Cancelled));
        }
        // 'ready' is durable before the rename. An explicit local recovery can
        // safely re-check either side of the rename without making a request.
        database(&self.database)?.set_model_download_phase(artifact.key, "ready", None)?;
        if !already_installed {
            self.blocking(move || files.install(&partial_name(artifact), final_name(artifact)))
                .await
                .ok_or_else(stopped)?
                .map_err(filesystem)?;
        }
        let _lease = self.reserve()?;
        if self.download_plan(artifact)?.external {
            // A matching external model may have been imported during the
            // transfer. Keep the existing row and release the redundant copy.
            let files = self.open_model_files(self.model_directory()).await?;
            self.blocking(move || files.remove(final_name(artifact)))
                .await
                .ok_or_else(stopped)?
                .map_err(filesystem)?;
            return database(&self.database)?.set_model_download_phase(
                artifact.key,
                "removed",
                None,
            );
        }
        let path = self.final_path(artifact);
        let mut db = database(&self.database)?;
        let mut reuse = db.model_at_path(&path)?;
        if reuse.is_none()
            && let Some(existing) = db.model_with_sha256(artifact.sha256)?
        {
            let known = db.model(existing.id)?;
            if !known.path.is_file() && self.active_model() != Some(existing.id) {
                reuse = Some(existing.id);
            }
        }
        let chat = inspected.as_ref().map(|(key, settings)| ManagedChatModel {
            path: &path,
            file_name: artifact.file_name,
            settings: *settings,
            content_key: key,
            reuse,
        });
        db.finish_model_download((artifact.key, artifact.sha256), chat)
    }

    async fn transfer_artifact(
        &self,
        artifact: &'static Artifact,
        partial: &str,
        files: Arc<dyn ModelFiles>,
        restart: bool,
        transfer: &Transfer,
        consent: &DownloadConsent,
    ) -> Result<()> {
        consent.check(&self.policy).map_err(network_error)?;
        let representation = database(&self.database)?
            .model_download(artifact.key, artifact.sha256)?
            .and_then(|record| record.representation)
            .map(|(fingerprint, etag)| Representation { fingerprint, etag });
        let name = partial.to_owned();
        let (mut file, offset, representation, files) = self
            .blocking(move || {
                let file = match files.open(&name, true, false) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        files.open(&name, true, true).map_err(filesystem)?
                    }
                    Err(error) => return Err(filesystem(error)),
                };
                let length = file.metadata().map_err(filesystem)?.len();
                let offset = if !restart && length < artifact.bytes {
                    length
                } else {
                    0
                };
                let offset = if offset > 0 && representation.is_some() {
                    offset
                } else {
                    0
                };
                ensure_space(
                    files
                        .free_bytes()
                        .map_err(filesystem)?
                        .saturating_add(length - offset),
                    artifact.bytes - offset,
                )?;
                if offset == 0 && length != 0 {
                    file.set_len(0)
                        .and_then(|()| file.sync_all())
                        .map_err(filesystem)?;
                }
                Ok((file, offset, representation, files))
            })
            .await
            .ok_or_else(stopped)??;
        transfer.bytes.store(offset, Ordering::Release);
        let resume = representation
            .as_ref()
            .filter(|_| offset > 0)
            .map(|value| (offset, value));
        let opened = self
            .boundary
            .open_artifact(consent, resume, |host| {
                database(&self.database)
                    .and_then(|mut db| db.set_model_download_destination(artifact.key, host))
                    .map_err(|_| Failure::Stopped)
            })
            .await;
        let mut response = match opened {
            Err(failure @ (Failure::Protocol | Failure::TooLarge)) if resume.is_some() => {
                database(&self.database)?.set_model_download_representation(artifact.key, None)?;
                return Err(network_error(failure));
            }
            other => other.map_err(network_error)?,
        };
        let start = response.offset;
        if start == 0 && offset != 0 {
            // A full 200 response ignored Range or rejected the validator.
            // Flush the truncation before associating the new representation.
            file = self
                .blocking(move || {
                    file.set_len(0)
                        .and_then(|()| file.sync_all())
                        .map_err(filesystem)?;
                    ensure_space(files.free_bytes().map_err(filesystem)?, artifact.bytes)?;
                    Ok(file)
                })
                .await
                .ok_or_else(stopped)??;
        }
        transfer.bytes.store(start, Ordering::Release);
        let representation = response
            .representation
            .as_ref()
            .map(|value| (value.fingerprint.as_str(), value.etag.as_str()));
        database(&self.database)?
            .set_model_download_representation(artifact.key, representation)?;
        file = self
            .blocking(move || {
                file.seek(SeekFrom::Start(start)).map_err(filesystem)?;
                Ok(file)
            })
            .await
            .ok_or_else(stopped)??;

        let mut received = start;
        // Write in bounded batches off the async and inference workers. Flush
        // a short batch even on interruption to keep a useful partial.
        let mut batch = Vec::with_capacity(1024 * 1024 + 64 * 1024);
        loop {
            let piece = response.stream.next().await;
            let finished = !matches!(piece, Some(Ok(_)));
            let result = match piece {
                Some(Ok(bytes)) => {
                    if received
                        .saturating_add(batch.len() as u64)
                        .saturating_add(bytes.len() as u64)
                        > artifact.bytes
                    {
                        return Err(network_error(Failure::TooLarge));
                    }
                    batch.extend_from_slice(&bytes);
                    Ok(())
                }
                Some(Err(failure)) => Err(network_error(failure)),
                None => Ok(()),
            };
            if batch.len() >= 1024 * 1024 || finished {
                received += batch.len() as u64;
                (file, batch) = self
                    .blocking(move || {
                        file.write_all(&batch).map_err(filesystem)?;
                        if finished {
                            file.sync_all().map_err(filesystem)?;
                        }
                        batch.clear();
                        Ok((file, batch))
                    })
                    .await
                    .ok_or_else(stopped)??;
                transfer.bytes.store(received, Ordering::Release);
            }
            result?;
            if finished {
                break;
            }
        }
        response
            .stream
            .finish_checked()
            .await
            .1
            .map_err(network_error)?;
        if received != artifact.bytes {
            return Err(network_error(Failure::Protocol));
        }
        Ok(())
    }
}

fn ensure_space(available: u64, remaining: u64) -> Result<()> {
    // Keep a small amount of space for SQLite and filesystem metadata.
    if available < remaining.saturating_add(16 * 1024 * 1024) {
        return Err(filesystem(io::ErrorKind::StorageFull.into()));
    }
    Ok(())
}

struct DownloadPlan {
    record: Option<DownloadRecord>,
    model_id: Option<ModelId>,
    external: bool,
    loaded: bool,
}

fn verify_file(mut file: File, artifact: &Artifact, cancel: &Cancellation) -> Result<()> {
    if file.metadata().map_err(filesystem)?.len() != artifact.bytes {
        return Err(incompatible(
            "The completed file has the wrong size. Remove it and download again.",
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut remaining = artifact.bytes;
    while remaining != 0 {
        if cancel.is_cancelled() {
            return Err(network_error(Failure::Cancelled));
        }
        let len = (buffer.len() as u64).min(remaining) as usize;
        file.read_exact(&mut buffer[..len]).map_err(filesystem)?;
        hash.update(&buffer[..len]);
        remaining -= len as u64;
    }
    if file.read(&mut buffer[..1]).map_err(filesystem)? != 0
        || format!("{:x}", hash.finalize()) != artifact.sha256
    {
        return Err(incompatible(
            "The downloaded file failed SHA-256 verification. Remove it and download again.",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
