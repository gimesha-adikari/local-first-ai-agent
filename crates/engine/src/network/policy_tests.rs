//! Policy-level egress rules. Socket behavior is covered by the transport
//! tests against real loopback and TLS fixtures.

use std::time::{Duration, Instant};

use agent_core::{
    capability::{CapabilityId, CapabilityRegistry, GENERATE, Locality, Retention},
    classification::{
        Classified, DataClass, DataClasses, Labels, MEMORY, SECRET, SYSTEM_TEXT, Sensitivity,
        USER_INPUT,
    },
    inference::Cancellation,
};

use super::*;
use crate::policy::{Connectivity, Permission};

fn destination(id: &str, url: &str) -> Destination {
    Destination::new(id, Endpoint::parse(url).unwrap(), Locality::ThirdParty).unwrap()
}

fn first() -> Destination {
    destination("fixture.first/1", "https://first.example/v1")
}

fn second() -> Destination {
    destination("fixture.second/1", "https://second.example/v1")
}

fn policy() -> Policy {
    Policy::new(
        CapabilityRegistry::new([GENERATE]),
        DataClasses::default(),
        [first(), second()],
    )
}

const ROUTE: Route = Route::new("/chat/completions");

fn outbound(session: &Session, target: Destination, body: Classified<Vec<u8>>) -> Outbound {
    Outbound::new(
        session,
        GENERATE,
        "answer",
        Locality::ThirdParty,
        target,
        Method::Post,
        ROUTE,
        body,
        Retention::None,
    )
}

fn request(session: &Session, target: Destination) -> Outbound {
    let body = Classified::derived(
        b"synthetic memory and prompt".to_vec(),
        [
            &Labels::from_source(USER_INPUT),
            &Labels::from_source(MEMORY),
        ],
        SYSTEM_TEXT,
    );
    outbound(session, target, body)
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

#[test]
fn denies_unknown_ids_endpoint_changes_routes_and_unverifiable_adapters() {
    let session = Session::new(Cancellation::default());
    let authority = policy();
    let payload = || Classified::from_source(vec![1], USER_INPUT);
    let admit = |request: &Outbound| authority.admit(request.operation(), 0, 0);
    let unknown_capability = Outbound::new(
        &session,
        CapabilityId::new("core.generate/2"),
        "answer",
        Locality::ThirdParty,
        first(),
        Method::Post,
        ROUTE,
        payload(),
        Retention::None,
    );
    assert_eq!(admit(&unknown_capability), Err(Denial::UnknownCapability));
    let unknown_class = DataClass {
        id: "core.memory/2",
        sensitivity: Sensitivity::Personal,
    };
    let unknown_data = outbound(
        &session,
        first(),
        Classified::from_source(vec![1], unknown_class),
    );
    assert_eq!(admit(&unknown_data), Err(Denial::UnknownDataClass));
    let unknown_destination = outbound(
        &session,
        destination("unknown", "https://first.example/v1"),
        payload(),
    );
    assert_eq!(admit(&unknown_destination), Err(Denial::UnknownDestination));
    // Same identifier, different canonical endpoint or credential binding.
    let changed_endpoint = outbound(
        &session,
        destination("fixture.first/1", "https://second.example/v1"),
        payload(),
    );
    assert_eq!(admit(&changed_endpoint), Err(Denial::UnknownDestination));
    let changed_port = outbound(
        &session,
        destination("fixture.first/1", "https://first.example:8443/v1"),
        payload(),
    );
    assert_eq!(admit(&changed_port), Err(Denial::UnknownDestination));
    let added_credential = outbound(&session, first().with_credential("fixture"), payload());
    assert_eq!(admit(&added_credential), Err(Denial::UnknownDestination));
    let bad_route = Outbound::new(
        &session,
        GENERATE,
        "answer",
        Locality::ThirdParty,
        first(),
        Method::Post,
        Route::new("/../admin"),
        payload(),
        Retention::None,
    );
    assert_eq!(admit(&bad_route), Err(Denial::UnknownDestination));
    let ungoverned = Operation::new(
        &session,
        GENERATE,
        "answer",
        Labels::from_source(USER_INPUT),
        Locality::OnDevice,
        Connectivity::Unverifiable,
        None,
        None,
        Retention::None,
    );
    assert_eq!(
        authority.grant(Permission::Process, &ungoverned, future()),
        Err(Denial::UngovernedProvider)
    );
}

#[test]
fn sharing_is_one_operation_bound_to_its_payload() {
    let session = Session::new(Cancellation::default());
    let authority = policy();
    let request = request(&session, first());
    let (process, share) = grants(&authority, &request, future());
    // An identical body in a new operation needs its own grants.
    let replacement = self::request(&session, first());
    assert_eq!(
        authority.admit(replacement.operation(), process, share),
        Err(Denial::Scope)
    );
    let changed_body = outbound(
        &session,
        first(),
        Classified::from_source(vec![2], USER_INPUT),
    );
    assert_eq!(
        authority.admit(changed_body.operation(), process, share),
        Err(Denial::Scope)
    );
    assert_eq!(authority.admit(request.operation(), process, share), Ok(()));
    assert_eq!(
        authority.commit(request.operation(), process, share),
        Ok(())
    );
    // The handoff consumed the sharing grant; the response stays acceptable.
    assert_eq!(
        authority.commit(request.operation(), process, share),
        Err(Denial::Scope)
    );
    assert_eq!(
        authority.accept_response(request.operation(), process, share),
        Ok(())
    );
    for class in [MEMORY, USER_INPUT, SYSTEM_TEXT] {
        assert!(request.labels().contains(class));
    }
    let secret = outbound(
        &session,
        first(),
        Classified::from_source(vec![1], SECRET).map(USER_INPUT, |bytes| bytes),
    );
    assert_eq!(
        authority.grant(Permission::Share, secret.operation(), future()),
        Err(Denial::Scope)
    );
    let unknown_retention = Outbound::new(
        &session,
        GENERATE,
        "answer",
        Locality::ThirdParty,
        first(),
        Method::Post,
        ROUTE,
        Classified::from_source(vec![1], USER_INPUT),
        Retention::Unknown,
    );
    assert_eq!(
        authority.grant(Permission::Share, unknown_retention.operation(), future()),
        Err(Denial::Scope)
    );
}

#[test]
fn requires_current_process_and_share_grants() {
    let session = Session::new(Cancellation::default());
    let authority = policy();
    let request = request(&session, first());
    assert_eq!(
        authority.admit(request.operation(), 0, 0),
        Err(Denial::MissingGrant)
    );
    let (process, share) = grants(&authority, &request, future());
    assert_eq!(
        authority.admit(request.operation(), process, 0),
        Err(Denial::MissingGrant)
    );
    assert_eq!(
        authority.admit(request.operation(), share, process),
        Err(Denial::Scope)
    );
    let expires = Instant::now() + Duration::from_millis(60);
    let expiring = authority
        .grant(Permission::Share, request.operation(), expires)
        .unwrap();
    std::thread::sleep(expires.saturating_duration_since(Instant::now()));
    assert_eq!(
        authority.admit(request.operation(), process, expiring),
        Err(Denial::Expired)
    );
    assert!(authority.revoke(share).unwrap());
    assert_eq!(
        authority.admit(request.operation(), process, share),
        Err(Denial::Revoked)
    );
    let (process, share) = grants(&authority, &request, future());
    authority.unregister_destination("fixture.first/1").unwrap();
    assert_eq!(
        authority.admit(request.operation(), process, share),
        Err(Denial::UnknownDestination)
    );
    authority.register_destination(first()).unwrap();
    assert_eq!(authority.admit(request.operation(), process, share), Ok(()));
    session.close();
    assert_eq!(
        authority.admit(request.operation(), process, share),
        Err(Denial::Cancelled)
    );
}

#[test]
fn observation_processing_retention_and_sharing_are_independent() {
    let authority = policy();
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
fn a_local_intention_cannot_be_routed_to_a_remote_destination() {
    let session = Session::new(Cancellation::default());
    let intended_local = Outbound::new(
        &session,
        GENERATE,
        "answer",
        Locality::OnDevice,
        first(),
        Method::Post,
        ROUTE,
        Classified::from_source(vec![1], USER_INPUT),
        Retention::None,
    );
    let authority = policy();
    assert_eq!(
        authority.admit(intended_local.operation(), 0, 0),
        Err(Denial::Locality)
    );
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

#[test]
fn search_requests_admit_visual_query_classes_but_never_history_memory_or_documents() {
    use crate::policy::VISUAL_SEARCH_PURPOSE;
    use agent_core::{
        capability::WEB_SEARCH,
        classification::{
            ATTACHMENT, ATTACHMENT_IMAGE, ATTACHMENT_TEXT, CAMERA_IMAGE, CAMERA_TEXT, HISTORY,
            LIBRARY, MODEL_OUTPUT, PDF_EXCERPT, PUBLIC_WEB, QUERY, SCREEN_TEXT, SYSTEM_TEXT,
            TEXT_EXCERPT,
        },
    };
    let search = destination("search/fixture", "https://search.example/res");
    let authority = Policy::new(
        CapabilityRegistry::new([WEB_SEARCH]),
        DataClasses::default(),
        [search.clone()],
    );
    let session = Session::new(Cancellation::default());
    let query = |purpose, sources: &[DataClass]| {
        let mut labels = Labels::from_source(USER_INPUT);
        for class in sources {
            labels.insert(*class);
        }
        Outbound::new(
            &session,
            WEB_SEARCH,
            purpose,
            Locality::ThirdParty,
            search.clone(),
            Method::Post,
            Route::new("/res"),
            Classified::derived(b"q=synthetic".to_vec(), [&labels], QUERY),
            Retention::ProviderDeclared,
        )
    };
    for visual in [
        &[SCREEN_TEXT][..],
        &[CAMERA_TEXT, CAMERA_IMAGE],
        &[ATTACHMENT, ATTACHMENT_IMAGE, ATTACHMENT_TEXT],
        &[CAMERA_TEXT, CAMERA_IMAGE, MODEL_OUTPUT, SYSTEM_TEXT],
    ] {
        let request = query(VISUAL_SEARCH_PURPOSE, visual);
        assert!(
            authority
                .grant(Permission::Share, request.operation(), future())
                .is_ok(),
            "{visual:?}"
        );
        let ordinary = query("web-search", visual);
        assert_eq!(
            authority.grant(Permission::Share, ordinary.operation(), future()),
            Err(Denial::Scope),
            "{visual:?}"
        );
    }
    for private in [HISTORY, MEMORY, LIBRARY, PDF_EXCERPT, TEXT_EXCERPT] {
        let request = query(
            VISUAL_SEARCH_PURPOSE,
            &[
                CAMERA_TEXT,
                CAMERA_IMAGE,
                MODEL_OUTPUT,
                SYSTEM_TEXT,
                private,
            ],
        );
        assert_eq!(
            authority.grant(Permission::Share, request.operation(), future()),
            Err(Denial::Scope),
            "{}",
            private.id
        );
    }
    let document = query(
        VISUAL_SEARCH_PURPOSE,
        &[CAMERA_TEXT, ATTACHMENT, ATTACHMENT_TEXT],
    );
    assert_eq!(
        authority.grant(Permission::Share, document.operation(), future()),
        Err(Denial::Scope)
    );
    for classes in [
        &[CAMERA_IMAGE, SYSTEM_TEXT][..],
        &[MODEL_OUTPUT, SYSTEM_TEXT],
    ] {
        let request = query(VISUAL_SEARCH_PURPOSE, classes);
        assert_eq!(
            authority.grant(Permission::Share, request.operation(), future()),
            Err(Denial::Scope)
        );
    }
    let purpose = crate::research::GENERATED_QUERY_PURPOSE;
    let generated = query(purpose, &[MODEL_OUTPUT, SYSTEM_TEXT, PUBLIC_WEB]);
    assert!(
        authority
            .grant(Permission::Share, generated.operation(), future())
            .is_ok()
    );
    for forbidden in [MEMORY, HISTORY, LIBRARY, ATTACHMENT_IMAGE] {
        let mixed = query(purpose, &[MODEL_OUTPUT, SYSTEM_TEXT, forbidden]);
        assert_eq!(
            authority.grant(Permission::Share, mixed.operation(), future()),
            Err(Denial::Scope)
        );
    }
    let ungenerated = query(purpose, &[SYSTEM_TEXT]);
    assert_eq!(
        authority.grant(Permission::Share, ungenerated.operation(), future()),
        Err(Denial::Scope)
    );
    // A search request must carry the query class.
    let bare = Outbound::new(
        &session,
        WEB_SEARCH,
        VISUAL_SEARCH_PURPOSE,
        Locality::ThirdParty,
        search.clone(),
        Method::Post,
        Route::new("/res"),
        Classified::from_source(b"q=synthetic".to_vec(), CAMERA_TEXT),
        Retention::ProviderDeclared,
    );
    assert_eq!(
        authority.grant(Permission::Share, bare.operation(), future()),
        Err(Denial::Scope)
    );
}
