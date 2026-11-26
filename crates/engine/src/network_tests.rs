use std::time::{Duration, Instant};

use agent_core::{
    capability::{CapabilityId, CapabilityRegistry, GENERATE, Locality, Retention},
    classification::{
        Classified, DataClass, DataClasses, Labels, MEMORY, SECRET, SYSTEM_TEXT, Sensitivity,
        USER_INPUT,
    },
    inference::Cancellation,
    intelligence::Transmission,
};

use super::*;
use crate::policy::{GrantId, Permission};

const FIRST: Destination = Destination {
    id: "fixture.first/1",
    endpoint: "https://first.invalid/v1/generate",
    locality: Locality::ThirdParty,
};
const SECOND: Destination = Destination {
    id: "fixture.second/1",
    endpoint: "https://second.invalid/v1/generate",
    locality: Locality::ThirdParty,
};

#[derive(Default)]
struct Mock {
    trace: Vec<(Destination, Vec<u8>)>,
    redirect: bool,
    cancel_after_send: bool,
}

impl Transport for Mock {
    type Response = &'static str;

    fn send_once(
        &mut self,
        destination: Destination,
        body: &[u8],
        cancellation: &Cancellation,
    ) -> TransportResult<Self::Response> {
        if cancellation.is_cancelled() {
            return TransportResult {
                outcome: TransportOutcome::Failed,
                transmission: Transmission::Unsent,
            };
        }
        self.trace.push((destination, body.to_vec()));
        if self.cancel_after_send {
            cancellation.cancel();
        }
        TransportResult {
            outcome: if self.redirect {
                TransportOutcome::Redirect(SECOND.endpoint.into())
            } else {
                TransportOutcome::Response("mock response")
            },
            transmission: Transmission::Sent,
        }
    }
}

fn policy(local_only: bool) -> Policy {
    Policy::new(
        CapabilityRegistry::new([GENERATE]),
        DataClasses::default(),
        [FIRST, SECOND],
        local_only,
    )
}

fn request(session: &Session, destination: Destination) -> Outbound {
    let body = Classified::derived(
        b"synthetic memory and prompt".to_vec(),
        [
            &Labels::from_source(USER_INPUT),
            &Labels::from_source(MEMORY),
        ],
        SYSTEM_TEXT,
    );
    Outbound::new(
        session,
        GENERATE,
        "answer",
        Locality::ThirdParty,
        destination,
        body,
        Retention::None,
    )
}

fn grants(policy: &Policy, request: &Outbound, until: Instant) -> (GrantId, GrantId) {
    (
        policy
            .grant(Permission::Process, request.operation(), until)
            .unwrap(),
        policy
            .grant(Permission::Share, request.operation(), until)
            .unwrap(),
    )
}

fn future() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn denied(
    boundary: &mut NetworkBoundary<Mock>,
    request: &Outbound,
    process: GrantId,
    share: GrantId,
    expected: Denial,
) {
    let count = boundary.transport.trace.len();
    let (outcome, record) = boundary.send(request, process, share);
    assert!(matches!(outcome, OutboundOutcome::Denied(reason) if reason == expected));
    assert_eq!(record.transmission, Transmission::Unsent);
    assert!(record.authorization.is_empty());
    assert_eq!(boundary.transport.trace.len(), count);
}

#[test]
fn denies_unknown_ids_endpoint_changes_and_unverifiable_adapters() {
    let session = Session::new(Cancellation::default());
    let mut boundary = NetworkBoundary::new(policy(false), Mock::default());
    let payload = || Classified::from_source(vec![1], USER_INPUT);
    let make = |capability, body, target| {
        Outbound::new(
            &session,
            capability,
            "answer",
            Locality::ThirdParty,
            target,
            body,
            Retention::None,
        )
    };
    let unknown_capability = make(CapabilityId::new("core.generate/2"), payload(), FIRST);
    denied(
        &mut boundary,
        &unknown_capability,
        0,
        0,
        Denial::UnknownCapability,
    );
    let unknown_class = DataClass {
        id: "core.memory/2",
        sensitivity: Sensitivity::Personal,
    };
    let unknown_data = make(
        GENERATE,
        Classified::from_source(vec![1], unknown_class),
        FIRST,
    );
    denied(&mut boundary, &unknown_data, 0, 0, Denial::UnknownDataClass);
    let unknown_destination = make(
        GENERATE,
        payload(),
        Destination {
            id: "unknown",
            ..FIRST
        },
    );
    denied(
        &mut boundary,
        &unknown_destination,
        0,
        0,
        Denial::UnknownDestination,
    );
    let changed_endpoint = make(
        GENERATE,
        payload(),
        Destination {
            endpoint: SECOND.endpoint,
            ..FIRST
        },
    );
    denied(
        &mut boundary,
        &changed_endpoint,
        0,
        0,
        Denial::UnknownDestination,
    );
    let ungoverned = Operation::new(
        &session,
        GENERATE,
        "answer",
        Labels::from_source(USER_INPUT),
        Locality::OnDevice,
        Connectivity::Unverifiable,
        None,
        Retention::None,
    );
    assert_eq!(
        boundary
            .authority()
            .grant(Permission::Process, &ungoverned, future()),
        Err(Denial::UngovernedProvider)
    );
}

#[test]
fn sharing_is_one_operation_and_receipts_match_recorded_bytes() {
    let session = Session::new(Cancellation::default());
    let mut boundary = NetworkBoundary::new(policy(false), Mock::default());
    let outbound = request(&session, FIRST);
    let (process, share) = grants(boundary.authority(), &outbound, future());
    let replacement = request(&session, FIRST);
    denied(&mut boundary, &replacement, process, share, Denial::Scope);
    let changed_purpose = Outbound::new(
        &session,
        GENERATE,
        "different-purpose",
        Locality::ThirdParty,
        FIRST,
        Classified::from_source(vec![1], USER_INPUT),
        Retention::None,
    );
    denied(
        &mut boundary,
        &changed_purpose,
        process,
        share,
        Denial::Scope,
    );
    let (outcome, receipt) = boundary.send(&outbound, process, share);
    assert!(matches!(
        outcome,
        OutboundOutcome::Transport(TransportOutcome::Response("mock response"))
    ));
    assert_eq!(receipt.authorization, vec![process, share]);
    assert_eq!(receipt.transmission, Transmission::Sent);
    assert_eq!(receipt.destination, boundary.transport.trace[0].0.id);
    assert_eq!(receipt.labels, *outbound.operation().labels());
    for class in [MEMORY, USER_INPUT, SYSTEM_TEXT] {
        assert!(receipt.labels.contains(class));
    }
    assert_eq!(
        boundary.transport.trace[0].1,
        b"synthetic memory and prompt"
    );
    denied(&mut boundary, &outbound, process, share, Denial::Scope);
    let secret = Outbound::new(
        &session,
        GENERATE,
        "answer",
        Locality::ThirdParty,
        FIRST,
        Classified::from_source(vec![1], SECRET).map(USER_INPUT, |bytes| bytes),
        Retention::None,
    );
    denied(&mut boundary, &secret, process, share, Denial::Scope);
}

#[test]
fn requires_current_process_and_share_grants_at_transmission() {
    let session = Session::new(Cancellation::default());
    let mut boundary = NetworkBoundary::new(policy(false), Mock::default());
    let outbound = request(&session, FIRST);
    denied(&mut boundary, &outbound, 0, 0, Denial::MissingGrant);
    let (process, share) = grants(boundary.authority(), &outbound, future());
    denied(&mut boundary, &outbound, process, 0, Denial::MissingGrant);
    denied(&mut boundary, &outbound, share, process, Denial::Scope);
    let expires = Instant::now() + Duration::from_millis(80);
    let expiring = boundary
        .authority()
        .grant(Permission::Share, outbound.operation(), expires)
        .unwrap();
    std::thread::sleep(expires.saturating_duration_since(Instant::now()));
    denied(&mut boundary, &outbound, process, expiring, Denial::Expired);
    assert!(boundary.authority().revoke(share).unwrap());
    denied(&mut boundary, &outbound, process, share, Denial::Revoked);
    let (process, share) = grants(boundary.authority(), &outbound, future());
    session.close();
    denied(&mut boundary, &outbound, process, share, Denial::Cancelled);
}

#[test]
fn observation_processing_retention_and_sharing_are_independent() {
    let authority = policy(false);
    let session = Session::new(Cancellation::default());
    let operation = Operation::local(
        &session,
        GENERATE,
        "capture",
        Labels::from_source(MEMORY),
        Retention::Session,
    );
    let observe = authority
        .grant(Permission::Observe, &operation, future())
        .unwrap();
    assert_eq!(
        authority.authorize(Permission::Observe, &operation, observe),
        Ok(())
    );
    for permission in [Permission::Process, Permission::Retain, Permission::Share] {
        assert_eq!(
            authority.authorize(permission, &operation, observe),
            Err(Denial::Scope)
        );
        assert_eq!(
            authority.authorize_local(permission, &operation),
            Err(Denial::MissingGrant)
        );
    }
    let retention = authority
        .grant(Permission::Retain, &operation, future())
        .unwrap();
    assert_eq!(
        authority.authorize(Permission::Retain, &operation, retention),
        Ok(())
    );
    let separate_session = Session::new(Cancellation::default());
    let other = Operation::local(
        &separate_session,
        GENERATE,
        "capture",
        Labels::from_source(MEMORY),
        Retention::Session,
    );
    assert_eq!(
        authority.authorize(Permission::Retain, &other, retention),
        Err(Denial::Scope)
    );
}

#[test]
fn local_only_and_local_selection_never_fall_back_to_remote() {
    let session = Session::new(Cancellation::default());
    let mut boundary = NetworkBoundary::new(policy(true), Mock::default());
    let remote = request(&session, FIRST);
    denied(&mut boundary, &remote, 0, 0, Denial::Locality);
    let local_only_request = Outbound::new(
        &session,
        GENERATE,
        "answer",
        Locality::OnDevice,
        FIRST,
        Classified::from_source(vec![1], USER_INPUT),
        Retention::None,
    );
    let mut remote_enabled = NetworkBoundary::new(policy(false), Mock::default());
    denied(
        &mut remote_enabled,
        &local_only_request,
        0,
        0,
        Denial::Locality,
    );
}

#[test]
fn redirects_need_fresh_consent_and_cancellation_discards_a_sent_response() {
    let session = Session::new(Cancellation::default());
    let mut boundary = NetworkBoundary::new(
        policy(false),
        Mock {
            redirect: true,
            ..Mock::default()
        },
    );
    let outbound = request(&session, FIRST);
    let (process, share) = grants(boundary.authority(), &outbound, future());
    let (outcome, receipt) = boundary.send(&outbound, process, share);
    assert!(
        matches!(outcome, OutboundOutcome::Transport(TransportOutcome::Redirect(url)) if url == SECOND.endpoint)
    );
    assert_eq!(receipt.transmission, Transmission::Sent);
    assert_eq!(boundary.transport.trace.len(), 1);
    let redirected = request(&session, SECOND);
    denied(&mut boundary, &redirected, process, share, Denial::Scope);
    boundary.transport.redirect = false;
    boundary.transport.cancel_after_send = true;
    let (process, share) = grants(boundary.authority(), &redirected, future());
    let (outcome, receipt) = boundary.send(&redirected, process, share);
    assert!(matches!(
        outcome,
        OutboundOutcome::Denied(Denial::Cancelled)
    ));
    assert_eq!(receipt.transmission, Transmission::Sent);
    assert_eq!(receipt.destination, boundary.transport.trace[1].0.id);
    assert_eq!(receipt.authorization, vec![process, share]);
}

#[test]
fn registering_a_data_class_does_not_grant_existing_workflow_access() {
    let extension = DataClass {
        id: "extension.sensor-result/1",
        sensitivity: Sensitivity::RawSensor,
    };
    let policy = Policy::new(
        CapabilityRegistry::new([GENERATE]),
        DataClasses::new([extension]),
        [],
        true,
    );
    let session = Session::new(Cancellation::default());
    let operation = Operation::local(
        &session,
        GENERATE,
        "chat",
        Labels::from_source(extension),
        Retention::SavedLocally,
    );
    assert_eq!(
        policy.authorize_local(Permission::Process, &operation),
        Err(Denial::MissingGrant)
    );
    let explicit = policy
        .grant(Permission::Process, &operation, future())
        .unwrap();
    assert_eq!(
        policy.authorize(Permission::Process, &operation, explicit),
        Ok(())
    );
}
