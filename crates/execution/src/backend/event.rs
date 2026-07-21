use super::{DirectHostReplyHandle, HostServiceError, PayloadLimit};
use crate::host::{BoundedBytes, HostOperation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedHostServiceError {
    error: HostServiceError,
    encoded_bytes: usize,
}

impl BoundedHostServiceError {
    pub fn try_new(
        error: HostServiceError,
        limit: &PayloadLimit,
    ) -> Result<Self, HostServiceError> {
        let encoded_bytes = limit.admit_json(&error)?;
        Ok(Self {
            error,
            encoded_bytes,
        })
    }

    pub fn error(&self) -> &HostServiceError {
        &self.error
    }

    pub fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    pub fn into_error(self) -> HostServiceError {
        self.error
    }
}

impl std::fmt::Display for BoundedHostServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionExit {
    Exited(i32),
    Signaled { signal: i32, core_dumped: bool },
}

/// Common events emitted by all production execution backends.
///
/// Adapter-specific Node stream/callback events remain behind an adapter
/// extension and are never consumed by shared host-service implementations.
#[derive(Debug, Clone)]
#[non_exhaustive]
#[allow(clippy::large_enum_variant)]
pub enum ExecutionEvent {
    Output {
        stream: OutputStream,
        bytes: BoundedBytes,
    },
    HostCall {
        operation: HostOperation,
        reply: DirectHostReplyHandle,
    },
    Warning(BoundedHostServiceError),
    RuntimeFault(BoundedHostServiceError),
    Exited(ExecutionExit),
}

impl ExecutionEvent {
    pub fn output(
        stream: OutputStream,
        bytes: Vec<u8>,
        limit: &PayloadLimit,
    ) -> Result<Self, HostServiceError> {
        Ok(Self::Output {
            stream,
            bytes: BoundedBytes::try_new(bytes, limit)?,
        })
    }

    pub fn warning(
        warning: HostServiceError,
        limit: &PayloadLimit,
    ) -> Result<Self, HostServiceError> {
        Ok(Self::Warning(BoundedHostServiceError::try_new(
            warning, limit,
        )?))
    }

    pub fn runtime_fault(
        fault: HostServiceError,
        limit: &PayloadLimit,
    ) -> Result<Self, HostServiceError> {
        Ok(Self::RuntimeFault(BoundedHostServiceError::try_new(
            fault, limit,
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_and_warning_events_require_named_admission_limits() {
        let output_limit = PayloadLimit::new("maxOutputEventBytes", 4).expect("output limit");
        assert_eq!(
            ExecutionEvent::output(OutputStream::Stdout, vec![0; 5], &output_limit)
                .expect_err("oversized output")
                .details
                .expect("details")["limitName"],
            "maxOutputEventBytes"
        );

        let warning_limit = PayloadLimit::new("maxWarningEventBytes", 64).expect("warning limit");
        let warning = HostServiceError::new("EIO", "x".repeat(128))
            .with_details(serde_json::json!({ "path": "/retained/details" }));
        let error = ExecutionEvent::warning(warning, &warning_limit)
            .expect_err("oversized warning must be rejected before event construction");
        assert_eq!(error.code, "E2BIG");
        assert_eq!(
            error.details.expect("limit details")["limitName"],
            "maxWarningEventBytes"
        );

        let fault_limit = PayloadLimit::new("maxRuntimeFaultBytes", 64).expect("fault limit");
        let fault = HostServiceError::new("ERR_AGENTOS_WASM_TRAP", "x".repeat(128));
        assert_eq!(
            ExecutionEvent::runtime_fault(fault, &fault_limit)
                .expect_err("oversized runtime fault must fail admission")
                .details
                .expect("details")["limitName"],
            "maxRuntimeFaultBytes"
        );
    }
}
