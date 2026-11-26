//! Application-controlled egress. Adapters receive no authorization token,
//! transport handle or connection. The host owns this boundary.

use agent_core::{
    capability::{CapabilityId, Locality, Retention},
    classification::Classified,
    inference::Cancellation,
    intelligence::{EgressRecord, Transmission},
};

use crate::policy::{Connectivity, Denial, Destination, GrantId, Operation, Policy, Session};

pub struct Outbound {
    operation: Operation,
    destination: Destination,
    body: Vec<u8>,
    provider_retention: Retention,
}

impl Outbound {
    pub fn new(
        session: &Session,
        capability: CapabilityId,
        purpose: &'static str,
        locality: Locality,
        destination: Destination,
        body: Classified<Vec<u8>>,
        provider_retention: Retention,
    ) -> Self {
        let (body, labels) = body.into_parts();
        Self {
            operation: Operation::new(
                session,
                capability,
                purpose,
                labels,
                locality,
                Connectivity::ThroughBoundary,
                Some(destination),
                provider_retention,
            ),
            destination,
            body,
            provider_retention,
        }
    }

    pub fn operation(&self) -> &Operation {
        &self.operation
    }
}

/// Trusted transport outcome. A redirect is returned as data, never followed.
pub enum TransportOutcome<T> {
    Response(T),
    Redirect(String),
    Failed,
}

pub struct TransportResult<T> {
    pub outcome: TransportOutcome<T>,
    pub transmission: Transmission,
}

/// Host-owned, single-hop transport. 03-B ships no implementation outside
/// tests. This call is only the bounded transmission commitment. See the
/// 03-B design record for the requirements on a real transport.
pub trait Transport {
    type Response;

    fn send_once(
        &mut self,
        destination: Destination,
        body: &[u8],
        cancellation: &Cancellation,
    ) -> TransportResult<Self::Response>;
}

pub enum OutboundOutcome<T> {
    Denied(Denial),
    Transport(TransportOutcome<T>),
}

pub struct NetworkBoundary<T> {
    policy: Policy,
    transport: T,
}

impl<T: Transport> NetworkBoundary<T> {
    pub fn new(policy: Policy, transport: T) -> Self {
        Self { policy, transport }
    }

    /// Kept with trusted consent handling. Provider adapters do not receive it.
    pub fn authority(&self) -> &Policy {
        &self.policy
    }

    pub fn send(
        &mut self,
        request: &Outbound,
        process: GrantId,
        share: GrantId,
    ) -> (OutboundOutcome<T::Response>, EgressRecord) {
        let mut receipt = EgressRecord {
            destination: request.destination.id,
            locality: request.destination.locality,
            provider_retention: request.provider_retention,
            transmission: Transmission::Unsent,
            labels: request.operation.labels().clone(),
            authorization: Vec::new(),
        };
        let result = self
            .policy
            .transmit(&request.operation, process, share, || {
                self.transport.send_once(
                    request.destination,
                    &request.body,
                    request.operation.cancellation(),
                )
            });
        let outcome = match result {
            Ok(result) => {
                receipt.transmission = result.transmission;
                receipt.authorization = vec![process, share];
                match self
                    .policy
                    .accept_response(&request.operation, process, share)
                {
                    Err(denial) => OutboundOutcome::Denied(denial),
                    Ok(()) => OutboundOutcome::Transport(result.outcome),
                }
            }
            Err(denial) => OutboundOutcome::Denied(denial),
        };
        (outcome, receipt)
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
