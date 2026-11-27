//! CPU-only BGE-small-en-v1.5 through the pinned llama.cpp runtime.

use std::{
    num::NonZeroU32,
    path::Path,
    sync::mpsc::{self, SyncSender, TrySendError},
    thread::{self, JoinHandle},
};

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    knowledge::{MAX_CHUNK_BYTES, knowledge_limit},
    semantic::{
        CONTEXT_TOKENS, DIMENSIONS, EmbeddingWindow, MAX_CHUNK_WINDOWS, QUERY_PREFIX, UnitVector,
        WINDOW_OVERLAP, WINDOW_TOKENS, incompatible,
    },
};
use futures_channel::oneshot;
use llama_cpp_2::{
    context::{
        LlamaContext,
        params::{LlamaAttentionType, LlamaContextParams, LlamaPoolingType},
    },
    llama_batch::LlamaBatch,
    model::{LlamaModel, params::LlamaModelParams},
    token::LlamaToken,
};

pub const THREADS: i32 = 2;
#[cfg(target_os = "linux")]
const MODEL_NAME: &str = "bge-small-en-v1.5";

/// Whether this build can load the pinned embedding backend.
pub const fn supported() -> bool {
    cfg!(target_os = "linux")
}
#[derive(Default)]
pub struct BgeProvider {
    model: Option<LlamaModel>,
}

impl BgeProvider {
    pub fn available(&self) -> bool {
        self.model.is_some()
    }

    #[cfg(target_os = "linux")]
    pub fn load(&mut self, path: &Path) -> AgentResult<()> {
        let copy = verified_copy(path)?;
        if self.available() {
            return Ok(());
        }
        let backend = crate::backend::cpu_backend().map_err(|_| unavailable())?;
        let model = LlamaModel::load_from_file(
            backend,
            copy.path(),
            &LlamaModelParams::default()
                .with_n_gpu_layers(0)
                .with_use_mmap(false),
        )
        .map_err(|_| incompatible())?;
        let metadata_matches =
            |key, expected| model.meta_val_str(key).is_ok_and(|value| value == expected);
        if !metadata_matches("general.name", MODEL_NAME)
            || !metadata_matches("general.architecture", "bert")
            || !metadata_matches("bert.pooling_type", "2")
            || model.n_ctx_train() != CONTEXT_TOKENS as u32
            || model.n_embd_out() != DIMENSIONS as i32
            || model.vocab().bos() != LlamaToken::new(101)
            || model.vocab().eos() != LlamaToken::new(102)
        {
            return Err(incompatible());
        }
        self.model = Some(model);
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn load(&mut self, _: &Path) -> AgentResult<()> {
        Err(AgentError::new(
            AgentErrorCode::UnsupportedPlatform,
            "Local embedding model loading is supported on Linux.",
        ))
    }

    pub fn unload(&mut self) {
        self.model = None;
    }

    pub fn session(&self) -> AgentResult<EmbeddingSession<'_>> {
        let model = self.model.as_ref().ok_or_else(unavailable)?;
        let backend = crate::backend::cpu_backend().map_err(|_| unavailable())?;
        let context = model
            .new_context(
                backend,
                LlamaContextParams::default()
                    .with_n_ctx(NonZeroU32::new(CONTEXT_TOKENS as u32))
                    .with_n_batch(CONTEXT_TOKENS as u32)
                    .with_n_ubatch(CONTEXT_TOKENS as u32)
                    .with_n_threads(THREADS)
                    .with_n_threads_batch(THREADS)
                    .with_embeddings(true)
                    .with_pooling_type(LlamaPoolingType::Cls)
                    .with_attention_type(LlamaAttentionType::NonCausal),
            )
            .map_err(|_| runtime_failed())?;
        Ok(EmbeddingSession {
            model,
            context,
            batch: LlamaBatch::new(CONTEXT_TOKENS, 1),
        })
    }
}

pub struct EmbeddingSession<'a> {
    model: &'a LlamaModel,
    context: LlamaContext<'a>,
    batch: LlamaBatch<'a>,
}

impl EmbeddingSession<'_> {
    pub fn query(
        &mut self,
        query: &str,
        check: impl Fn() -> AgentResult<()>,
    ) -> AgentResult<UnitVector> {
        check()?;
        if query.trim().is_empty() || query.len() > MAX_CHUNK_BYTES || query.contains('\0') {
            return Err(knowledge_limit());
        }
        let input = format!("{QUERY_PREFIX}{}", query.trim());
        let tokens = self.model.vocab().tokenize(input.as_bytes(), false, false);
        if tokens.len() > WINDOW_TOKENS {
            return Err(knowledge_limit());
        }
        self.embed(&tokens, &check)
    }

    pub fn passage(
        &mut self,
        text: &str,
        check: impl Fn() -> AgentResult<()>,
    ) -> AgentResult<Vec<EmbeddingWindow>> {
        check()?;
        if text.len() > MAX_CHUNK_BYTES || text.contains('\0') {
            return Err(knowledge_limit());
        }
        let mut tokens = self.model.vocab().tokenize(text.as_bytes(), false, false);
        // BERT may discard a chunk consisting solely of Unicode controls. Keep
        // it associated with the real source and represent it as [UNK].
        if tokens.is_empty() {
            tokens.push(LlamaToken::new(100));
        }
        let mut windows = Vec::new();
        for interval in token_windows(tokens.len())? {
            let vector = self.embed(&tokens[interval.clone()], &check)?;
            windows.push(EmbeddingWindow {
                start: interval.start as u32,
                end: interval.end as u32,
                vector,
            });
        }
        Ok(windows)
    }

    fn embed(
        &mut self,
        tokens: &[LlamaToken],
        check: &dyn Fn() -> AgentResult<()>,
    ) -> AgentResult<UnitVector> {
        check()?;
        self.context.clear_kv_cache();
        self.batch.clear();
        // The pinned runtime's BERT graph uses llama_decode with bidirectional
        // attention. Construct the sequence explicitly to include both markers.
        let vocab = self.model.vocab();
        let tokens = std::iter::once(vocab.bos())
            .chain(tokens.iter().copied())
            .chain(std::iter::once(vocab.eos()));
        for (position, token) in tokens.enumerate() {
            self.batch
                .add(token, position as i32, &[0], true)
                .map_err(|_| runtime_failed())?;
        }
        self.context
            .decode(&mut self.batch)
            .map_err(|_| runtime_failed())?;
        check()?;
        UnitVector::normalize(
            self.context
                .embeddings_seq_ith(0)
                .map_err(|_| runtime_failed())?,
        )
    }
}

fn token_windows(count: usize) -> AgentResult<Vec<std::ops::Range<usize>>> {
    let mut start = 0;
    let mut windows = Vec::new();
    loop {
        if windows.len() == MAX_CHUNK_WINDOWS {
            return Err(knowledge_limit());
        }
        let end = (start + WINDOW_TOKENS).min(count);
        windows.push(start..end);
        if end == count {
            return Ok(windows);
        }
        start = end - WINDOW_OVERLAP;
    }
}

#[cfg(target_os = "linux")]
struct VerifiedCopy(std::fs::File);

#[cfg(target_os = "linux")]
impl VerifiedCopy {
    fn path(&self) -> std::path::PathBuf {
        use std::os::fd::AsRawFd;
        format!("/proc/self/fd/{}", self.0.as_raw_fd()).into()
    }
}

#[cfg(target_os = "linux")]
fn verified_copy(path: &Path) -> AgentResult<VerifiedCopy> {
    use agent_core::semantic::BGE_KEY;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::OpenOptionsExt,
        },
    };

    const FILE_BYTES: u64 = 67_308_128;
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| unavailable())?;
    if !input
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() == FILE_BYTES)
    {
        return Err(incompatible());
    }
    // SAFETY: the name is a C string. A successful call returns a new fd.
    let fd = unsafe {
        libc::memfd_create(
            c"agent-bge".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(unavailable());
    }
    // SAFETY: fd is uniquely owned and File will close it on every exit.
    let mut copy = unsafe { File::from_raw_fd(fd) };
    let mut input = input.take(FILE_BYTES + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(|_| unavailable())?;
        if count == 0 {
            break;
        }
        let bytes = &buffer[..count];
        hash.update(bytes);
        copy.write_all(bytes).map_err(|_| unavailable())?;
    }
    if input.limit() != 1 || <[u8; 32]>::from(hash.finalize()) != BGE_KEY.fingerprint {
        return Err(incompatible());
    }
    // SAFETY: File owns fd. Sealing makes the exact verified bytes immutable.
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(copy.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(unavailable());
    }
    Ok(VerifiedCopy(copy))
}

type Job = Box<dyn FnOnce(&mut BgeProvider) + Send>;

pub struct EmbeddingWorker {
    sender: Option<SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl EmbeddingWorker {
    pub fn spawn() -> AgentResult<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        let thread = thread::Builder::new()
            .name("local-embedding".into())
            .spawn(move || {
                let mut provider = BgeProvider::default();
                for job in receiver {
                    job(&mut provider);
                }
            })
            .map_err(|_| unavailable())?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    pub fn submit<T: Send + 'static>(
        &self,
        run: impl FnOnce(&mut BgeProvider) -> AgentResult<T> + Send + 'static,
    ) -> AgentResult<oneshot::Receiver<AgentResult<T>>> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .as_ref()
            .ok_or_else(unavailable)?
            .try_send(Box::new(move |provider| {
                let _ = reply.send(run(provider));
            }))
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    AgentError::new(AgentErrorCode::Busy, "The embedding worker is busy.")
                }
                TrySendError::Disconnected(_) => unavailable(),
            })?;
        Ok(receiver)
    }
}

impl Drop for EmbeddingWorker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn unavailable() -> AgentError {
    AgentError::new(
        AgentErrorCode::EmbeddingUnavailable,
        "The local embedding model or worker is unavailable.",
    )
}

fn runtime_failed() -> AgentError {
    AgentError::new(
        AgentErrorCode::ToolFailed,
        "The pinned embedding runtime could not evaluate this input.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cover_every_token_and_reject_excess() {
        assert_eq!(token_windows(511).unwrap(), [0..510, 478..511]);
        assert_eq!(token_windows(1025).unwrap(), [0..510, 478..988, 956..1025]);
        assert!(token_windows(4096).is_err());
    }
}
