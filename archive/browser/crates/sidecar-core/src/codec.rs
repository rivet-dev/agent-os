//! Host-free wire codec for ACP requests/responses/events/callbacks.
//!
//! Identical (de)serialization to the native sidecar's private codec, lifted here
//! so both backends share it. BARE over the `agentos-protocol` generated types.

use agentos_protocol::generated::v1::{AcpCallback, AcpEvent, AcpRequest, AcpResponse};

use crate::AcpCoreError;

pub fn decode_request(payload: &[u8]) -> Result<AcpRequest, AcpCoreError> {
    serde_bare::from_slice(payload)
        .map_err(|error| AcpCoreError::InvalidState(format!("invalid ACP request: {error}")))
}

pub fn encode_response(response: &AcpResponse) -> Result<Vec<u8>, AcpCoreError> {
    serde_bare::to_vec(response)
        .map_err(|error| AcpCoreError::InvalidState(format!("invalid ACP response: {error}")))
}

pub fn encode_event(event: &AcpEvent) -> Result<Vec<u8>, AcpCoreError> {
    serde_bare::to_vec(event)
        .map_err(|error| AcpCoreError::InvalidState(format!("invalid ACP event: {error}")))
}

pub fn encode_callback(callback: &AcpCallback) -> Result<Vec<u8>, AcpCoreError> {
    serde_bare::to_vec(callback)
        .map_err(|error| AcpCoreError::InvalidState(format!("invalid ACP callback: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_protocol::generated::v1::{AcpCloseSessionRequest, AcpSessionClosedResponse};

    #[test]
    fn request_round_trips_through_bare() {
        let request = AcpRequest::AcpCloseSessionRequest(AcpCloseSessionRequest {
            session_id: "sess-1".into(),
        });
        let bytes = serde_bare::to_vec(&request).expect("encode");
        let decoded = decode_request(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn response_round_trips_through_bare() {
        let response = AcpResponse::AcpSessionClosedResponse(AcpSessionClosedResponse {
            session_id: "sess-1".into(),
        });
        let bytes = encode_response(&response).expect("encode");
        let decoded: AcpResponse = serde_bare::from_slice(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }
}
