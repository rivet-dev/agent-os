//! wasm-bindgen entry point for the Agent OS browser sidecar.
//!
//! Mirrors agentos's `BrowserSidecarWasm` (pushFrame/pollEvent over the shared
//! `BrowserWireDispatcher` + `BrowserJsBridge`) but registers the Agent OS ACP
//! `BrowserExtension` into the dispatcher's sidecar, so guest ACP/session traffic
//! is handled by the Agent OS wrapper while every kernel syscall still routes
//! through the converged agentos wasm kernel (the sole enforcement point).

use agentos_native_sidecar_browser::wire_dispatch::{BrowserWireDispatcher, BROWSER_SIDECAR_ID};
use agentos_native_sidecar_browser::BrowserJsBridge;
use js_sys::{Error as JsError, Uint8Array};
use wasm_bindgen::prelude::*;

use crate::BrowserAcpExtension;

#[wasm_bindgen]
pub struct AgentOsBrowserSidecarWasm {
    dispatcher: BrowserWireDispatcher<BrowserJsBridge>,
}

#[wasm_bindgen]
impl AgentOsBrowserSidecarWasm {
    #[wasm_bindgen(constructor)]
    pub fn new(host_bridge: Option<JsValue>) -> Result<AgentOsBrowserSidecarWasm, JsValue> {
        let mut dispatcher = BrowserWireDispatcher::new(BrowserJsBridge::new(host_bridge));
        dispatcher
            .sidecar_mut()
            .register_extension(Box::new(BrowserAcpExtension::new()))
            .map_err(js_error)?;
        Ok(Self { dispatcher })
    }

    #[wasm_bindgen(getter, js_name = sidecarId)]
    pub fn sidecar_id(&self) -> String {
        String::from(BROWSER_SIDECAR_ID)
    }

    #[wasm_bindgen(js_name = pushFrame)]
    pub fn push_frame(&mut self, frame: Uint8Array) -> Result<JsValue, JsValue> {
        let bytes = frame.to_vec();
        let response = self
            .dispatcher
            .handle_request_bytes(&bytes)
            .map_err(js_error)?;
        Ok(Uint8Array::from(response.as_slice()).into())
    }

    #[wasm_bindgen(js_name = pollEvent)]
    pub fn poll_event(&mut self) -> Result<JsValue, JsValue> {
        match self.dispatcher.poll_event_bytes().map_err(js_error)? {
            Some(event) => Ok(Uint8Array::from(event.as_slice()).into()),
            None => Ok(JsValue::NULL),
        }
    }
}

fn js_error(error: impl ToString) -> JsValue {
    JsError::new(&error.to_string()).into()
}
