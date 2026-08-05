#![forbid(unsafe_code)]

// AGENTOS_BROWSER_SUPPORT_DISABLED: retained for reference, but AgentOS is native-only.
/*
//! Agent OS browser sidecar wrapper.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::AgentOsBrowserSidecarWasm;

mod acp_host;

use std::collections::BTreeMap;
use std::sync::Mutex;

use agentos_native_sidecar_browser::{
    BrowserExtension, BrowserExtensionContext, BrowserSidecar, BrowserSidecarBridge,
    BrowserSidecarConfig, BrowserSidecarError,
};
use agentos_sidecar_core::{codec, error_response, AcpCore, AcpCoreError};

use crate::acp_host::BrowserAcpHost;

/// The browser ACP extension: decodes ACP wire requests, dispatches them through
/// the host-free `agentos-sidecar-core` engine, and drives the agent process via a
/// `BrowserAcpHost` over the converged executor. This crate stays host-free (no
/// tokio / native agentos-native-sidecar) so it compiles to wasm32; the kernel remains
/// the sole enforcement point and all guest syscalls route through the converged
/// sync bridge. `Mutex` (not `RefCell`) satisfies the `Send + Sync` trait bound; the
/// browser runs single-threaded so there is no real contention.
pub struct BrowserAcpExtension {
    core: Mutex<AcpCore>,
    /// process_id -> execution_id, persisted across requests for a session.
    executions: Mutex<BTreeMap<String, String>>,
}

impl BrowserAcpExtension {
    pub fn new() -> Self {
        Self {
            core: Mutex::new(AcpCore::new()),
            executions: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Default for BrowserAcpExtension {
    fn default() -> Self {
        Self::new()
    }
}

fn to_browser_error(error: AcpCoreError) -> BrowserSidecarError {
    BrowserSidecarError::InvalidState(error.to_string())
}

impl BrowserExtension for BrowserAcpExtension {
    fn namespace(&self) -> &str {
        agentos_protocol::ACP_EXTENSION_NAMESPACE
    }

    fn handle_request(
        &self,
        context: &mut BrowserExtensionContext<'_>,
        payload: &[u8],
    ) -> Result<Vec<u8>, BrowserSidecarError> {
        let request = codec::decode_request(payload).map_err(to_browser_error)?;
        let connection_id = context.connection_id().unwrap_or_default().to_string();
        let vm_id = context
            .vm_id()
            .ok_or_else(|| {
                BrowserSidecarError::InvalidState(String::from(
                    "ACP requests require VM ownership (no vm_id on the request)",
                ))
            })?
            .to_string();

        let mut executions = self.executions.lock().map_err(|_| {
            BrowserSidecarError::InvalidState(String::from("ACP executions lock poisoned"))
        })?;
        let mut host = BrowserAcpHost::new(context, vm_id, &mut executions);
        let mut core = self.core.lock().map_err(|_| {
            BrowserSidecarError::InvalidState(String::from("ACP core lock poisoned"))
        })?;
        // Browser uses the RESUMABLE path (AGENTOS-WEB-ASYNC-AGENTS.md §3.2.1):
        // create_session / session/prompt return AcpPending{processId} and the kernel
        // worker drives them via deliver_agent_output, so the worker never blocks
        // inside pushFrame while the agent makes a mid-turn syscall. A handler error
        // becomes an `AcpErrorResponse` (matching native), not a rejected wire frame.
        let response = match core.dispatch_resumable(&mut host, &connection_id, request) {
            Ok(response) => response,
            Err(error) => error_response(&error),
        };
        codec::encode_response(&response).map_err(to_browser_error)
    }
}

pub fn extensions() -> Vec<Box<dyn BrowserExtension>> {
    vec![Box::new(BrowserAcpExtension::new())]
}

pub fn browser_sidecar<B>(
    bridge: B,
    config: BrowserSidecarConfig,
) -> Result<BrowserSidecar<B>, BrowserSidecarError>
where
    B: BrowserSidecarBridge,
    <B as agentos_bridge::BridgeTypes>::Error: std::fmt::Debug,
{
    BrowserSidecar::with_extensions(bridge, config, extensions())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_protocol::ACP_EXTENSION_NAMESPACE;

    #[test]
    fn browser_extensions_register_acp_namespace() {
        let extensions = extensions();

        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0].namespace(), ACP_EXTENSION_NAMESPACE);
    }

    #[test]
    fn browser_acp_extension_rejects_invalid_payload() {
        // A bogus payload fails ACP wire decoding before any host work — proving the
        // request is now routed through the core codec (not a stub).
        let extension = BrowserAcpExtension::new();
        let mut host = NullBrowserExtensionHost;
        let mut context = BrowserExtensionContext::new(&mut host);

        let error = extension
            .handle_request(&mut context, b"not-a-valid-acp-frame")
            .expect_err("invalid ACP payload must be rejected");
        assert!(error.to_string().contains("invalid ACP request"));
    }

    #[test]
    fn browser_acp_extension_requires_vm_ownership() {
        // A well-formed request with no vm_id on the context fails closed (the seam
        // that threads vm ownership into the extension is exercised).
        use agentos_protocol::generated::v1::{AcpGetSessionStateRequest, AcpRequest};
        let extension = BrowserAcpExtension::new();
        let mut host = NullBrowserExtensionHost;
        let mut context = BrowserExtensionContext::new(&mut host);

        let payload = serde_bare::to_vec(&AcpRequest::AcpGetSessionStateRequest(
            AcpGetSessionStateRequest {
                session_id: "s1".into(),
            },
        ))
        .expect("encode");
        let error = extension
            .handle_request(&mut context, &payload)
            .expect_err("missing vm ownership must fail");
        assert!(error.to_string().contains("require VM ownership"));
    }

    struct NullBrowserExtensionHost;

    impl agentos_native_sidecar_browser::BrowserExtensionHost for NullBrowserExtensionHost {
        fn write_file(
            &mut self,
            _vm_id: &str,
            _path: &str,
            _contents: Vec<u8>,
        ) -> Result<(), BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn read_file(&mut self, _vm_id: &str, _path: &str) -> Result<Vec<u8>, BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn mkdir(
            &mut self,
            _vm_id: &str,
            _path: &str,
            _recursive: bool,
        ) -> Result<(), BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn read_dir(
            &mut self,
            _vm_id: &str,
            _path: &str,
        ) -> Result<Vec<String>, BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn create_javascript_context(
            &mut self,
            _request: agentos_bridge::CreateJavascriptContextRequest,
        ) -> Result<agentos_bridge::GuestContextHandle, BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn create_wasm_context(
            &mut self,
            _request: agentos_bridge::CreateWasmContextRequest,
        ) -> Result<agentos_bridge::GuestContextHandle, BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn start_execution(
            &mut self,
            _request: agentos_bridge::StartExecutionRequest,
        ) -> Result<agentos_bridge::StartedExecution, BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn write_stdin(
            &mut self,
            _request: agentos_bridge::WriteExecutionStdinRequest,
        ) -> Result<(), BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn close_stdin(
            &mut self,
            _request: agentos_bridge::ExecutionHandleRequest,
        ) -> Result<(), BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn kill_execution(
            &mut self,
            _request: agentos_bridge::KillExecutionRequest,
        ) -> Result<(), BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }

        fn poll_execution_event(
            &mut self,
            _request: agentos_bridge::PollExecutionEventRequest,
        ) -> Result<Option<agentos_bridge::ExecutionEvent>, BrowserSidecarError> {
            unreachable!("test ACP extension does not call browser context")
        }
    }
}
*/
