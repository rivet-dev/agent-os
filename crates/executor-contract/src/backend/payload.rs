use super::HostServiceError;
use serde::Serialize;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const NEAR_LIMIT_PERCENT: usize = 80;
const REARM_PERCENT: usize = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NearLimitWarning {
    pub limit_name: &'static str,
    pub limit: usize,
    pub observed: usize,
}

pub trait NearLimitWarningHook: Send + Sync {
    fn warn(&self, warning: NearLimitWarning);
}

struct StderrNearLimitWarningHook;

impl NearLimitWarningHook for StderrNearLimitWarningHook {
    fn warn(&self, warning: NearLimitWarning) {
        eprintln!(
            "WARN_AGENTOS_PAYLOAD_NEAR_LIMIT: limit={} observed={} maximum={}",
            warning.limit_name, warning.observed, warning.limit
        );
    }
}

struct PayloadLimitState {
    limit_name: &'static str,
    maximum: usize,
    warning_hook: Option<Arc<dyn NearLimitWarningHook>>,
    warning_active: AtomicBool,
}

/// A named admission bound supplied by the layer that owns configuration.
///
/// The common execution layer deliberately provides no product default. A
/// sidecar or adapter must pass the configured name and value at construction.
#[derive(Clone)]
pub struct PayloadLimit {
    inner: Arc<PayloadLimitState>,
}

impl fmt::Debug for PayloadLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PayloadLimit")
            .field("limit_name", &self.inner.limit_name)
            .field("maximum", &self.inner.maximum)
            .finish_non_exhaustive()
    }
}

impl PayloadLimit {
    pub fn new(limit_name: &'static str, maximum: usize) -> Result<Self, HostServiceError> {
        Self::with_stderr_warning(limit_name, maximum)
    }

    pub fn with_stderr_warning(
        limit_name: &'static str,
        maximum: usize,
    ) -> Result<Self, HostServiceError> {
        Self::with_warning_hook(
            limit_name,
            maximum,
            Some(Arc::new(StderrNearLimitWarningHook)),
        )
    }

    pub fn with_warning_hook(
        limit_name: &'static str,
        maximum: usize,
        warning_hook: Option<Arc<dyn NearLimitWarningHook>>,
    ) -> Result<Self, HostServiceError> {
        if limit_name.is_empty() {
            return Err(HostServiceError::new(
                "EINVAL",
                "payload limit name must not be empty",
            ));
        }
        if maximum == 0 {
            return Err(HostServiceError::new(
                "EINVAL",
                format!("{limit_name} must be greater than zero"),
            ));
        }
        Ok(Self {
            inner: Arc::new(PayloadLimitState {
                limit_name,
                maximum,
                warning_hook,
                warning_active: AtomicBool::new(false),
            }),
        })
    }

    pub fn name(&self) -> &'static str {
        self.inner.limit_name
    }

    pub fn maximum(&self) -> usize {
        self.inner.maximum
    }

    pub fn admit(&self, observed: usize) -> Result<(), HostServiceError> {
        self.update_warning(observed);
        if observed > self.inner.maximum {
            return Err(HostServiceError::limit(
                "E2BIG",
                self.inner.limit_name,
                self.inner.maximum as u64,
                observed as u64,
            ));
        }
        Ok(())
    }

    pub fn admit_json<T: Serialize + ?Sized>(&self, value: &T) -> Result<usize, HostServiceError> {
        let mut writer = LimitedCountingWriter::new(self.inner.maximum);
        match serde_json::to_writer(&mut writer, value) {
            Ok(()) => {
                self.admit(writer.observed)?;
                Ok(writer.observed)
            }
            Err(_) if writer.exceeded => {
                let observed = self.inner.maximum.saturating_add(1);
                self.update_warning(observed);
                Err(HostServiceError::limit(
                    "E2BIG",
                    self.inner.limit_name,
                    self.inner.maximum as u64,
                    observed as u64,
                ))
            }
            Err(error) => Err(HostServiceError::new("EIO", error.to_string())),
        }
    }

    fn update_warning(&self, observed: usize) {
        let Some(hook) = &self.inner.warning_hook else {
            return;
        };
        let near = observed != 0
            && observed.saturating_mul(100)
                >= self.inner.maximum.saturating_mul(NEAR_LIMIT_PERCENT);
        if near {
            if !self.inner.warning_active.swap(true, Ordering::AcqRel) {
                hook.warn(NearLimitWarning {
                    limit_name: self.inner.limit_name,
                    limit: self.inner.maximum,
                    observed,
                });
            }
        } else if observed.saturating_mul(100) < self.inner.maximum.saturating_mul(REARM_PERCENT) {
            self.inner.warning_active.store(false, Ordering::Release);
        }
    }
}

struct LimitedCountingWriter {
    maximum: usize,
    observed: usize,
    exceeded: bool,
}

impl LimitedCountingWriter {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            observed: 0,
            exceeded: false,
        }
    }
}

impl io::Write for LimitedCountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.observed.saturating_add(bytes.len());
        if next > self.maximum {
            self.observed = self.maximum.saturating_add(1);
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded payload exceeds configured limit",
            ));
        }
        self.observed = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingWarnings(Mutex<Vec<NearLimitWarning>>);

    impl NearLimitWarningHook for RecordingWarnings {
        fn warn(&self, warning: NearLimitWarning) {
            self.0.lock().expect("warning lock").push(warning);
        }
    }

    #[test]
    fn named_payload_limit_admits_at_limit_and_rejects_plus_one_with_typed_details() {
        let limit =
            PayloadLimit::with_warning_hook("limits.test.maxBytes", 8, None).expect("named limit");

        limit.admit(8).expect("exact limit must be admitted");
        let error = limit.admit(9).expect_err("limit plus one must fail");
        assert_eq!(error.code, "E2BIG");
        let details = error.details.expect("typed limit details");
        assert_eq!(details["limitName"], "limits.test.maxBytes");
        assert_eq!(details["limit"], 8);
        assert_eq!(details["observed"], 9);
    }

    #[test]
    fn json_measurement_stops_without_allocating_an_encoded_payload() {
        let limit = PayloadLimit::new("maxReplyBytes", 4).expect("limit");
        let error = limit
            .admit_json(&serde_json::json!({ "payload": "too large" }))
            .expect_err("oversized JSON");
        assert_eq!(error.code, "E2BIG");
        assert_eq!(
            error.details.expect("details")["limitName"],
            "maxReplyBytes"
        );
    }

    #[test]
    fn near_limit_warning_is_coalesced_and_rearms_below_seventy_percent() {
        let warnings = Arc::new(RecordingWarnings::default());
        let limit = PayloadLimit::with_warning_hook("maxEventBytes", 100, Some(warnings.clone()))
            .expect("limit");

        limit.admit(80).expect("near limit");
        limit.admit(90).expect("same warning window");
        limit.admit(69).expect("rearm");
        limit.admit(81).expect("second warning window");

        let warnings = warnings.0.lock().expect("warning lock");
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].limit_name, "maxEventBytes");
        assert_eq!(warnings[1].observed, 81);
    }

    #[test]
    fn standard_constructor_enables_near_limit_warning_delivery() {
        let standard = PayloadLimit::new("maxEventBytes", 100).expect("standard limit");
        assert!(standard.inner.warning_hook.is_some());

        let deliberately_silent =
            PayloadLimit::with_warning_hook("maxSilentBytes", 100, None).expect("silent limit");
        assert!(deliberately_silent.inner.warning_hook.is_none());
    }
}
