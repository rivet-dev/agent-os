//! Bounded transport configuration for the sidecar protocol.
//!
//! These limits belong to the process transport, not to the Tokio work
//! driver. Keeping them beside the wire protocol lets alternative drivers use
//! the same framing contract without depending on `agentos-driver-tokio`.

pub const DEFAULT_MAX_INGRESS_FRAMES: usize = 128;
pub const DEFAULT_MAX_INGRESS_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_CONTROL_FRAMES: usize = 1_024;
pub const DEFAULT_MAX_CONTROL_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_EGRESS_FRAMES: usize = 4_096;
pub const DEFAULT_MAX_EGRESS_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_PENDING_RESPONSES: usize = 10_000;
pub const DEFAULT_MAX_PENDING_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_PROCESS_EVENTS: usize = 10_000;
pub const DEFAULT_MAX_OUTBOUND_REQUESTS: usize = 10_000;
pub const DEFAULT_MAX_COMPLETED_RESPONSES: usize = 10_000;

/// Process-owned bounds for the multiplexed sidecar transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidecarProtocolConfig {
    pub max_ingress_frames: usize,
    pub max_ingress_bytes: usize,
    pub max_control_frames: usize,
    pub max_control_bytes: usize,
    pub max_egress_frames: usize,
    pub max_egress_bytes: usize,
    pub max_pending_responses: usize,
    pub max_pending_response_bytes: usize,
    pub max_process_events: usize,
    pub max_outbound_requests: usize,
    pub max_completed_responses: usize,
}

impl Default for SidecarProtocolConfig {
    fn default() -> Self {
        Self {
            max_ingress_frames: DEFAULT_MAX_INGRESS_FRAMES,
            max_ingress_bytes: DEFAULT_MAX_INGRESS_BYTES,
            max_control_frames: DEFAULT_MAX_CONTROL_FRAMES,
            max_control_bytes: DEFAULT_MAX_CONTROL_BYTES,
            max_egress_frames: DEFAULT_MAX_EGRESS_FRAMES,
            max_egress_bytes: DEFAULT_MAX_EGRESS_BYTES,
            max_pending_responses: DEFAULT_MAX_PENDING_RESPONSES,
            max_pending_response_bytes: DEFAULT_MAX_PENDING_RESPONSE_BYTES,
            max_process_events: DEFAULT_MAX_PROCESS_EVENTS,
            max_outbound_requests: DEFAULT_MAX_OUTBOUND_REQUESTS,
            max_completed_responses: DEFAULT_MAX_COMPLETED_RESPONSES,
        }
    }
}

impl SidecarProtocolConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (path, value) in [
            ("runtime.protocol.maxIngressFrames", self.max_ingress_frames),
            ("runtime.protocol.maxIngressBytes", self.max_ingress_bytes),
            ("runtime.protocol.maxControlFrames", self.max_control_frames),
            ("runtime.protocol.maxControlBytes", self.max_control_bytes),
            ("runtime.protocol.maxEgressFrames", self.max_egress_frames),
            ("runtime.protocol.maxEgressBytes", self.max_egress_bytes),
            (
                "runtime.protocol.maxPendingResponses",
                self.max_pending_responses,
            ),
            (
                "runtime.protocol.maxPendingResponseBytes",
                self.max_pending_response_bytes,
            ),
            ("runtime.protocol.maxProcessEvents", self.max_process_events),
            (
                "runtime.protocol.maxOutboundRequests",
                self.max_outbound_requests,
            ),
            (
                "runtime.protocol.maxCompletedResponses",
                self.max_completed_responses,
            ),
        ] {
            if value == 0 {
                return Err(format!("{path} must be greater than zero"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_capacity_with_the_public_config_path() {
        let config = SidecarProtocolConfig {
            max_ingress_bytes: 0,
            ..SidecarProtocolConfig::default()
        };
        let error = config
            .validate()
            .expect_err("zero protocol byte capacity must be rejected");
        assert!(error.contains("runtime.protocol.maxIngressBytes"));
    }
}
