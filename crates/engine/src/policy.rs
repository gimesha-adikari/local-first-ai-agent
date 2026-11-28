//! Deterministic authority for intelligence data operations. The existing
//! PermissionEngine remains the sole authority for Agent actions.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use agent_core::{
    capability::{
        AGENT_DECISION, CAMERA_SNAPSHOT, CapabilityId, CapabilityRegistry, DISCOVER,
        DOWNLOAD_MODEL, EMBED, GENERATE, IMPORT, Locality, RETRIEVE, Retention, WEB_PAGE,
        WEB_SEARCH,
    },
    classification::{
        AGENT_CONTEXT, ATTACHMENT, ATTACHMENT_IMAGE, ATTACHMENT_TEXT, CAMERA_IMAGE, CAMERA_TEXT,
        CLIPBOARD_TEXT, DataClasses, EMBEDDING, HISTORY, LIBRARY, Labels, MEMORY, MODEL_ARTIFACT,
        MODEL_OUTPUT, PDF_EXCERPT, PUBLIC_WEB, QUERY, SCREEN_IMAGE, SCREEN_TEXT, SYSTEM_TEXT,
        Sensitivity, TEXT_EXCERPT, USER_INPUT,
    },
    inference::Cancellation,
};

pub use crate::network::{Destination, RequestBinding};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Observe,
    Process,
    Retain,
    Share,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    UnknownCapability,
    UnknownDataClass,
    UnknownDestination,
    UngovernedProvider,
    Locality,
    Cancelled,
    MissingGrant,
    Expired,
    Revoked,
    Scope,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    Offline,
    ThroughBoundary,
    Unverifiable,
}

/// Issued by the engine for the life of one request or session. Its caller
/// keeps the existing operation lease until persistence and streaming finish.
#[derive(Clone)]
pub struct Session {
    key: Arc<AtomicBool>,
    cancellation: Cancellation,
}

impl Session {
    pub fn new(cancellation: Cancellation) -> Self {
        Self {
            key: Arc::new(AtomicBool::new(false)),
            cancellation,
        }
    }

    pub fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.key.load(Ordering::Acquire) || self.cancellation.is_cancelled()
    }

    pub fn close(&self) {
        self.key.store(true, Ordering::Release);
    }
}

/// Each scope owns its identity and freezes policy-relevant metadata. The
/// operation can be moved across threads. A second instance requires its own
/// grants even if every metadata field matches.
pub struct Operation {
    key: Arc<()>,
    session: Session,
    capability: CapabilityId,
    purpose: &'static str,
    labels: Labels,
    intended_locality: Locality,
    locality: Locality,
    connectivity: Connectivity,
    destination: Option<Destination>,
    request: Option<RequestBinding>,
    retention: Retention,
}

impl Operation {
    pub fn local(
        session: &Session,
        capability: CapabilityId,
        purpose: &'static str,
        labels: Labels,
        retention: Retention,
    ) -> Self {
        Self::new(
            session,
            capability,
            purpose,
            labels,
            Locality::OnDevice,
            Connectivity::Offline,
            None,
            None,
            retention,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session: &Session,
        capability: CapabilityId,
        purpose: &'static str,
        labels: Labels,
        intended_locality: Locality,
        connectivity: Connectivity,
        destination: Option<Destination>,
        request: Option<RequestBinding>,
        retention: Retention,
    ) -> Self {
        Self {
            key: Arc::new(()),
            session: session.clone(),
            capability,
            purpose,
            labels,
            intended_locality,
            locality: destination
                .as_ref()
                .map_or(Locality::OnDevice, Destination::locality),
            connectivity,
            destination,
            request,
            retention,
        }
    }

    pub(crate) fn purpose(&self) -> &'static str {
        self.purpose
    }

    pub fn destination(&self) -> Option<&Destination> {
        self.destination.as_ref()
    }

    pub(crate) fn request_binding(&self) -> Option<&RequestBinding> {
        self.request.as_ref()
    }

    pub fn retention(&self) -> Retention {
        self.retention
    }

    pub fn labels(&self) -> &Labels {
        &self.labels
    }

    pub fn cancellation(&self) -> &Cancellation {
        self.session.cancellation()
    }
}

pub type GrantId = u64;

struct Grant {
    operation: std::sync::Weak<()>,
    session: std::sync::Weak<AtomicBool>,
    permission: Permission,
    until: Instant,
    revoked: bool,
    used: bool,
}

#[derive(Default)]
struct Grants {
    sequence: GrantId,
    entries: BTreeMap<GrantId, Grant>,
}

pub struct Policy {
    capabilities: CapabilityRegistry,
    classes: DataClasses,
    /// Registered by the trusted provider configuration. Changing or
    /// removing an entry invalidates every operation bound to the old value.
    destinations: RwLock<BTreeMap<Arc<str>, Destination>>,
    grants: Mutex<Grants>,
}

pub(crate) const VISUAL_SEARCH_PURPOSE: &str = "visual-search";

/// Classes a reviewed visual-assisted search query may carry besides the
/// user's input (Stage 04-E): text recognized from, or written by a model
/// about, a screen region, camera snapshot or attached image, and clipboard
/// text. Only the query text is ever sent; these classes record where it
/// came from. History, memories, library documents and document excerpts
/// are never admitted into a search request.
const VISUAL_QUERY_CLASSES: [agent_core::classification::DataClass; 8] = [
    CLIPBOARD_TEXT,
    SCREEN_TEXT,
    SCREEN_IMAGE,
    CAMERA_TEXT,
    CAMERA_IMAGE,
    ATTACHMENT,
    ATTACHMENT_TEXT,
    ATTACHMENT_IMAGE,
];

impl Default for Policy {
    fn default() -> Self {
        Self::new(
            CapabilityRegistry::new([
                GENERATE,
                AGENT_DECISION,
                EMBED,
                IMPORT,
                RETRIEVE,
                DISCOVER,
                DOWNLOAD_MODEL,
                WEB_SEARCH,
                WEB_PAGE,
                CAMERA_SNAPSHOT,
            ]),
            DataClasses::default(),
            crate::network::artifact::destinations(),
        )
    }
}

impl Policy {
    pub fn new(
        capabilities: CapabilityRegistry,
        classes: DataClasses,
        destinations: impl IntoIterator<Item = Destination>,
    ) -> Self {
        let mut known = BTreeMap::new();
        let mut conflicts = std::collections::BTreeSet::new();
        for destination in destinations {
            let id = destination.shared_id();
            if let Some(other) = known.insert(Arc::clone(&id), destination.clone())
                && other != destination
            {
                conflicts.insert(id);
            }
        }
        known.retain(|id, _| !conflicts.contains(id));
        Self {
            capabilities,
            classes,
            destinations: RwLock::new(known),
            grants: Mutex::default(),
        }
    }

    /// Register or replace a destination. Only the trusted provider
    /// configuration calls this. Operations bound to a replaced value fail
    /// their next check, including in-flight transport liveness checks.
    pub fn register_destination(&self, destination: Destination) -> Result<(), Denial> {
        self.destinations
            .write()
            .map_err(|_| Denial::Unavailable)?
            .insert(destination.shared_id(), destination);
        Ok(())
    }

    pub fn unregister_destination(&self, id: &str) -> Result<bool, Denial> {
        Ok(self
            .destinations
            .write()
            .map_err(|_| Denial::Unavailable)?
            .remove(id)
            .is_some())
    }

    pub fn registered_destination(&self, id: &str) -> Option<Destination> {
        self.destinations.read().ok()?.get(id).cloned()
    }

    fn check(&self, operation: &Operation) -> Result<(), Denial> {
        if operation.session.is_closed() {
            return Err(Denial::Cancelled);
        }
        if self
            .capabilities
            .resolve(operation.capability.as_str())
            .is_none()
        {
            return Err(Denial::UnknownCapability);
        }
        if operation.labels.is_empty()
            || operation
                .labels
                .ids()
                .any(|id| self.classes.sensitivity(id).is_none())
        {
            return Err(Denial::UnknownDataClass);
        }
        if operation
            .labels
            .ids()
            .any(|id| self.classes.sensitivity(id) == Some(Sensitivity::Secret))
        {
            return Err(Denial::Scope);
        }
        if operation.purpose.is_empty() {
            return Err(Denial::Scope);
        }
        if operation.connectivity == Connectivity::Unverifiable {
            return Err(Denial::UngovernedProvider);
        }
        if operation.locality != operation.intended_locality {
            return Err(Denial::Locality);
        }
        let artifact_binding = operation
            .request
            .as_ref()
            .is_some_and(RequestBinding::has_artifact_binding);
        if operation.capability == DOWNLOAD_MODEL {
            if !artifact_binding
                || operation.labels != Labels::from_source(MODEL_ARTIFACT)
                || operation.purpose != "model-download"
                || !operation.destination.as_ref().is_some_and(|target| {
                    target.id().starts_with("artifact/")
                        && target.credential().is_none()
                        && target.locality() == Locality::ThirdParty
                })
            {
                return Err(Denial::Scope);
            }
        } else if artifact_binding
            || operation
                .destination
                .as_ref()
                .is_some_and(|target| target.id().starts_with("artifact/"))
        {
            return Err(Denial::Scope);
        }
        let page_binding = operation
            .request
            .as_ref()
            .is_some_and(RequestBinding::has_page_binding);
        let public_page = operation.capability == WEB_PAGE;
        if public_page {
            // A dynamic public destination: never registered, so every
            // property that registration would fix is checked here.
            let labels_ok = operation.labels.contains(PUBLIC_WEB)
                && operation
                    .labels
                    .ids()
                    .all(|id| id == PUBLIC_WEB.id || id == USER_INPUT.id);
            if !page_binding
                || !labels_ok
                || operation.purpose != "web-page"
                || !operation
                    .request
                    .as_ref()
                    .is_some_and(RequestBinding::is_bodiless_get)
                || !operation.destination.as_ref().is_some_and(|target| {
                    target.id() == crate::network::page::DESTINATION
                        && target.credential().is_none()
                        && target.locality() == Locality::ThirdParty
                        && target.endpoint().scheme() == crate::network::Scheme::Https
                        && public_scope(target.scope())
                })
            {
                return Err(Denial::Scope);
            }
        } else if page_binding
            || operation
                .destination
                .as_ref()
                .is_some_and(|target| target.id().starts_with("web/"))
        {
            return Err(Denial::Scope);
        }
        if operation.capability == WEB_SEARCH {
            let visual = operation.purpose == VISUAL_SEARCH_PURPOSE;
            let generated = operation.purpose == crate::research::GENERATED_QUERY_PURPOSE;
            let labels = &operation.labels;
            // A model's visual description also inherits the built-in prompt
            // class. Keep that label without accepting standalone system text.
            let description = labels.contains(MODEL_OUTPUT)
                && [SCREEN_IMAGE, CAMERA_IMAGE, ATTACHMENT_IMAGE]
                    .into_iter()
                    .any(|class| labels.contains(class));
            if (!visual && !generated && operation.purpose != "web-search")
                || (generated
                    && (!labels.contains(MODEL_OUTPUT)
                        || !labels.contains(USER_INPUT)
                        || !labels.contains(SYSTEM_TEXT)))
                || !labels.contains(QUERY)
                || ((labels.contains(ATTACHMENT) || labels.contains(ATTACHMENT_TEXT))
                    && !labels.contains(ATTACHMENT_IMAGE))
                || !labels.ids().all(|id| {
                    id == USER_INPUT.id
                        || id == QUERY.id
                        || (generated
                            && (id == MODEL_OUTPUT.id
                                || id == SYSTEM_TEXT.id
                                || id == PUBLIC_WEB.id))
                        || (visual
                            && ((description && (id == MODEL_OUTPUT.id || id == SYSTEM_TEXT.id))
                                || VISUAL_QUERY_CLASSES.iter().any(|class| class.id == id)))
                })
                || !operation
                    .destination
                    .as_ref()
                    .is_some_and(|target| target.id().starts_with("search/"))
            {
                return Err(Denial::Scope);
            }
        }
        if operation.purpose == crate::research::MODEL_PURPOSE
            && (operation.capability != GENERATE
                || !operation.labels.ids().all(|id| {
                    [USER_INPUT, SYSTEM_TEXT, PUBLIC_WEB, MODEL_OUTPUT]
                        .iter()
                        .any(|class| id == class.id)
                }))
        {
            return Err(Denial::Scope);
        }
        match (
            &operation.destination,
            &operation.request,
            operation.connectivity,
        ) {
            (None, None, Connectivity::Offline) => Ok(()),
            (Some(_), Some(request), Connectivity::ThroughBoundary)
                if public_page && request.is_valid() =>
            {
                Ok(())
            }
            (Some(target), Some(request), Connectivity::ThroughBoundary)
                if request.is_valid()
                    && self
                        .destinations
                        .read()
                        .map_err(|_| Denial::Unavailable)?
                        .get(target.id())
                        == Some(target) =>
            {
                Ok(())
            }
            _ => Err(Denial::UnknownDestination),
        }
    }

    fn check_builtin_classes(&self, labels: &Labels) -> Result<(), Denial> {
        // Registering a class describes its sensitivity. Existing workflow
        // rules require a separate, explicit change before admitting it.
        let allowed = [
            USER_INPUT,
            CLIPBOARD_TEXT,
            SCREEN_IMAGE,
            SCREEN_TEXT,
            HISTORY,
            MEMORY,
            LIBRARY,
            PDF_EXCERPT,
            TEXT_EXCERPT,
            AGENT_CONTEXT,
            EMBEDDING,
            QUERY,
            MODEL_OUTPUT,
            SYSTEM_TEXT,
            // Admitted for Chat research answers (Stage 04-C).
            PUBLIC_WEB,
            // Admitted for Chat answers with attached files (Stage 04-D).
            ATTACHMENT,
            ATTACHMENT_TEXT,
            ATTACHMENT_IMAGE,
            // Admitted for confirmed camera snapshots (Stage 04-E).
            CAMERA_IMAGE,
            CAMERA_TEXT,
        ];
        if labels.ids().all(|id| {
            allowed.iter().any(|class| {
                class.id == id && self.classes.sensitivity(id) == Some(class.sensitivity)
            })
        }) {
            Ok(())
        } else {
            Err(Denial::MissingGrant)
        }
    }

    /// Built-in local workflows preserve existing behavior. Observation and
    /// sharing always require separate explicit authorization.
    pub fn authorize_local(
        &self,
        permission: Permission,
        operation: &Operation,
    ) -> Result<&'static str, Denial> {
        self.check(operation)?;
        if operation.destination.is_some() {
            return Err(Denial::Locality);
        }
        self.check_builtin_classes(&operation.labels)?;
        match (
            permission,
            operation.capability,
            operation.purpose,
            operation.retention,
        ) {
            (Permission::Process, GENERATE, "chat", Retention::SavedLocally)
            | (Permission::Retain, GENERATE, "chat", Retention::SavedLocally) => {
                Ok("builtin.chat/1")
            }
            (Permission::Process, GENERATE, "glance", Retention::Session) => Ok("builtin.glance/1"),
            (Permission::Process, GENERATE, "web-research-step", Retention::Session) => {
                Ok("builtin.web-research/1")
            }
            (Permission::Retain, GENERATE, "glance-remember", Retention::SavedLocally) => {
                Ok("explicit.glance-remember/1")
            }
            (Permission::Retain, AGENT_DECISION, "agent-answer", Retention::SavedLocally)
            | (Permission::Process, AGENT_DECISION, "agent", Retention::Session) => {
                Ok("builtin.agent/1")
            }
            (Permission::Process, EMBED, "query", Retention::Session)
            | (Permission::Process, RETRIEVE, "retrieval", Retention::Session)
            | (Permission::Process | Permission::Retain, EMBED, "index", Retention::SavedLocally) => {
                Ok("builtin.knowledge/1")
            }
            _ => Err(Denial::MissingGrant),
        }
    }

    /// Called only by the trusted consent owner. The grant cannot be reused
    /// for a different operation, payload, destination, purpose or session.
    pub fn grant(
        &self,
        permission: Permission,
        operation: &Operation,
        until: Instant,
    ) -> Result<GrantId, Denial> {
        let mut grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
        self.check(operation)?;
        if permission == Permission::Share {
            self.check_share(operation)?;
        }
        if until <= Instant::now() {
            return Err(Denial::Expired);
        }
        grants
            .entries
            .retain(|_, grant| grant.operation.strong_count() != 0);
        grants.sequence = grants.sequence.checked_add(1).ok_or(Denial::Unavailable)?;
        let id = grants.sequence;
        grants.entries.insert(
            id,
            Grant {
                operation: Arc::downgrade(&operation.key),
                session: Arc::downgrade(&operation.session.key),
                permission,
                until,
                revoked: false,
                used: false,
            },
        );
        Ok(id)
    }

    pub fn revoke(&self, id: GrantId) -> Result<bool, Denial> {
        let mut grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
        Ok(grants.entries.get_mut(&id).is_some_and(|grant| {
            let changed = !grant.revoked;
            grant.revoked = true;
            changed
        }))
    }

    /// Observe, process and retain each verify their own grant. Sharing can
    /// only be consumed by the network boundary.
    pub fn authorize(
        &self,
        permission: Permission,
        operation: &Operation,
        id: GrantId,
    ) -> Result<(), Denial> {
        if permission == Permission::Share {
            return Err(Denial::Scope);
        }
        let grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
        self.check(operation)?;
        verify_grant(&grants, permission, operation, id, Instant::now())
    }

    fn check_share(&self, operation: &Operation) -> Result<(), Denial> {
        if matches!(
            operation.retention,
            Retention::Unknown | Retention::SavedLocally
        ) || operation.destination.is_none()
            || operation
                .labels
                .ids()
                .any(|id| self.classes.sensitivity(id) == Some(Sensitivity::Secret))
        {
            return Err(Denial::Scope);
        }
        Ok(())
    }

    /// Check every condition for a handoff without consuming the sharing
    /// grant. The transport calls this before resolution and connection.
    pub(crate) fn admit(
        &self,
        operation: &Operation,
        process: GrantId,
        share: GrantId,
    ) -> Result<(), Denial> {
        let grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
        self.check(operation)?;
        self.check_share(operation)?;
        let now = Instant::now();
        verify_grant(&grants, Permission::Process, operation, process, now)?;
        verify_grant(&grants, Permission::Share, operation, share, now)
    }

    /// Final check and consumption of the one-handoff sharing grant, under
    /// the same lock as revocation. The transport writes the first request
    /// byte only after this succeeds. A revocation ordered after the commit
    /// stops further writes and discards the response.
    pub(crate) fn commit(
        &self,
        operation: &Operation,
        process: GrantId,
        share: GrantId,
    ) -> Result<(), Denial> {
        let mut grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
        self.check(operation)?;
        self.check_share(operation)?;
        let now = Instant::now();
        verify_grant(&grants, Permission::Process, operation, process, now)?;
        verify_grant(&grants, Permission::Share, operation, share, now)?;
        grants
            .entries
            .get_mut(&share)
            .ok_or(Denial::MissingGrant)?
            .used = true;
        Ok(())
    }

    /// Liveness after commit: the transport checks this while writing and
    /// before delivering each response piece.
    pub(crate) fn accept_response(
        &self,
        operation: &Operation,
        process: GrantId,
        share: GrantId,
    ) -> Result<(), Denial> {
        self.with_live_grants(operation, process, share, || ())
    }

    /// Order a single non-blocking handoff with grant revocation.
    pub(crate) fn with_live_grants<T>(
        &self,
        operation: &Operation,
        process: GrantId,
        share: GrantId,
        handoff: impl FnOnce() -> T,
    ) -> Result<T, Denial> {
        let grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
        self.check(operation)?;
        let now = Instant::now();
        verify_grant(&grants, Permission::Process, operation, process, now)?;
        let grant = grants.entries.get(&share).ok_or(Denial::MissingGrant)?;
        if !grant.used {
            return Err(Denial::Scope);
        }
        check_liveness(grant, now)?;
        Ok(handoff())
    }
}

/// Public pages connect only to globally routable addresses. Transport tests
/// route them to a loopback fixture instead.
fn public_scope(scope: &crate::network::NetworkScope) -> bool {
    match scope {
        crate::network::NetworkScope::Public => true,
        crate::network::NetworkScope::Loopback => cfg!(test),
        crate::network::NetworkScope::Pinned(_) => false,
    }
}

fn verify_grant(
    grants: &Grants,
    permission: Permission,
    operation: &Operation,
    id: GrantId,
    now: Instant,
) -> Result<(), Denial> {
    let grant = grants.entries.get(&id).ok_or(Denial::MissingGrant)?;
    if grant.permission != permission
        || !grant.operation.ptr_eq(&Arc::downgrade(&operation.key))
        || !grant
            .session
            .ptr_eq(&Arc::downgrade(&operation.session.key))
        || grant.used
    {
        return Err(Denial::Scope);
    }
    check_liveness(grant, now)
}

fn check_liveness(grant: &Grant, now: Instant) -> Result<(), Denial> {
    if grant.revoked {
        return Err(Denial::Revoked);
    }
    if now >= grant.until {
        return Err(Denial::Expired);
    }
    Ok(())
}
