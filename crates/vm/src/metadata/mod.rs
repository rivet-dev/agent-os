use crate::protocol::{ExtEnvelope, OwnershipScope, SidecarRequestPayload, SidecarResponsePayload};
use crate::state::SharedSidecarRequestClient;
use crate::VmError;
use agentos_vfs_storage::callback_store::CallbackMetadataClient;
use std::time::Duration;

pub(crate) use agentos_vfs_storage::CallbackMetadataStore;

impl CallbackMetadataClient for SharedSidecarRequestClient {
    type Ownership = OwnershipScope;
    type Error = VmError;

    fn invoke_metadata_callback(
        &self,
        ownership: Self::Ownership,
        namespace: &str,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<(String, Vec<u8>), Self::Error> {
        let payload = SidecarRequestPayload::Ext(ExtEnvelope {
            namespace: namespace.to_owned(),
            payload,
        });
        match self.invoke(ownership, payload, timeout)? {
            SidecarResponsePayload::ExtResult(envelope) => {
                Ok((envelope.namespace, envelope.payload))
            }
            other => Err(VmError::InvalidState(format!(
                "unexpected vfs metadata callback response payload: {other:?}"
            ))),
        }
    }
}
