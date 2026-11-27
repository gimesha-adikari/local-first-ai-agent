//! Conversation attachments: the bounded lifecycle from native selection to
//! readiness, use, cancellation, removal and expiration, plus deterministic
//! passage selection for Chat (Stage 04-D).
//!
//! The host pins each selected file and supplies an [`AttachmentFile`] that
//! reads one immutable snapshot and runs the trusted, sandboxed extractors.
//! The engine owns identifiers, scopes, admission limits, the policy checks
//! for local preparation and explicit retention, and the in-memory store.
//!
//! Identifiers are random. They never grant access on their own: every use
//! is checked against the owning draft or conversation.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use agent_core::{
    agent::{AgentError, AgentErrorCode, AgentResult},
    attachment::{
        ATTACHMENT_ID_LEN, AttachmentKind, AttachmentOrigin, AttachmentRetention, AttachmentTask,
        MAX_ATTACHMENT_SOURCES, MAX_SCOPE_ATTACHMENTS, MAX_SESSION_ATTACHMENTS, MAX_SESSION_BYTES,
        PREPARE_DEADLINE_SECONDS, PageCoverage, PassageSource, TEMPORARY_IDLE_SECONDS,
        valid_attachment_id,
    },
    capability::{IMPORT, Retention},
    chat::ConversationId,
    chat_context::ChatIntent,
    classification::{ATTACHMENT, Labels},
    inference::Cancellation,
    inference::{ErrorCode, InferenceError, Result as Res},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use storage::{
    SqliteStore,
    attachments::{RetainedAttachment, StoredImage, StoredPassage},
};
use zeroize::Zeroize;

use crate::{
    Engine, Platform,
    policy::{Operation, Permission, Policy, Session},
};

/// A preparation step the host reports while working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Reading,
    Extracting,
    Recognizing,
}

/// One passage of extracted or recognized text.
pub struct PreparedText {
    pub source: PassageSource,
    pub text: String,
}

/// A validated, metadata-stripped image.
pub struct PreparedImage {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// What local preparation produced. Wiped when the last reference drops.
#[derive(Default)]
pub struct PreparedContent {
    pub passages: Vec<PreparedText>,
    pub pages: Option<PageCoverage>,
    pub image: Option<PreparedImage>,
    /// Engine and languages of local text recognition.
    pub recognition: Option<String>,
    /// Factual limitations, such as text recognition being unavailable.
    pub notes: Vec<String>,
}

impl Drop for PreparedContent {
    fn drop(&mut self) {
        for passage in &mut self.passages {
            passage.text.zeroize();
        }
        if let Some(image) = &mut self.image {
            image.bytes.zeroize();
        }
    }
}

impl PreparedContent {
    fn bytes(&self) -> usize {
        self.passages
            .iter()
            .map(|passage| passage.text.len())
            .sum::<usize>()
            + self.image.as_ref().map_or(0, |image| image.bytes.len())
    }

    fn characters(&self) -> usize {
        self.passages
            .iter()
            .map(|passage| passage.text.chars().count())
            .sum()
    }

    fn fingerprint(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        for passage in &self.passages {
            hash.update(passage.text.as_bytes());
            hash.update([0]);
        }
        if let Some(image) = &self.image {
            hash.update(&image.bytes);
        }
        hash.finalize().into()
    }
}

/// A pinned native selection. The host implements this with the trusted
/// tool adapters: one snapshot read, validation and sandboxed extraction.
pub trait AttachmentFile: Send + 'static {
    fn name(&self) -> &str;
    fn kind(&self) -> AttachmentKind;
    /// Where the content came from; only a camera snapshot says otherwise.
    fn origin(&self) -> AttachmentOrigin {
        AttachmentOrigin::File
    }
    fn file_bytes(&self) -> u64;
    /// Read, validate and extract, reporting each stage and calling `check`
    /// between bounded steps.
    fn prepare(
        self: Box<Self>,
        stage: &dyn Fn(Stage),
        check: &dyn Fn() -> AgentResult<()>,
    ) -> AgentResult<PreparedContent>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum AttachmentPhase {
    Queued,
    Reading,
    Extracting,
    Recognizing,
    Ready,
    Failed {
        message: String,
    },
    Cancelled,
    /// Temporary content was discarded after a period without use.
    Expired,
}

impl AttachmentPhase {
    fn preparing(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Reading | Self::Extracting | Self::Recognizing
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Draft,
    Conversation(ConversationId),
}

impl From<Option<ConversationId>> for Scope {
    fn from(value: Option<ConversationId>) -> Self {
        value.map_or(Self::Draft, Self::Conversation)
    }
}

struct EntryData {
    scope: Scope,
    retention: AttachmentRetention,
    phase: AttachmentPhase,
    content: Option<Arc<PreparedContent>>,
    last_used: Instant,
    /// A snapshot is stored in the encrypted profile.
    saved: bool,
}

struct Entry {
    id: String,
    name: String,
    kind: AttachmentKind,
    origin: AttachmentOrigin,
    file_bytes: u64,
    cancellation: Cancellation,
    data: Mutex<EntryData>,
}

impl Entry {
    fn data(&self) -> Res<MutexGuard<'_, EntryData>> {
        self.data.lock().map_err(|_| store_error())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageView {
    pub width: u32,
    pub height: u32,
    /// Small enough to be sent to a provider that accepts images.
    pub remote_eligible: bool,
}

/// What the composer shows for one attachment. No content.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub id: String,
    pub name: String,
    pub kind: AttachmentKind,
    pub origin: AttachmentOrigin,
    pub file_bytes: u64,
    pub retention: AttachmentRetention,
    /// Kept in the encrypted profile with the conversation.
    pub saved: bool,
    /// Not yet bound to a conversation.
    pub draft: bool,
    #[serde(flatten)]
    pub phase: AttachmentPhase,
    pub passages: usize,
    pub characters: usize,
    pub pages: Option<PageCoverage>,
    pub image: Option<ImageView>,
    pub recognition: Option<String>,
    pub notes: Vec<String>,
}

fn store_error() -> InferenceError {
    InferenceError::new(ErrorCode::Storage, "The attachment store is unavailable.")
}

fn unavailable() -> InferenceError {
    InferenceError::new(
        ErrorCode::InvalidConfig,
        "That attachment isn't available in this conversation.",
    )
}

fn limit(message: impl Into<String>) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, message)
}

fn cancelled() -> AgentError {
    AgentError::new(
        AgentErrorCode::Cancelled,
        "Attachment preparation was cancelled.",
    )
}

fn new_id() -> Res<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| store_error())?;
    let mut id = String::with_capacity(ATTACHMENT_ID_LEN);
    id.push('a');
    for byte in bytes {
        id.push_str(&format!("{byte:02x}"));
    }
    Ok(id)
}

/// One attachment resolved for a Chat turn.
pub(crate) struct Resolved {
    pub id: String,
    pub name: String,
    pub kind: AttachmentKind,
    pub origin: AttachmentOrigin,
    pub retention: AttachmentRetention,
    pub content: Arc<PreparedContent>,
}

#[derive(Default)]
struct State {
    entries: Vec<Arc<Entry>>,
    queue: VecDeque<(Arc<Entry>, Box<dyn AttachmentFile>)>,
    working: bool,
}

/// The session's attachments. Temporary content exists only here.
#[derive(Default)]
pub(crate) struct AttachmentStore {
    state: Mutex<State>,
}

impl AttachmentStore {
    fn state(&self) -> Res<MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| store_error())
    }

    /// Drop the content of temporary attachments unused for too long.
    pub(crate) fn expire(&self, now: Instant) -> Res<()> {
        let idle = Duration::from_secs(TEMPORARY_IDLE_SECONDS);
        for entry in &self.state()?.entries {
            let mut data = entry.data()?;
            if data.retention == AttachmentRetention::Temporary
                && !data.saved
                && data.phase == AttachmentPhase::Ready
                && now.duration_since(data.last_used) >= idle
            {
                data.content = None;
                data.phase = AttachmentPhase::Expired;
            }
        }
        Ok(())
    }

    fn find(&self, id: &str) -> Res<Option<Arc<Entry>>> {
        if !valid_attachment_id(id) {
            return Err(unavailable());
        }
        Ok(self
            .state()?
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned())
    }

    /// The entry, only if it belongs to `scope`.
    fn owned(&self, scope: Scope, id: &str) -> Res<Arc<Entry>> {
        let entry = self.find(id)?.ok_or_else(unavailable)?;
        if entry.data()?.scope != scope {
            return Err(unavailable());
        }
        Ok(entry)
    }

    fn held_bytes(entries: &[Arc<Entry>]) -> Res<usize> {
        let mut total = 0;
        for entry in entries {
            let data = entry.data()?;
            total += match (&data.content, data.phase.preparing()) {
                (Some(content), _) => content.bytes(),
                (None, true) => entry.file_bytes as usize,
                (None, false) => 0,
            };
        }
        Ok(total)
    }

    fn view(entry: &Entry) -> Res<AttachmentView> {
        let data = entry.data()?;
        let content = data.content.as_deref();
        Ok(AttachmentView {
            id: entry.id.clone(),
            name: entry.name.clone(),
            kind: entry.kind,
            origin: entry.origin,
            file_bytes: entry.file_bytes,
            retention: data.retention,
            saved: data.saved,
            draft: data.scope == Scope::Draft,
            phase: data.phase.clone(),
            passages: content.map_or(0, |content| content.passages.len()),
            characters: content.map_or(0, PreparedContent::characters),
            pages: content.and_then(|content| content.pages),
            image: content
                .and_then(|content| content.image.as_ref())
                .map(|image| ImageView {
                    width: image.width,
                    height: image.height,
                    remote_eligible: image.bytes.len() <= agent_core::capability::MAX_IMAGE_BYTES,
                }),
            recognition: content.and_then(|content| content.recognition.clone()),
            notes: content.map_or_else(Vec::new, |content| content.notes.clone()),
        })
    }

    /// Remove every attachment of a deleted conversation from memory.
    pub(crate) fn purge_conversation(&self, conversation: ConversationId) {
        if let Ok(mut state) = self.state() {
            let scope = Scope::Conversation(conversation);
            state.entries.retain(|entry| {
                let owned = entry.data().is_ok_and(|data| data.scope == scope);
                if owned {
                    entry.cancellation.cancel();
                }
                !owned
            });
            state
                .queue
                .retain(|(entry, _)| !entry.cancellation.is_cancelled());
        }
    }

    /// Resolve the attachments a Chat turn names, binding draft attachments
    /// to `conversation`. Every identifier must belong to that conversation
    /// or to the current draft, and be ready.
    pub(crate) fn resolve(
        &self,
        conversation: ConversationId,
        ids: &[String],
    ) -> Res<(Vec<Resolved>, Vec<Arc<EntryHandle>>)> {
        self.expire(Instant::now())?;
        let mut resolved = Vec::new();
        let mut bind = Vec::new();
        for id in ids {
            let entry = self.find(id)?.ok_or_else(unavailable)?;
            let mut data = entry.data()?;
            match data.scope {
                Scope::Conversation(owner) if owner == conversation => {}
                Scope::Draft => bind.push(Arc::new(EntryHandle(Arc::clone(&entry)))),
                Scope::Conversation(_) => return Err(unavailable()),
            }
            let content = match (&data.phase, &data.content) {
                (AttachmentPhase::Ready, Some(content)) => Arc::clone(content),
                (AttachmentPhase::Expired, _) => {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        format!(
                            "\u{201c}{}\u{201d} expired after two hours without use. Reattach it to ask about it again.",
                            entry.name
                        ),
                    ));
                }
                (phase, _) if phase.preparing() => {
                    return Err(InferenceError::new(
                        ErrorCode::Busy,
                        format!("\u{201c}{}\u{201d} is still being prepared.", entry.name),
                    ));
                }
                _ => {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        format!(
                            "\u{201c}{}\u{201d} couldn't be prepared. Remove it before sending.",
                            entry.name
                        ),
                    ));
                }
            };
            data.last_used = Instant::now();
            resolved.push(Resolved {
                id: entry.id.clone(),
                name: entry.name.clone(),
                kind: entry.kind,
                origin: entry.origin,
                retention: data.retention,
                content,
            });
        }
        Ok((resolved, bind))
    }

    /// The classes a search query built from these attachments' text
    /// carries: the user's input plus each attachment's text classes. Every
    /// attachment must be in the draft or in `conversation`; nothing is
    /// bound, read or sent.
    pub(crate) fn query_labels(&self, conversation: ConversationId, ids: &[String]) -> Res<Labels> {
        if ids.len() > MAX_SCOPE_ATTACHMENTS {
            return Err(unavailable());
        }
        let mut labels = Labels::from_source(agent_core::classification::USER_INPUT);
        for id in ids {
            let entry = self.find(id)?.ok_or_else(unavailable)?;
            let scope = entry.data()?.scope;
            if !matches!(scope, Scope::Draft) && scope != Scope::Conversation(conversation) {
                return Err(unavailable());
            }
            // Visual-assisted search starts only from images.
            if !entry.kind.is_image() {
                return Err(limit(
                    "A search query can start only from an attached image's recognized text.",
                ));
            }
            labels.inherit(&entry.origin.text_labels(entry.kind));
        }
        Ok(labels)
    }

    /// The text of one passage of an attachment owned by `conversation`.
    pub(crate) fn passage(
        &self,
        conversation: ConversationId,
        id: &str,
        ordinal: u32,
    ) -> Res<Option<String>> {
        let Some(entry) = self.find(id)? else {
            return Ok(None);
        };
        let data = entry.data()?;
        if data.scope != Scope::Conversation(conversation) {
            return Ok(None);
        }
        Ok(data
            .content
            .as_ref()
            .and_then(|content| content.passages.get(ordinal as usize))
            .map(|passage| passage.text.clone()))
    }

    /// The readable text of a ready attachment in `scope`, joined in order
    /// and bounded, for showing recognized text and starting a reviewed
    /// search query. Local only: nothing is sent or stored.
    pub(crate) fn text(&self, scope: Option<ConversationId>, id: &str) -> Res<String> {
        const MAX_CHARS: usize = 20_000;
        let entry = self.find(id)?.ok_or_else(unavailable)?;
        let mut data = entry.data()?;
        if data.scope != Scope::from(scope) {
            return Err(unavailable());
        }
        let content = data.content.clone().ok_or_else(unavailable)?;
        data.last_used = Instant::now();
        let mut text = String::new();
        for passage in &content.passages {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&passage.text);
            if text.chars().count() >= MAX_CHARS {
                text = text.chars().take(MAX_CHARS).collect();
                break;
            }
        }
        Ok(text)
    }

    /// The validated image bytes and media type of a ready image attachment.
    pub(crate) fn preview(&self, id: &str) -> Option<(Vec<u8>, &'static str)> {
        let entry = self.find(id).ok()??;
        let data = entry.data().ok()?;
        let image = data.content.as_ref()?.image.as_ref()?;
        let mime = match entry.kind {
            AttachmentKind::Png => "image/png",
            AttachmentKind::Jpeg => "image/jpeg",
            _ => return None,
        };
        Some((image.bytes.clone(), mime))
    }
}

/// A draft attachment a turn bound to its conversation.
pub(crate) struct EntryHandle(Arc<Entry>);

fn snapshot(
    entry: &Entry,
    conversation: ConversationId,
    content: &PreparedContent,
) -> RetainedAttachment {
    RetainedAttachment {
        public_id: entry.id.clone(),
        conversation_id: conversation,
        name: entry.name.clone(),
        kind: entry.kind,
        origin: entry.origin,
        file_bytes: entry.file_bytes,
        fingerprint: content.fingerprint(),
        pages: content.pages,
        passages: content
            .passages
            .iter()
            .map(|passage| StoredPassage {
                source: passage.source,
                text: passage.text.clone(),
            })
            .collect(),
        image: content.image.as_ref().map(|image| StoredImage {
            bytes: image.bytes.clone(),
            width: image.width,
            height: image.height,
        }),
        recognition: content.recognition.clone(),
    }
}

fn policy_denied() -> AgentError {
    AgentError::new(
        AgentErrorCode::ToolFailed,
        "The intelligence policy denied this attachment operation.",
    )
}

fn retain_denied() -> InferenceError {
    InferenceError::new(
        ErrorCode::PolicyDenied,
        "The intelligence policy denied keeping this attachment.",
    )
}

/// Save an explicitly retained attachment, authorized for this one write.
fn retain(
    database: &Mutex<SqliteStore>,
    policy: &Policy,
    entry: &Entry,
    conversation: ConversationId,
    content: &PreparedContent,
) -> Res<()> {
    let session = Session::new(Cancellation::default());
    let mut labels = entry.origin.text_labels(entry.kind);
    if entry.kind.is_image() {
        labels.inherit(&entry.origin.image_labels());
    }
    let operation = Operation::local(
        &session,
        IMPORT,
        "attachment-retain",
        labels,
        Retention::SavedLocally,
    );
    let until = Instant::now() + Duration::from_secs(30);
    let grant = policy
        .grant(Permission::Retain, &operation, until)
        .map_err(|_| retain_denied())?;
    policy
        .authorize(Permission::Retain, &operation, grant)
        .map_err(|_| retain_denied())?;
    let record = snapshot(entry, conversation, content);
    let result = database
        .lock()
        .map_err(|_| store_error())?
        .retain_attachment(&record);
    let _ = policy.revoke(grant);
    result
}

fn prepare_one(
    policy: &Policy,
    entry: &Entry,
    file: Box<dyn AttachmentFile>,
) -> AgentResult<PreparedContent> {
    let started = Instant::now();
    let deadline = Duration::from_secs(PREPARE_DEADLINE_SECONDS);
    let check = || {
        if entry.cancellation.is_cancelled() {
            return Err(cancelled());
        }
        if started.elapsed() >= deadline {
            return Err(AgentError::new(
                AgentErrorCode::TimedOut,
                "Preparing the attachment took too long.",
            ));
        }
        Ok(())
    };
    // Selecting a file authorizes local preparation only.
    let session = Session::new(entry.cancellation.clone());
    let until = started + deadline;
    let observation = Operation::local(
        &session,
        IMPORT,
        "attachment",
        match entry.origin {
            AttachmentOrigin::File => Labels::from_source(ATTACHMENT),
            AttachmentOrigin::Camera => entry.origin.image_labels(),
        },
        Retention::Session,
    );
    let observe = policy
        .grant(Permission::Observe, &observation, until)
        .map_err(|_| policy_denied())?;
    policy
        .authorize(Permission::Observe, &observation, observe)
        .map_err(|_| policy_denied())?;
    let processing = Operation::local(
        &session,
        IMPORT,
        "attachment",
        entry.origin.text_labels(entry.kind),
        Retention::Session,
    );
    let process = policy
        .grant(Permission::Process, &processing, until)
        .map_err(|_| policy_denied())?;
    policy
        .authorize(Permission::Process, &processing, process)
        .map_err(|_| policy_denied())?;
    let stage = |stage: Stage| {
        if let Ok(mut data) = entry.data()
            && data.phase.preparing()
        {
            data.phase = match stage {
                Stage::Reading => AttachmentPhase::Reading,
                Stage::Extracting => AttachmentPhase::Extracting,
                Stage::Recognizing => AttachmentPhase::Recognizing,
            };
        }
    };
    let content = file.prepare(&stage, &check)?;
    check()?;
    if content.passages.is_empty() && content.image.is_none() {
        return Err(AgentError::new(
            AgentErrorCode::InvalidArguments,
            "The file has no readable text.",
        ));
    }
    if content.bytes() > MAX_SESSION_BYTES {
        return Err(AgentError::new(
            AgentErrorCode::KnowledgeLimit,
            "The attachment's content is too large.",
        ));
    }
    Ok(content)
}

fn failure_message(error: &AgentError) -> String {
    error.message.to_owned()
}

/// Run queued preparations one at a time on the host's blocking pool.
fn spawn_worker(
    platform: Arc<dyn Platform>,
    store: Arc<AttachmentStore>,
    policy: Arc<Policy>,
    database: Arc<Mutex<SqliteStore>>,
) {
    let runner = Arc::clone(&platform);
    platform.spawn(Box::pin(async move {
        loop {
            let next = match store.state() {
                Ok(mut state) => match state.queue.pop_front() {
                    Some(next) => Some(next),
                    None => {
                        state.working = false;
                        None
                    }
                },
                Err(_) => None,
            };
            let Some((entry, file)) = next else {
                break;
            };
            if entry.cancellation.is_cancelled() {
                if let Ok(mut data) = entry.data() {
                    data.phase = AttachmentPhase::Cancelled;
                }
                continue;
            }
            if let Ok(mut data) = entry.data() {
                data.phase = AttachmentPhase::Reading;
            }
            let preparing = Arc::clone(&policy);
            let job = Arc::clone(&entry);
            let outcome =
                crate::run_blocking(runner.as_ref(), move || prepare_one(&preparing, &job, file))
                    .await
                    .unwrap_or_else(|| {
                        Err(AgentError::new(
                            AgentErrorCode::ToolFailed,
                            "The attachment worker stopped unexpectedly.",
                        ))
                    });
            let save = {
                let Ok(mut data) = entry.data() else {
                    continue;
                };
                match outcome {
                    _ if entry.cancellation.is_cancelled() => {
                        data.phase = AttachmentPhase::Cancelled;
                        None
                    }
                    Ok(content) => {
                        let content = Arc::new(content);
                        data.content = Some(Arc::clone(&content));
                        data.phase = AttachmentPhase::Ready;
                        data.last_used = Instant::now();
                        match (data.retention, data.scope) {
                            (AttachmentRetention::Conversation, Scope::Conversation(id)) => {
                                Some((id, content))
                            }
                            _ => None,
                        }
                    }
                    Err(error) if error.code == AgentErrorCode::Cancelled => {
                        data.phase = AttachmentPhase::Cancelled;
                        None
                    }
                    Err(error) => {
                        data.phase = AttachmentPhase::Failed {
                            message: failure_message(&error),
                        };
                        None
                    }
                }
            };
            if let Some((conversation, content)) = save {
                let database = Arc::clone(&database);
                let policy = Arc::clone(&policy);
                let job = Arc::clone(&entry);
                let saved = crate::run_blocking(runner.as_ref(), move || {
                    retain(&database, &policy, &job, conversation, &content)
                })
                .await
                .unwrap_or_else(|| Err(store_error()));
                if let Ok(mut data) = entry.data() {
                    match saved {
                        Ok(()) => data.saved = true,
                        // Fall back to temporary rather than claim retention.
                        Err(_) => data.retention = AttachmentRetention::Temporary,
                    }
                }
            }
        }
    }));
}

impl Engine {
    fn attachment_scope_exists(&self, scope: Scope) -> Res<()> {
        if let Scope::Conversation(id) = scope {
            crate::database(&self.database)
                .and_then(|db| agent_core::chat::ChatStore::get(&*db, id))
                .map_err(|_| unavailable())?;
        }
        Ok(())
    }

    /// Register pinned selections for a draft (`None`) or conversation and
    /// queue their local preparation. Limits are checked before reading.
    pub fn attach_files(
        &self,
        conversation: Option<ConversationId>,
        retention: AttachmentRetention,
        files: Vec<Box<dyn AttachmentFile>>,
    ) -> Res<Vec<AttachmentView>> {
        let scope = Scope::from(conversation);
        self.attachment_scope_exists(scope)?;
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let store = &self.attachments;
        store.expire(Instant::now())?;
        let mut views = Vec::new();
        let start = {
            let mut state = store.state()?;
            let in_scope = state
                .entries
                .iter()
                .filter(|entry| entry.data().is_ok_and(|data| data.scope == scope))
                .count();
            if in_scope + files.len() > MAX_SCOPE_ATTACHMENTS {
                return Err(limit(format!(
                    "Attach at most {MAX_SCOPE_ATTACHMENTS} files at a time. Remove some first."
                )));
            }
            if state.entries.len() + files.len() > MAX_SESSION_ATTACHMENTS {
                return Err(limit(
                    "This session holds the most attachments it can. Remove some, or delete conversations you no longer need.",
                ));
            }
            let incoming: u64 = files.iter().map(|file| file.file_bytes()).sum();
            if AttachmentStore::held_bytes(&state.entries)? as u64 + incoming
                > MAX_SESSION_BYTES as u64
            {
                return Err(limit(
                    "These files would exceed the attachment memory limit (64 MB). Remove some attachments first.",
                ));
            }
            for file in files {
                if file.file_bytes() > file.kind().max_file_bytes() as u64 {
                    return Err(limit(
                        "The file is larger than attachments of this type allow.",
                    ));
                }
                let entry = Arc::new(Entry {
                    id: new_id()?,
                    name: file.name().to_owned(),
                    kind: file.kind(),
                    origin: file.origin(),
                    file_bytes: file.file_bytes(),
                    cancellation: Cancellation::default(),
                    data: Mutex::new(EntryData {
                        scope,
                        retention,
                        phase: AttachmentPhase::Queued,
                        content: None,
                        last_used: Instant::now(),
                        saved: false,
                    }),
                });
                views.push(AttachmentStore::view(&entry)?);
                state.entries.push(Arc::clone(&entry));
                state.queue.push_back((entry, file));
            }
            !std::mem::replace(&mut state.working, true)
        };
        if start {
            spawn_worker(
                Arc::clone(&self.platform),
                Arc::clone(&self.attachments),
                Arc::clone(&self.policy),
                Arc::clone(&self.database),
            );
        }
        Ok(views)
    }

    /// Attachments of a draft or conversation. Opening a conversation loads
    /// the attachments kept with it; other conversations' kept attachments
    /// leave memory (they remain saved).
    pub async fn attachments(
        &self,
        conversation: Option<ConversationId>,
    ) -> Res<Vec<AttachmentView>> {
        let scope = Scope::from(conversation);
        self.attachments.expire(Instant::now())?;
        if let Scope::Conversation(id) = scope {
            let database = Arc::clone(&self.database);
            let retained = self
                .blocking(move || crate::database(&database)?.retained_attachments(id))
                .await
                .ok_or_else(store_error)??;
            let mut state = self.attachments.state()?;
            state.entries.retain(|entry| {
                entry
                    .data()
                    .map(|data| !data.saved || data.scope == scope)
                    .unwrap_or(false)
            });
            for row in retained {
                if state.entries.iter().any(|entry| entry.id == row.public_id) {
                    continue;
                }
                let content = PreparedContent {
                    passages: row
                        .passages
                        .into_iter()
                        .map(|passage| PreparedText {
                            source: passage.source,
                            text: passage.text,
                        })
                        .collect(),
                    pages: row.pages,
                    image: row.image.map(|image| PreparedImage {
                        bytes: image.bytes,
                        width: image.width,
                        height: image.height,
                    }),
                    recognition: row.recognition,
                    notes: Vec::new(),
                };
                state.entries.push(Arc::new(Entry {
                    id: row.public_id,
                    name: row.name,
                    kind: row.kind,
                    origin: row.origin,
                    file_bytes: row.file_bytes,
                    cancellation: Cancellation::default(),
                    data: Mutex::new(EntryData {
                        scope,
                        retention: AttachmentRetention::Conversation,
                        phase: AttachmentPhase::Ready,
                        content: Some(Arc::new(content)),
                        last_used: Instant::now(),
                        saved: true,
                    }),
                }));
            }
        }
        let state = self.attachments.state()?;
        state
            .entries
            .iter()
            .filter(|entry| entry.data().is_ok_and(|data| data.scope == scope))
            .map(|entry| AttachmentStore::view(entry))
            .collect()
    }

    /// Stop preparing an attachment. Returns whether it was still preparing.
    pub fn cancel_attachment(&self, conversation: Option<ConversationId>, id: &str) -> Res<bool> {
        let entry = self.attachments.owned(Scope::from(conversation), id)?;
        let mut data = entry.data()?;
        if !data.phase.preparing() {
            return Ok(false);
        }
        entry.cancellation.cancel();
        if data.phase == AttachmentPhase::Queued {
            data.phase = AttachmentPhase::Cancelled;
        }
        Ok(true)
    }

    /// Remove an attachment: stop it, discard its content and delete any
    /// kept snapshot. Answers already given are not changed.
    pub async fn remove_attachment(
        &self,
        conversation: Option<ConversationId>,
        id: &str,
    ) -> Res<()> {
        let scope = Scope::from(conversation);
        let entry = self.attachments.owned(scope, id)?;
        entry.cancellation.cancel();
        let saved = entry.data()?.saved;
        if let (true, Scope::Conversation(owner)) = (saved, scope) {
            let public = entry.id.clone();
            let database = Arc::clone(&self.database);
            self.blocking(move || {
                crate::database(&database)?.remove_retained_attachment(owner, &public)
            })
            .await
            .ok_or_else(store_error)??;
        }
        let mut state = self.attachments.state()?;
        state.entries.retain(|candidate| candidate.id != entry.id);
        state
            .queue
            .retain(|(candidate, _)| candidate.id != entry.id);
        drop(state);
        if let Ok(mut data) = entry.data() {
            data.content = None;
            data.phase = AttachmentPhase::Cancelled;
        }
        Ok(())
    }

    /// Change how long an attachment is kept. Keeping requires the user's
    /// explicit choice; it saves a snapshot once the attachment belongs to
    /// a conversation. Making it temporary deletes the snapshot.
    pub async fn set_attachment_retention(
        &self,
        conversation: Option<ConversationId>,
        id: &str,
        retention: AttachmentRetention,
    ) -> Res<AttachmentView> {
        let scope = Scope::from(conversation);
        let entry = self.attachments.owned(scope, id)?;
        let (content, saved) = {
            let mut data = entry.data()?;
            if data.phase == AttachmentPhase::Expired {
                return Err(InferenceError::new(
                    ErrorCode::InvalidConfig,
                    "This attachment expired. Reattach it to keep it.",
                ));
            }
            data.retention = retention;
            (data.content.clone(), data.saved)
        };
        match (retention, scope) {
            (AttachmentRetention::Conversation, Scope::Conversation(owner)) => {
                if let Some(content) = content
                    && !saved
                {
                    let database = Arc::clone(&self.database);
                    let policy = Arc::clone(&self.policy);
                    let job = Arc::clone(&entry);
                    let result = self
                        .blocking(move || retain(&database, &policy, &job, owner, &content))
                        .await
                        .unwrap_or_else(|| Err(store_error()));
                    let mut data = entry.data()?;
                    match result {
                        Ok(()) => data.saved = true,
                        Err(error) => {
                            data.retention = AttachmentRetention::Temporary;
                            return Err(error);
                        }
                    }
                }
            }
            (AttachmentRetention::Temporary, Scope::Conversation(owner)) if saved => {
                let public = entry.id.clone();
                let database = Arc::clone(&self.database);
                self.blocking(move || {
                    crate::database(&database)?.remove_retained_attachment(owner, &public)
                })
                .await
                .ok_or_else(store_error)??;
                let mut data = entry.data()?;
                data.saved = false;
                data.last_used = Instant::now();
            }
            _ => {}
        }
        AttachmentStore::view(&entry)
    }

    /// The validated image of an attachment, for the main window preview.
    pub fn attachment_preview(&self, id: &str) -> Option<(Vec<u8>, &'static str)> {
        self.attachments.preview(id)
    }

    /// Bind draft attachments a turn used to its conversation, saving the
    /// ones the user chose to keep.
    pub(crate) async fn bind_attachments(
        database: Arc<Mutex<SqliteStore>>,
        policy: Arc<Policy>,
        platform: Arc<dyn Platform>,
        conversation: ConversationId,
        handles: Vec<Arc<EntryHandle>>,
    ) {
        for handle in handles {
            let entry = Arc::clone(&handle.0);
            let content = {
                let Ok(mut data) = entry.data() else { continue };
                if data.scope != Scope::Draft {
                    continue;
                }
                data.scope = Scope::Conversation(conversation);
                (data.retention == AttachmentRetention::Conversation)
                    .then(|| data.content.clone())
                    .flatten()
            };
            if let Some(content) = content {
                let database = Arc::clone(&database);
                let policy = Arc::clone(&policy);
                let job = Arc::clone(&entry);
                let saved = crate::run_blocking(platform.as_ref(), move || {
                    retain(&database, &policy, &job, conversation, &content)
                })
                .await
                .unwrap_or_else(|| Err(store_error()));
                if let Ok(mut data) = entry.data() {
                    match saved {
                        Ok(()) => data.saved = true,
                        Err(_) => data.retention = AttachmentRetention::Temporary,
                    }
                }
            }
        }
    }

    /// The readable text of a ready attachment in the draft (`None`) or an
    /// open conversation.
    pub fn attachment_text(&self, conversation: Option<ConversationId>, id: &str) -> Res<String> {
        self.attachments.text(conversation, id)
    }

    /// The text of a supplied attachment passage of `message`, if the
    /// attachment is still available in that message's conversation.
    pub async fn attachment_evidence(
        &self,
        message: agent_core::chat::MessageId,
        citation: &str,
    ) -> Res<Option<String>> {
        let database = Arc::clone(&self.database);
        let found = self
            .blocking(move || crate::database(&database)?.attachment_citation(message))
            .await
            .ok_or_else(store_error)??;
        let Some((conversation, context)) = found else {
            return Ok(None);
        };
        let Some(record) = context.attachments else {
            return Ok(None);
        };
        let supplied = context.fitted.map_or(0, |usage| usage.sources);
        let Some(index) = citation
            .strip_prefix('S')
            .and_then(|digits| digits.parse::<usize>().ok())
            .filter(|number| (1..=supplied.min(record.passages.len())).contains(number))
        else {
            return Ok(None);
        };
        let passage = &record.passages[index - 1];
        self.attachments
            .passage(conversation, &passage.attachment, passage.ordinal)
    }
}

/// Passages chosen for one answer, in citation and fitting priority order.
pub(crate) struct Selection {
    /// `(attachment index, passage ordinal)` pairs.
    pub picks: Vec<(usize, u32)>,
    pub task: AttachmentTask,
    pub no_match: bool,
}

fn normalized(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Evenly spread ordinals of `count` passages, ordered so any prefix stays
/// spread: first, middle, quarters, eighths and so on.
fn spread(count: u32) -> Vec<u32> {
    let mut order = Vec::new();
    if count == 0 {
        return order;
    }
    order.push(0);
    let mut denominator = 2u64;
    while (order.len() as u32) < count && denominator <= u64::from(count) * 2 {
        for numerator in (1..denominator).step_by(2) {
            let ordinal = (numerator * u64::from(count) / denominator) as u32;
            if !order.contains(&ordinal) {
                order.push(ordinal);
            }
        }
        denominator *= 2;
    }
    for ordinal in 0..count {
        if !order.contains(&ordinal) {
            order.push(ordinal);
        }
    }
    order
}

/// Interleave per-attachment priority lists so every attachment is
/// represented before any gets a second passage, then cut at the budget.
fn interleave(
    lists: Vec<Vec<u32>>,
    contents: &[&PreparedContent],
    budget: usize,
) -> Vec<(usize, u32)> {
    let mut picks = Vec::new();
    let mut used = 0;
    let mut cursors = vec![0usize; lists.len()];
    loop {
        let mut progressed = false;
        for (attachment, list) in lists.iter().enumerate() {
            if picks.len() >= MAX_ATTACHMENT_SOURCES {
                return picks;
            }
            let Some(&ordinal) = list.get(cursors[attachment]) else {
                continue;
            };
            let cost = contents[attachment].passages[ordinal as usize].text.len() + 64;
            if !picks.is_empty() && used + cost > budget {
                return picks;
            }
            cursors[attachment] += 1;
            used += cost;
            picks.push((attachment, ordinal));
            progressed = true;
        }
        if !progressed {
            return picks;
        }
    }
}

/// Choose evidence deterministically. Short attachments are supplied
/// whole; otherwise passages are ranked against the question, searched for
/// literal text, or sampled representatively for an overview. `budget` is
/// the evidence size, in bytes, the target model's context can afford.
pub(crate) fn select(
    contents: &[&PreparedContent],
    intent: ChatIntent,
    query: &str,
    budget: usize,
) -> Selection {
    let total: usize = contents.iter().map(|content| content.passages.len()).sum();
    let bytes: usize = contents
        .iter()
        .flat_map(|content| &content.passages)
        .map(|passage| passage.text.len() + 64)
        .sum();
    if intent == ChatIntent::Search {
        let phrase = normalized(query);
        let lists = contents
            .iter()
            .map(|content| {
                content
                    .passages
                    .iter()
                    .enumerate()
                    .filter(|(_, passage)| {
                        !phrase.is_empty() && normalized(&passage.text).contains(&phrase)
                    })
                    .map(|(ordinal, _)| ordinal as u32)
                    .collect()
            })
            .collect();
        return Selection {
            picks: interleave(lists, contents, budget),
            task: AttachmentTask::Search,
            no_match: false,
        };
    }
    if total <= MAX_ATTACHMENT_SOURCES && bytes <= budget {
        let picks = contents
            .iter()
            .enumerate()
            .flat_map(|(attachment, content)| {
                (0..content.passages.len() as u32).map(move |ordinal| (attachment, ordinal))
            })
            .collect();
        return Selection {
            picks,
            task: AttachmentTask::Complete,
            no_match: false,
        };
    }
    let representative = |no_match| Selection {
        picks: interleave(
            contents
                .iter()
                .map(|content| spread(content.passages.len() as u32))
                .collect(),
            contents,
            budget,
        ),
        task: AttachmentTask::Representative,
        no_match,
    };
    if intent == ChatIntent::Question {
        let terms = crate::research::rank::terms(query);
        let mut found = false;
        let lists = contents
            .iter()
            .map(|content| {
                let mut ranked: Vec<(f64, u32)> = content
                    .passages
                    .iter()
                    .enumerate()
                    .map(|(ordinal, passage)| {
                        (
                            crate::research::rank::coverage(&passage.text, &terms),
                            ordinal as u32,
                        )
                    })
                    .filter(|(score, _)| *score > 0.0)
                    .collect();
                ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
                found |= !ranked.is_empty();
                ranked.into_iter().map(|(_, ordinal)| ordinal).collect()
            })
            .collect();
        if found {
            return Selection {
                picks: interleave(lists, contents, budget),
                task: AttachmentTask::Relevant,
                no_match: false,
            };
        }
        return representative(true);
    }
    representative(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(texts: &[&str]) -> PreparedContent {
        PreparedContent {
            passages: texts
                .iter()
                .enumerate()
                .map(|(index, text)| PreparedText {
                    source: PassageSource::Page {
                        page: index as u32 + 1,
                    },
                    text: (*text).to_owned(),
                })
                .collect(),
            pages: None,
            image: None,
            recognition: None,
            notes: Vec::new(),
        }
    }

    #[test]
    fn spread_prefixes_stay_representative() {
        assert_eq!(spread(1), vec![0]);
        assert_eq!(&spread(8)[..4], &[0, 4, 2, 6]);
        let all = spread(13);
        assert_eq!(all.len(), 13);
        let mut sorted = all.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..13).collect::<Vec<_>>());
    }

    #[test]
    fn short_content_is_complete_and_large_content_is_ranked_balanced_or_sampled() {
        let short = document(&["The pump runs at noon.", "Valves close at dusk."]);
        let chosen = select(
            &[&short],
            ChatIntent::Question,
            "When does the pump run?",
            4096,
        );
        assert_eq!(chosen.task, AttachmentTask::Complete);
        assert_eq!(chosen.picks, vec![(0, 0), (0, 1)]);

        let filler = "Unrelated synthetic filler text about nothing in particular. ".repeat(20);
        let mut pages: Vec<String> = (0..30).map(|_| filler.clone()).collect();
        pages[17] = "The turbine maintenance interval is 400 hours.".into();
        let large = document(&pages.iter().map(String::as_str).collect::<Vec<_>>());
        let other = document(&["Turbine maintenance in the second manual happens monthly."; 1]);
        let chosen = select(
            &[&large, &other],
            ChatIntent::Question,
            "turbine maintenance interval",
            4096,
        );
        assert_eq!(chosen.task, AttachmentTask::Relevant);
        // Both attachments are represented first, the best passage leading.
        assert_eq!(&chosen.picks[..2], &[(0, 17), (1, 0)]);

        let chosen = select(&[&large], ChatIntent::Overview, "Summarize this", 4096);
        assert_eq!(chosen.task, AttachmentTask::Representative);
        assert!(chosen.picks.len() < 30 && chosen.picks.len() >= 2);
        assert_eq!(chosen.picks[0], (0, 0));

        let chosen = select(&[&large], ChatIntent::Question, "zebra migration", 4096);
        assert!(chosen.no_match);
        assert_eq!(chosen.task, AttachmentTask::Representative);

        let chosen = select(&[&large, &other], ChatIntent::Search, "400 hours", 4096);
        assert_eq!(chosen.picks, vec![(0, 17)]);
        let none = select(&[&large], ChatIntent::Search, "absent phrase", 4096);
        assert!(none.picks.is_empty());
        // Never more than the citation slots, whatever the budget.
        let chosen = select(&[&large], ChatIntent::Overview, "overview", usize::MAX);
        assert_eq!(chosen.picks.len(), MAX_ATTACHMENT_SOURCES);
    }
}
