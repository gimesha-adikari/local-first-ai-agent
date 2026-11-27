//! Visual input workflows shared by screen regions, camera snapshots and
//! attached images (Stage 04-E): the shared settings and the deterministic
//! authorization of one camera snapshot. Platform capture stays in the
//! desktop host; the engine never touches a device.

use std::time::{Duration, Instant};

use agent_core::{
    capability::{CAMERA_SNAPSHOT, Retention},
    classification::{CAMERA_IMAGE, Labels},
    inference::{Cancellation, ErrorCode, InferenceError, Result},
    visual::VisualSettings,
};

use crate::{
    Engine, database,
    policy::{GrantId, Operation, Permission, Policy, Session},
};

/// The longest one snapshot authorization lasts: the desktop's consent
/// dialog, camera start-up and a frame, with margin. It ends sooner when
/// the host releases the camera.
pub const CAMERA_AUTHORIZATION: Duration = Duration::from_secs(240);

/// An observation grant for exactly one camera snapshot. Dropping it
/// revokes the grant, so it cannot outlive the capture that holds it.
pub struct CameraObservation {
    policy: std::sync::Arc<Policy>,
    // Kept alive: the grant is bound to this session and operation.
    _session: Session,
    _operation: Operation,
    grant: GrantId,
}

impl CameraObservation {
    /// The policy's identifier of the grant, for the receipt.
    pub fn grant(&self) -> GrantId {
        self.grant
    }
}

impl Drop for CameraObservation {
    fn drop(&mut self) {
        let _ = self.policy.revoke(self.grant);
    }
}

impl Engine {
    pub fn visual_settings(&self) -> Result<VisualSettings> {
        database(&self.database)?.visual_settings()
    }

    pub fn save_visual_settings(&self, settings: &VisualSettings) -> Result<()> {
        database(&self.database)?.save_visual_settings(settings)
    }

    /// Authorize observing one camera snapshot after an explicit gesture.
    /// This covers only taking the frame on this device: it grants no
    /// processing by a provider, no retention and no sharing.
    pub fn authorize_camera_snapshot(
        &self,
        cancellation: Cancellation,
    ) -> Result<CameraObservation> {
        let session = Session::new(cancellation);
        let operation = Operation::local(
            &session,
            CAMERA_SNAPSHOT,
            "camera-snapshot",
            Labels::from_source(CAMERA_IMAGE),
            Retention::Session,
        );
        let denied = || {
            InferenceError::new(
                ErrorCode::PolicyDenied,
                "The intelligence policy denied camera access. Nothing was captured.",
            )
        };
        let grant = self
            .policy
            .grant(
                Permission::Observe,
                &operation,
                Instant::now() + CAMERA_AUTHORIZATION,
            )
            .map_err(|_| denied())?;
        if self
            .policy
            .authorize(Permission::Observe, &operation, grant)
            .is_err()
        {
            let _ = self.policy.revoke(grant);
            return Err(denied());
        }
        Ok(CameraObservation {
            policy: std::sync::Arc::clone(&self.policy),
            _session: session,
            _operation: operation,
            grant,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_camera_grant_observes_only_and_is_revoked_when_released() {
        let engine = crate::tests::harness().engine;
        let observation = engine
            .authorize_camera_snapshot(Cancellation::default())
            .unwrap();
        let grant = observation.grant();
        drop(observation);
        // Already revoked: revoking again changes nothing.
        assert!(!engine.intelligence_policy().revoke(grant).unwrap());

        // Observation never stands in for processing, retention or sharing.
        let session = Session::new(Cancellation::default());
        let operation = Operation::local(
            &session,
            CAMERA_SNAPSHOT,
            "camera-snapshot",
            Labels::from_source(CAMERA_IMAGE),
            Retention::Session,
        );
        let policy = engine.intelligence_policy();
        let observe = policy
            .grant(
                Permission::Observe,
                &operation,
                Instant::now() + CAMERA_AUTHORIZATION,
            )
            .unwrap();
        for other in [Permission::Process, Permission::Retain] {
            assert!(policy.authorize(other, &operation, observe).is_err());
        }
        assert!(
            policy
                .authorize_local(Permission::Process, &operation)
                .is_err()
        );

        // A cancelled gesture cannot be authorized.
        let cancelled = Cancellation::default();
        cancelled.cancel();
        assert!(engine.authorize_camera_snapshot(cancelled).is_err());
    }
}
