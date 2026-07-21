use super::*;
use agentos_kernel::process_table::{SigmaskHow, SignalAction, SignalDisposition, SignalSet};

pub(super) struct SignalCapability;

impl SidecarHostCapability<SignalOperation> for SignalCapability {
    fn requires_claim(operation: &SignalOperation) -> bool {
        matches!(
            operation,
            SignalOperation::RegisterThread { .. }
                | SignalOperation::UnregisterThread { .. }
                | SignalOperation::SetAction { .. }
                | SignalOperation::UpdateMask { .. }
                | SignalOperation::UpdateMaskForThread { .. }
                | SignalOperation::BeginDelivery
                | SignalOperation::BeginDeliveryForThread { .. }
                | SignalOperation::TakePublishedDelivery
                | SignalOperation::TakePublishedDeliveryForThread { .. }
                | SignalOperation::EndDelivery { .. }
                | SignalOperation::EndDeliveryForThread { .. }
                | SignalOperation::BeginTemporaryMask { .. }
                | SignalOperation::EndTemporaryMask { .. }
                | SignalOperation::BeginTemporaryMaskForThread { .. }
                | SignalOperation::EndTemporaryMaskForThread { .. }
        )
    }

    fn execute(
        _: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: SignalOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let value = match operation {
            SignalOperation::RegisterThread {
                thread_id,
                inherit_from,
            } => {
                process
                    .kernel_handle
                    .register_signal_thread(thread_id, inherit_from)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            SignalOperation::UnregisterThread { thread_id } => {
                process
                    .kernel_handle
                    .unregister_signal_thread(thread_id)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            SignalOperation::GetAction { signal } => {
                let action = process
                    .kernel_handle
                    .signal_action(signal, None)
                    .map_err(kernel_host_error)?;
                signal_action_value(action)
            }
            SignalOperation::SetAction { signal, action } => {
                process
                    .kernel_handle
                    .signal_action(signal, Some(kernel_signal_action(action)?))
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            SignalOperation::BeginDelivery => {
                materialize_real_timer_signal(process);
                process
                    .kernel_handle
                    .begin_signal_delivery()
                    .map_err(kernel_host_error)?
                    .map(|delivery| {
                        json!({
                            "signal": delivery.signal,
                            "token": delivery.token,
                            "flags": delivery.action.flags,
                        })
                    })
                    .unwrap_or(Value::Null)
            }
            SignalOperation::BeginDeliveryForThread { thread_id } => {
                materialize_real_timer_signal(process);
                process
                    .kernel_handle
                    .begin_signal_delivery_for_thread(thread_id)
                    .map_err(kernel_host_error)?
                    .map(signal_delivery_value)
                    .unwrap_or(Value::Null)
            }
            SignalOperation::TakePublishedDelivery => {
                materialize_real_timer_signal(process);
                process
                    .apply_runtime_controls()
                    .map_err(|error| host_service_error(&error))?;
                let identity = process.kernel_handle.runtime_identity();
                let delivery = ExecutionBackend::take_signal_checkpoint(
                    &process.execution,
                    ExecutionWakeIdentity {
                        generation: identity.generation,
                        pid: identity.pid,
                    },
                )?;
                // apply_runtime_controls publishes at most one strict-LIFO
                // delivery at a time. Once the executor takes it, the sidecar
                // admission guard must no longer treat that checkpoint as
                // queued; otherwise every later ppoll spuriously returns EINTR.
                process.guest_signal_checkpoint_pending = false;
                delivery
                    .map(|delivery| {
                        json!({
                            "signal": delivery.signal,
                            "token": delivery.delivery_token,
                            "flags": delivery.flags,
                        })
                    })
                    .unwrap_or(Value::Null)
            }
            SignalOperation::TakePublishedDeliveryForThread { thread_id } => {
                materialize_real_timer_signal(process);
                process
                    .apply_runtime_controls()
                    .map_err(|error| host_service_error(&error))?;
                let identity = process.kernel_handle.runtime_identity();
                let delivery = process.execution.take_signal_checkpoint_for_thread(
                    ExecutionWakeIdentity {
                        generation: identity.generation,
                        pid: identity.pid,
                    },
                    thread_id,
                )?;
                process.guest_signal_checkpoint_pending = false;
                delivery
                    .map(|delivery| {
                        json!({
                            "signal": delivery.signal,
                            "token": delivery.delivery_token,
                            "flags": delivery.flags,
                            "threadId": delivery.thread_id,
                        })
                    })
                    .unwrap_or(Value::Null)
            }
            SignalOperation::EndDelivery { token } => {
                process
                    .kernel_handle
                    .end_signal_delivery(token)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            SignalOperation::EndDeliveryForThread { thread_id, token } => {
                process
                    .kernel_handle
                    .end_signal_delivery_for_thread(thread_id, token)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            SignalOperation::UpdateMask { how, set } => {
                let previous = process
                    .kernel_handle
                    .sigprocmask(kernel_mask_how(how), kernel_signal_set(set)?)
                    .map_err(kernel_host_error)?;
                json!({ "signals": previous.signals() })
            }
            SignalOperation::UpdateMaskForThread {
                thread_id,
                how,
                set,
            } => {
                let previous = process
                    .kernel_handle
                    .sigprocmask_for_thread(
                        thread_id,
                        kernel_mask_how(how),
                        kernel_signal_set(set)?,
                    )
                    .map_err(kernel_host_error)?;
                json!({ "signals": previous.signals() })
            }
            SignalOperation::Pending => json!({
                "signals": process
                    .kernel_handle
                    .sigpending()
                    .map_err(kernel_host_error)?
                    .signals(),
            }),
            SignalOperation::BeginTemporaryMask { mask } => {
                let token = process
                    .kernel_handle
                    .begin_temporary_signal_mask(kernel_signal_set(mask)?)
                    .map_err(kernel_host_error)?;
                json!(token)
            }
            SignalOperation::EndTemporaryMask { token } => {
                process
                    .kernel_handle
                    .end_temporary_signal_mask(token)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            SignalOperation::BeginTemporaryMaskForThread { thread_id, mask } => {
                let token = process
                    .kernel_handle
                    .begin_temporary_signal_mask_for_thread(thread_id, kernel_signal_set(mask)?)
                    .map_err(kernel_host_error)?;
                json!(token)
            }
            SignalOperation::EndTemporaryMaskForThread { thread_id, token } => {
                process
                    .kernel_handle
                    .end_temporary_signal_mask_for_thread(thread_id, token)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            other => return Err(unsupported("signal", other)),
        };
        Ok(HostCallReply::Json(value))
    }
}

fn signal_delivery_value(delivery: agentos_kernel::process_table::SignalDelivery) -> Value {
    json!({
        "signal": delivery.signal,
        "token": delivery.token,
        "flags": delivery.action.flags,
        "threadId": delivery.thread_id,
    })
}

pub(super) fn materialize_real_timer_signal(process: &ActiveProcess) {
    if process.real_interval_timer.take_expiry() {
        process.kernel_handle.kill(libc::SIGALRM);
    }
}

fn kernel_signal_action(action: SignalActionValue) -> Result<SignalAction, HostServiceError> {
    Ok(SignalAction {
        disposition: match action.disposition {
            SignalDispositionValue::Default => SignalDisposition::Default,
            SignalDispositionValue::Ignore => SignalDisposition::Ignore,
            SignalDispositionValue::User => SignalDisposition::User,
        },
        mask: kernel_signal_set(action.mask)?,
        flags: action.flags,
    })
}

fn signal_action_value(action: SignalAction) -> Value {
    let disposition = match action.disposition {
        SignalDisposition::Default => "default",
        SignalDisposition::Ignore => "ignore",
        SignalDisposition::User => "user",
    };
    json!({
        "action": disposition,
        "mask": action.mask.signals(),
        "flags": action.flags,
    })
}

fn kernel_mask_how(how: SignalMaskHow) -> SigmaskHow {
    match how {
        SignalMaskHow::Block => SigmaskHow::Block,
        SignalMaskHow::Unblock => SigmaskHow::Unblock,
        SignalMaskHow::Set => SigmaskHow::SetMask,
    }
}

fn kernel_signal_set(set: SignalSetValue) -> Result<SignalSet, HostServiceError> {
    let signals = (1..=64)
        .filter(|signal| set.0 & (1_u64 << (signal - 1)) != 0)
        .collect::<Vec<_>>();
    SignalSet::from_signals(signals)
        .map_err(|error| HostServiceError::new(error.code(), error.to_string()))
}
