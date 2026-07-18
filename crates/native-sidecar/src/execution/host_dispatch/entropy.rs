use super::*;
use agentos_execution::host::EntropyOperation;

pub(super) struct EntropyCapability;

impl SidecarHostCapability<EntropyOperation> for EntropyCapability {
    fn requires_claim(_: &EntropyOperation) -> bool {
        // Entropy is an observable host-side effect. Claiming first prevents a
        // stale execution from consuming randomness after its reply capability
        // has already been superseded.
        true
    }

    fn execute(
        _: &mut SidecarKernel,
        _: &mut ActiveProcess,
        operation: EntropyOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let mut bytes = vec![0_u8; operation.length.get()];
        getrandom::getrandom(&mut bytes)
            .map_err(|error| HostServiceError::new("EIO", error.to_string()))?;
        Ok(HostCallReply::Raw(bytes))
    }
}
