use agentos_runtime::readiness::ReadyFlags;
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

/// Identifies the one execution generation a wake target may notify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionWakeIdentity {
    pub generation: u64,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionWakeError {
    code: &'static str,
    message: String,
    details: Option<Value>,
}

impl ExecutionWakeError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Option<Value>) -> Self {
        self.details = details;
        self
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }
}

impl fmt::Display for ExecutionWakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for ExecutionWakeError {}

/// Adapter-owned readiness sink. Implementations update durable per-capability
/// readiness before scheduling their one coalesced execution wake.
///
/// This trait deliberately exposes no engine session, isolate, Store, or guest
/// memory. Resource owners retain level state; consuming a wake never clears
/// readiness by itself.
pub trait ExecutionWakeTarget: Send + Sync {
    fn publish_readiness(
        &self,
        capability_id: u64,
        capability_generation: u64,
        flags: ReadyFlags,
    ) -> Result<(), ExecutionWakeError>;

    fn remove_readiness(
        &self,
        capability_id: u64,
        capability_generation: u64,
    ) -> Result<(), ExecutionWakeError>;

    fn set_application_read_interest(
        &self,
        capability_id: u64,
        capability_generation: u64,
        enabled: bool,
    ) -> Result<(), ExecutionWakeError>;

    fn publish_signal(&self, signal: i32, delivery_token: u64) -> Result<(), ExecutionWakeError>;

    /// Adapter extension for evented runtimes. Shared resource owners pass an
    /// engine-neutral value; the adapter owns its wire encoding and enforces
    /// the encoded-byte limit before queueing it.
    fn send_adapter_event(
        &self,
        event_type: &str,
        payload: &Value,
        encoded_limit_name: &'static str,
        max_encoded_bytes: usize,
    ) -> Result<(), ExecutionWakeError>;
}

#[derive(Clone)]
pub struct ExecutionWakeHandle {
    identity: ExecutionWakeIdentity,
    target: Arc<dyn ExecutionWakeTarget>,
}

impl fmt::Debug for ExecutionWakeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionWakeHandle")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl ExecutionWakeHandle {
    pub fn new(identity: ExecutionWakeIdentity, target: Arc<dyn ExecutionWakeTarget>) -> Self {
        Self { identity, target }
    }

    pub fn identity(&self) -> ExecutionWakeIdentity {
        self.identity
    }

    pub fn publish_readiness(
        &self,
        capability_id: u64,
        capability_generation: u64,
        flags: ReadyFlags,
    ) -> Result<(), ExecutionWakeError> {
        self.target
            .publish_readiness(capability_id, capability_generation, flags)
    }

    pub fn remove_readiness(
        &self,
        capability_id: u64,
        capability_generation: u64,
    ) -> Result<(), ExecutionWakeError> {
        self.target
            .remove_readiness(capability_id, capability_generation)
    }

    pub fn set_application_read_interest(
        &self,
        capability_id: u64,
        capability_generation: u64,
        enabled: bool,
    ) -> Result<(), ExecutionWakeError> {
        self.target
            .set_application_read_interest(capability_id, capability_generation, enabled)
    }

    pub fn publish_signal(
        &self,
        signal: i32,
        delivery_token: u64,
    ) -> Result<(), ExecutionWakeError> {
        self.target.publish_signal(signal, delivery_token)
    }

    pub fn send_adapter_event(
        &self,
        event_type: &str,
        payload: &Value,
        encoded_limit_name: &'static str,
        max_encoded_bytes: usize,
    ) -> Result<(), ExecutionWakeError> {
        self.target
            .send_adapter_event(event_type, payload, encoded_limit_name, max_encoded_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingTarget {
        readiness: Mutex<Vec<(u64, u64, ReadyFlags)>>,
        signals: Mutex<Vec<(i32, u64)>>,
    }

    impl ExecutionWakeTarget for RecordingTarget {
        fn publish_readiness(
            &self,
            capability_id: u64,
            capability_generation: u64,
            flags: ReadyFlags,
        ) -> Result<(), ExecutionWakeError> {
            self.readiness.lock().expect("readiness lock").push((
                capability_id,
                capability_generation,
                flags,
            ));
            Ok(())
        }

        fn remove_readiness(&self, _: u64, _: u64) -> Result<(), ExecutionWakeError> {
            Ok(())
        }

        fn set_application_read_interest(
            &self,
            _: u64,
            _: u64,
            _: bool,
        ) -> Result<(), ExecutionWakeError> {
            Ok(())
        }

        fn publish_signal(
            &self,
            signal: i32,
            delivery_token: u64,
        ) -> Result<(), ExecutionWakeError> {
            self.signals
                .lock()
                .expect("signals lock")
                .push((signal, delivery_token));
            Ok(())
        }

        fn send_adapter_event(
            &self,
            _: &str,
            _: &Value,
            _: &'static str,
            _: usize,
        ) -> Result<(), ExecutionWakeError> {
            Ok(())
        }
    }

    #[test]
    fn handle_keeps_generation_identity_and_forwards_level_state() {
        let target = Arc::new(RecordingTarget::default());
        let handle = ExecutionWakeHandle::new(
            ExecutionWakeIdentity {
                generation: 7,
                pid: 41,
            },
            target.clone(),
        );
        handle
            .publish_readiness(9, 3, ReadyFlags::READABLE)
            .expect("publish readiness");
        handle
            .publish_signal(15, 29)
            .expect("publish signal delivery");

        assert_eq!(handle.identity().generation, 7);
        assert_eq!(
            target.readiness.lock().expect("readiness lock").as_slice(),
            &[(9, 3, ReadyFlags::READABLE)]
        );
        assert_eq!(
            target.signals.lock().expect("signals lock").as_slice(),
            &[(15, 29)]
        );
    }
}
