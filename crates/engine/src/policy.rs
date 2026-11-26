//! Deterministic authority for intelligence data operations. The existing
//! PermissionEngine remains the sole authority for Agent actions.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use agent_core::{
    capability::{
        AGENT_DECISION, CapabilityId, CapabilityRegistry, EMBED, GENERATE, IMPORT, Locality,
        RETRIEVE, Retention,
    },
    classification::{
        AGENT_CONTEXT, DataClasses, EMBEDDING, HISTORY, LIBRARY, Labels, MEMORY, MODEL_OUTPUT,
        PDF_EXCERPT, QUERY, SYSTEM_TEXT, Sensitivity, TEXT_EXCERPT, USER_INPUT,
    },
    inference::Cancellation,
};

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
pub struct Destination {
    pub id: &'static str,
    /// Exact transport target registered by the trusted engine.
    pub endpoint: &'static str,
    pub locality: Locality,
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
        retention: Retention,
    ) -> Self {
        Self {
            key: Arc::new(()),
            session: session.clone(),
            capability,
            purpose,
            labels,
            intended_locality,
            locality: destination.map_or(Locality::OnDevice, |target| target.locality),
            connectivity,
            destination,
            retention,
        }
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
    destinations: BTreeMap<&'static str, Destination>,
    local_only: bool,
    grants: Mutex<Grants>,
}

impl Default for Policy {
    fn default() -> Self {
        Self::new(
            CapabilityRegistry::new([GENERATE, AGENT_DECISION, EMBED, IMPORT, RETRIEVE]),
            DataClasses::default(),
            [],
            true,
        )
    }
}

impl Policy {
    pub fn new(
        capabilities: CapabilityRegistry,
        classes: DataClasses,
        destinations: impl IntoIterator<Item = Destination>,
        local_only: bool,
    ) -> Self {
        let mut known = BTreeMap::new();
        let mut conflicts = std::collections::BTreeSet::new();
        for destination in destinations {
            if destination.id.is_empty()
                || destination.endpoint.is_empty()
                || destination.locality == Locality::OnDevice
            {
                continue;
            }
            if known
                .insert(destination.id, destination)
                .is_some_and(|other| other != destination)
            {
                conflicts.insert(destination.id);
            }
        }
        known.retain(|id, _| !conflicts.contains(id));
        Self {
            capabilities,
            classes,
            destinations: known,
            local_only,
            grants: Mutex::default(),
        }
    }

    fn check(&self, operation: &Operation) -> Result<(), Denial> {
        if operation.session.key.load(Ordering::Acquire) || operation.cancellation().is_cancelled()
        {
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
        if operation.locality != operation.intended_locality
            || (self.local_only && operation.locality != Locality::OnDevice)
        {
            return Err(Denial::Locality);
        }
        match (operation.destination, operation.connectivity) {
            (None, Connectivity::Offline) => Ok(()),
            (Some(target), Connectivity::ThroughBoundary)
                if self.destinations.get(target.id) == Some(&target) =>
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
        self.check(operation)?;
        if permission == Permission::Share {
            self.check_share(operation)?;
        }
        if until <= Instant::now() {
            return Err(Denial::Expired);
        }
        let mut grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
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

    /// Final check and handoff share the revoke lock. The handoff must be one
    /// bounded transmission commitment, never a response wait or redirect.
    pub(crate) fn transmit<T>(
        &self,
        operation: &Operation,
        process: GrantId,
        share: GrantId,
        send: impl FnOnce() -> T,
    ) -> Result<T, Denial> {
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
        Ok(send())
    }

    pub(crate) fn accept_response(
        &self,
        operation: &Operation,
        process: GrantId,
        share: GrantId,
    ) -> Result<(), Denial> {
        let grants = self.grants.lock().map_err(|_| Denial::Unavailable)?;
        self.check(operation)?;
        let now = Instant::now();
        verify_grant(&grants, Permission::Process, operation, process, now)?;
        let grant = grants.entries.get(&share).ok_or(Denial::MissingGrant)?;
        if !grant.used {
            return Err(Denial::Scope);
        }
        check_liveness(grant, now)
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
