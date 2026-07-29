use super::*;

impl AcpExtension {
    pub(super) async fn prompt_durable_session(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpPromptRequest,
    ) -> AcpHandlerOutput {
        let session_id = match default_session_id(request.session_id) {
            Ok(id) => id,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let store = match self.session_store(ctx).await {
            Ok(store) => store,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let mut session = match required_stored_session(&store, &session_id).await {
            Ok(session) => session,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let limits = match ctx.vm_acp_limits().await {
            Ok(limits) => limits,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let content = match parse_content_blocks(&request.content, &session_id, &limits) {
            Ok(content) => content,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let input_json = match serde_json::to_string(&json!({
            "formatVersion": 1,
            "content": content,
        })) {
            Ok(value) => value,
            Err(error) => {
                return AcpHandlerOutput::response(Err(SidecarError::InvalidState(
                    error.to_string(),
                )));
            }
        };
        let input_hash = Sha256::digest(input_json.as_bytes()).to_vec();
        if let Some(key) = request.idempotency_key.as_deref() {
            if key.is_empty() || key.len() > 256 {
                return AcpHandlerOutput::response(Err(SidecarError::InvalidState(String::from(
                    "invalid_idempotency_key: idempotencyKey must contain 1..=256 bytes",
                ))));
            }
            match store.prompt_by_idempotency_key(&session_id, key).await {
                Ok(Some(existing)) if existing.input_hash != input_hash => {
                    return AcpHandlerOutput::response(Err(SidecarError::InvalidState(
                        String::from(
                            "idempotency_conflict: key was already used with different prompt content",
                        ),
                    )));
                }
                Ok(Some(existing)) if existing.state == "completed" => {
                    return AcpHandlerOutput::response(
                        existing
                            .result_json
                            .as_deref()
                            .ok_or_else(|| {
                                SidecarError::InvalidState(String::from(
                                    "invalid stored completed prompt result",
                                ))
                            })
                            .and_then(prompt_response_from_json),
                    );
                }
                Ok(Some(existing)) if existing.state == "failed" => {
                    return AcpHandlerOutput::response(Err(SidecarError::InvalidState(
                        existing.error_json.unwrap_or_else(|| {
                            String::from("stored prompt failed without serialized error")
                        }),
                    )));
                }
                Ok(Some(existing)) => {
                    return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                        "prompt_in_progress: idempotent prompt {} has not reached a terminal state",
                        existing.prompt_id
                    ))));
                }
                Ok(None) => {}
                Err(error) => {
                    return AcpHandlerOutput::response(Err(session_store_error(error)));
                }
            }
        }
        if session.state == "running" {
            return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                "session_busy: session {session_id} already has an active prompt"
            ))));
        }
        session = match self.ensure_durable_runtime(ctx, &store, session).await {
            Ok(session) => session,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        if session.acp_session_id.is_none() {
            return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                "session_restore_failed: session {session_id} has no private ACP id"
            ))));
        }
        let prompt_id = uuid::Uuid::new_v4().to_string();
        let user_message_id = uuid::Uuid::new_v4().to_string();
        let user_updates = content
            .iter()
            .map(|content| {
                json!({
                    "sessionUpdate": "user_message_chunk",
                    "content": content,
                    "messageId": user_message_id,
                })
            })
            .collect::<Vec<_>>();
        let accepted = match store
            .accept_prompt(
                &session_id,
                &prompt_id,
                request.idempotency_key.as_deref(),
                input_hash,
                &user_updates,
            )
            .await
        {
            Ok(events) => events,
            Err(error) => return AcpHandlerOutput::response(Err(session_store_error(error))),
        };
        let mut events = Vec::new();
        let mut sink = match DurableUpdateSink::new(
            store.clone(),
            &session,
            Some(prompt_id.clone()),
            limits,
            Arc::clone(&self.pending_permission_responses),
        ) {
            Ok(sink) => sink,
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    None,
                    "invalid_session_options",
                    error,
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        if let Some(last) = accepted.last() {
            sink.latest_sequence = last.sequence;
        }
        if let Err(error) = sink.emit_stored(ctx, &mut events, &accepted) {
            let error = finish_prompt_failure(
                &store,
                &session_id,
                &prompt_id,
                sink.last_output_sequence,
                "event_delivery_failed",
                error,
            )
            .await;
            return AcpHandlerOutput {
                response: Err(error),
                events,
            };
        }

        let params = match serde_json::to_string(&json!({ "prompt": content })) {
            Ok(params) => params,
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "prompt_serialization_failed",
                    SidecarError::InvalidState(error.to_string()),
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        let cancellation_key = durable_route_key(ctx.ownership(), &session_id);
        let (cancellation_sender, mut cancellation_receiver) = tokio::sync::watch::channel(false);
        if let Ok(mut cancellations) = self.prompt_cancellations.lock() {
            cancellations.insert(cancellation_key.clone(), cancellation_sender);
        } else {
            let error = finish_prompt_failure(
                &store,
                &session_id,
                &prompt_id,
                sink.last_output_sequence,
                "prompt_cancellation_registry_failed",
                SidecarError::InvalidState(String::from(
                    "prompt cancellation registry is poisoned",
                )),
            )
            .await;
            return AcpHandlerOutput {
                response: Err(error),
                events,
            };
        }
        let raw = self
            .send_runtime_request_with_sink(
                ctx,
                AcpSessionRequest {
                    session_id: cancellation_key.clone(),
                    method: String::from("session/prompt"),
                    params: Some(params),
                },
                Some(&mut sink),
                Some(&mut cancellation_receiver),
            )
            .await;
        if let Ok(mut cancellations) = self.prompt_cancellations.lock() {
            cancellations.remove(&cancellation_key);
        } else {
            eprintln!(
                "ERR_AGENTOS_PROMPT_CANCELLATION_REGISTRY: failed to remove completed prompt {session_id}"
            );
        }
        events.extend(raw.events);
        let rpc = match raw.response {
            Ok(AcpResponse::AcpSessionRpcResponse(response)) => response,
            Ok(other) => {
                let error = SidecarError::InvalidState(format!(
                    "invalid prompt response variant: {other:?}"
                ));
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "invalid_prompt_response",
                    error,
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "prompt_interrupted",
                    error,
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        let response: Value = match serde_json::from_str(&rpc.response) {
            Ok(response) => response,
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "invalid_prompt_response",
                    SidecarError::InvalidState(format!(
                        "invalid ACP prompt response JSON: {error}"
                    )),
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        let result = match response_result(response, "ACP session/prompt") {
            Ok(result) => result,
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "prompt_failed",
                    error,
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        let stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_owned();
        if let Err(parse_error) = serde_json::from_value::<
            agent_client_protocol_schema::v1::StopReason,
        >(Value::String(stop_reason.clone()))
        {
            let error = finish_prompt_failure(
                &store,
                &session_id,
                &prompt_id,
                sink.last_output_sequence,
                "invalid_stop_reason",
                SidecarError::InvalidState(format!(
                    "unsupported ACP stop reason {stop_reason}: {parse_error}"
                )),
            )
            .await;
            return AcpHandlerOutput {
                response: Err(error),
                events,
            };
        }
        // Buffered message/thought chunks are live-only until the adapter has
        // completed the prompt successfully. Never turn partial output from a
        // failed, cancelled, timed-out, or crashed turn into durable history.
        if let Err(error) = sink.flush(ctx, &mut events).await {
            let error = finish_prompt_failure(
                &store,
                &session_id,
                &prompt_id,
                sink.last_output_sequence,
                "history_commit_failed",
                error,
            )
            .await;
            return AcpHandlerOutput {
                response: Err(error),
                events,
            };
        }
        let message = match sink.message_json() {
            Ok(message) => message,
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "message_serialization_failed",
                    error,
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        let message_value = match message
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
        {
            Ok(message) => message,
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "message_serialization_failed",
                    SidecarError::InvalidState(format!(
                        "failed to decode completed message JSON: {error}"
                    )),
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        let result_json = match serde_json::to_string(&json!({
            "sessionId": session_id,
            "message": message_value,
            "stopReason": stop_reason,
        })) {
            Ok(result) => result,
            Err(error) => {
                let error = finish_prompt_failure(
                    &store,
                    &session_id,
                    &prompt_id,
                    sink.last_output_sequence,
                    "result_serialization_failed",
                    SidecarError::InvalidState(error.to_string()),
                )
                .await;
                return AcpHandlerOutput {
                    response: Err(error),
                    events,
                };
            }
        };
        if let Err(error) = store
            .finish_prompt(
                &session_id,
                &prompt_id,
                &[],
                sink.last_output_sequence,
                Some(&result_json),
                None,
            )
            .await
        {
            return AcpHandlerOutput {
                response: Err(session_store_error(error)),
                events,
            };
        }
        AcpHandlerOutput {
            response: Ok(AcpResponse::AcpPromptResponse(AcpPromptResponse {
                session_id,
                message,
                stop_reason,
            })),
            events,
        }
    }

    pub(super) async fn cancel_durable_prompt(
        &self,
        _ctx: &mut ExtensionContext<'_>,
        _request: AcpCancelPromptRequest,
    ) -> AcpHandlerOutput {
        AcpHandlerOutput::response(Ok(AcpResponse::AcpCancelPromptResponse(
            AcpCancelPromptResponse {
                status: String::from("no_active_prompt"),
            },
        )))
    }

    pub(super) async fn respond_durable_permission(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpRespondPermissionRequest,
    ) -> AcpHandlerOutput {
        let store = match self.session_store(ctx).await {
            Ok(store) => store,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let resolution = match store
            .pending_request_resolution(&request.session_id, &request.request_id)
            .await
        {
            Ok(resolution) => resolution,
            Err(error) => return AcpHandlerOutput::response(Err(session_store_error(error))),
        };
        let status = permission_terminal_response(resolution);
        AcpHandlerOutput::response(Ok(AcpResponse::AcpRespondPermissionResponse(status)))
    }

    pub(super) async fn set_durable_config(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpSetSessionConfigOptionRequest,
    ) -> AcpHandlerOutput {
        let session_id = match default_session_id(request.session_id) {
            Ok(id) => id,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let value: Value = match serde_json::from_str(&request.value) {
            Ok(value @ Value::String(_)) | Ok(value @ Value::Bool(_)) => value,
            Ok(_) => {
                return AcpHandlerOutput::response(Err(SidecarError::InvalidState(String::from(
                    "invalid_config_value: ACP configuration values must be strings or booleans",
                ))));
            }
            Err(error) => {
                return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                    "invalid_config_value: {error}"
                ))));
            }
        };
        let store = match self.session_store(ctx).await {
            Ok(store) => store,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let session = match required_stored_session(&store, &session_id).await {
            Ok(session) => session,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        if session.state == "running" {
            return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                "session_busy: cannot set configuration while {session_id} is running"
            ))));
        }
        let session = match self.ensure_durable_runtime(ctx, &store, session).await {
            Ok(session) => session,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        if session.acp_session_id.is_none() {
            return AcpHandlerOutput::response(Err(SidecarError::InvalidState(String::from(
                "session_restore_failed: missing private ACP id",
            ))));
        }
        let mut params = Map::from_iter([
            (String::from("configId"), Value::String(request.config_id)),
            (String::from("value"), value.clone()),
        ]);
        if value.is_boolean() {
            params.insert(String::from("type"), Value::String(String::from("boolean")));
        }
        let params = match serde_json::to_string(&Value::Object(params)) {
            Ok(params) => params,
            Err(error) => {
                return AcpHandlerOutput::response(Err(SidecarError::InvalidState(
                    error.to_string(),
                )))
            }
        };
        let limits = match ctx.vm_acp_limits().await {
            Ok(limits) => limits,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let mut sink = match DurableUpdateSink::new(
            store.clone(),
            &session,
            None,
            limits,
            Arc::clone(&self.pending_permission_responses),
        ) {
            Ok(sink) => sink,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let mut output = self
            .send_runtime_request_with_sink(
                ctx,
                AcpSessionRequest {
                    session_id: durable_route_key(ctx.ownership(), &session_id),
                    method: String::from("session/set_config_option"),
                    params: Some(params),
                },
                Some(&mut sink),
                None,
            )
            .await;
        if output.response.is_err() {
            return output;
        }
        let response = match &output.response {
            Ok(AcpResponse::AcpSessionRpcResponse(response)) => {
                match serde_json::from_str::<Value>(&response.response) {
                    Ok(response) => response,
                    Err(error) => {
                        return AcpHandlerOutput {
                            response: Err(SidecarError::InvalidState(format!(
                                "invalid ACP config update response JSON: {error}"
                            ))),
                            events: output.events,
                        };
                    }
                }
            }
            Ok(_) => {
                return AcpHandlerOutput {
                    response: Err(SidecarError::InvalidState(String::from(
                        "invalid ACP config update response",
                    ))),
                    events: output.events,
                };
            }
            Err(_) => unreachable!("ACP transport errors returned above"),
        };
        if let Err(error) = response_result(response, "ACP session/set_config_option") {
            return AcpHandlerOutput {
                response: Err(error),
                events: output.events,
            };
        }
        if let Err(error) = sink.flush(ctx, &mut output.events).await {
            return AcpHandlerOutput {
                response: Err(error),
                events: output.events,
            };
        }
        let route_key = durable_route_key(ctx.ownership(), &session_id);
        let runtime = match self.sessions.lock().await.get(&route_key).cloned() {
            Some(runtime) => runtime,
            None => {
                return AcpHandlerOutput {
                    response: Err(SidecarError::InvalidState(String::from(
                        "ACP route disappeared after configuration update",
                    ))),
                    events: output.events,
                };
            }
        };
        let options = match json_strings_to_array_text(&runtime.config_options) {
            Ok(options) => options,
            Err(error) => {
                return AcpHandlerOutput {
                    response: Err(error),
                    events: output.events,
                };
            }
        };
        let revision = match store.replace_config(&session_id, &options).await {
            Ok(revision) => revision,
            Err(error) => {
                return AcpHandlerOutput {
                    response: Err(session_store_error(error)),
                    events: output.events,
                };
            }
        };
        AcpHandlerOutput {
            response: Ok(AcpResponse::AcpSessionConfigResponse(
                AcpSessionConfigResponse {
                    revision: u64::try_from(revision).unwrap_or(0),
                    options,
                },
            )),
            events: output.events,
        }
    }

    pub(super) fn signal_prompt_cancellation(&self, route_key: &str) -> bool {
        match self.prompt_cancellations.lock() {
            Ok(cancellations) => cancellations
                .get(route_key)
                .cloned()
                .is_some_and(|sender| sender.send(true).is_ok()),
            Err(_) => {
                eprintln!(
                    "ERR_AGENTOS_PROMPT_CANCELLATION_REGISTRY: cancellation registry is poisoned"
                );
                false
            }
        }
    }

    pub(super) fn cancel_pending_permissions(&self, route_key: &str, reason: &str) {
        let prefix = format!("{route_key}:");
        let mut pending = match self.pending_permission_responses.lock() {
            Ok(pending) => pending,
            Err(_) => {
                eprintln!(
                    "ERR_AGENTOS_PERMISSION_CANCEL: permission response registry is poisoned"
                );
                return;
            }
        };
        let request_keys = pending
            .keys()
            .filter(|request_key| request_key.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        for request_key in request_keys {
            if let Some(entry) = pending.remove(&request_key) {
                // The native adapter request ID remains private sidecar state
                // for the lifetime of the waiter and is deliberately discarded
                // without entering logs, history, or public responses.
                drop(entry.acp_request_id);
                if entry
                    .sender
                    .send(PendingPermissionSignal::Terminal(reason.to_owned()))
                    .is_err()
                {
                    eprintln!(
                        "ERR_AGENTOS_PERMISSION_CANCEL: permission waiter {request_key} closed before cancellation delivery"
                    );
                }
            }
        }
    }
}

pub(super) fn permission_terminal_response(
    resolution: PendingRequestResolution,
) -> AcpRespondPermissionResponse {
    match resolution {
        PendingRequestResolution::Terminal { reason, .. } => AcpRespondPermissionResponse {
            status: String::from("not_pending"),
            reason: Some(if reason == "accepted" {
                String::from("already_resolved")
            } else {
                reason
            }),
        },
        PendingRequestResolution::NotFound => AcpRespondPermissionResponse {
            status: String::from("not_pending"),
            reason: Some(String::from("request_not_found")),
        },
        PendingRequestResolution::Accepted(_) => {
            unreachable!("stored lookup cannot accept")
        }
    }
}

pub(super) struct DurableUpdateSink {
    store: SessionStore,
    user_session_id: String,
    latest_sequence: i64,
    buffered_kind: Option<String>,
    buffered_message_id: Option<Value>,
    buffered: Vec<Value>,
    agent_content: Vec<Value>,
    agent_message_id: Option<String>,
    last_output_sequence: Option<i64>,
    prompt_id: Option<String>,
    permission_policy: String,
    limits: agentos_native_sidecar::limits::AcpLimits,
    buffered_bytes: usize,
    turn_output_bytes: usize,
    warned_message_bytes: bool,
    warned_turn_bytes: bool,
    pending_permission_responses: Arc<StdMutex<BTreeMap<String, PendingPermissionResponse>>>,
}

impl DurableUpdateSink {
    pub(super) fn new(
        store: SessionStore,
        session: &StoredSession,
        prompt_id: Option<String>,
        limits: agentos_native_sidecar::limits::AcpLimits,
        pending_permission_responses: Arc<StdMutex<BTreeMap<String, PendingPermissionResponse>>>,
    ) -> Result<Self, SidecarError> {
        let permission_policy = session.permission_policy.clone();
        Ok(Self {
            store,
            user_session_id: session.session_id.clone(),
            latest_sequence: session.latest_sequence,
            buffered_kind: None,
            buffered_message_id: None,
            buffered: Vec::new(),
            agent_content: Vec::new(),
            agent_message_id: None,
            last_output_sequence: None,
            prompt_id,
            permission_policy,
            limits,
            buffered_bytes: 0,
            turn_output_bytes: 0,
            warned_message_bytes: false,
            warned_turn_bytes: false,
            pending_permission_responses,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_permission_request(
        &mut self,
        ctx: &mut ExtensionContext<'_>,
        process_id: &str,
        acp_session_id: &str,
        rpc_id: Option<&Value>,
        params: &Value,
        events: &mut Vec<agentos_native_sidecar::wire::EventFrame>,
        cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    ) -> Result<Value, SidecarError> {
        serde_json::from_value::<agent_client_protocol_schema::v1::RequestPermissionRequest>(
            params.clone(),
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!(
                "acp_protocol_error: invalid session/request_permission: {error}"
            ))
        })?;

        // Cancellation can win just before the adapter asks for permission. In
        // that ordering there is no waiter for `cancel_pending_permissions` to
        // remove, and `watch::Receiver::changed` will not wake for a value that was
        // already observed before the waiter starts. Reject the late request
        // immediately so a cancelled prompt cannot become stuck on permission.
        if cancellation
            .as_ref()
            .is_some_and(|receiver| *receiver.borrow())
        {
            return Ok(json!({ "outcome": { "outcome": "cancelled" } }));
        }

        let automatic = automatic_permission_option(&self.permission_policy, params)?;
        if self.permission_policy != "ask" {
            if let Some(option_id) = automatic {
                return Ok(json!({
                    "outcome": { "outcome": "selected", "optionId": option_id }
                }));
            }
            unreachable!("automatic policies either select an option or return a typed error");
        }

        let prompt_id = self.prompt_id.as_deref().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "permission request is only supported during an active durable prompt",
            ))
        })?;
        let acp_request_id = rpc_id.cloned().ok_or_else(|| {
            SidecarError::InvalidState(String::from("ACP inbound request missing private id"))
        })?;
        let (request_id, request_json) = public_permission_request(params, &self.user_session_id)?;
        let key = format!(
            "{}:{}",
            durable_route_key(ctx.ownership(), &self.user_session_id),
            request_id
        );
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let offered_option_ids = permission_option_ids(params);
        {
            let mut pending = self.pending_permission_responses.lock().map_err(|_| {
                SidecarError::InvalidState(String::from("permission response registry is poisoned"))
            })?;
            if pending
                .insert(
                    key.clone(),
                    PendingPermissionResponse {
                        offered_option_ids,
                        acp_request_id,
                        sender,
                    },
                )
                .is_some()
            {
                return Err(SidecarError::InvalidState(format!(
                    "duplicate pending permission request {request_id}"
                )));
            }
        }
        let stored_request = match self
            .store
            .create_pending_request(
                &self.user_session_id,
                prompt_id,
                &request_id,
                "permission",
                &request_json,
            )
            .await
        {
            Ok(event) => event,
            Err(error) => {
                if let Ok(mut pending) = self.pending_permission_responses.lock() {
                    pending.remove(&key);
                }
                return Err(session_store_error(error));
            }
        };
        self.emit_stored(ctx, events, std::slice::from_ref(&stored_request))?;

        let signal = wait_for_permission_signal(
            ctx,
            process_id,
            &request_id,
            &key,
            receiver,
            Arc::clone(&self.pending_permission_responses),
            cancellation,
            events,
        )
        .await?;
        let option_id = match signal {
            PendingPermissionSignal::Selected(option_id) => option_id,
            PendingPermissionSignal::Terminal(reason) => {
                let resolution = self
                    .store
                    .terminate_pending_request(
                        &self.user_session_id,
                        prompt_id,
                        &request_id,
                        &reason,
                    )
                    .await
                    .map_err(session_store_error)?;
                if let PendingRequestResolution::Terminal {
                    event: Some(event), ..
                } = resolution
                {
                    self.emit_stored(ctx, events, std::slice::from_ref(&event))?;
                }
                write_session_cancel_notification(ctx, process_id, acp_session_id).await?;
                return Ok(json!({ "outcome": { "outcome": "cancelled" } }));
            }
        };
        let result = json!({
            "outcome": { "outcome": "selected", "optionId": option_id }
        });
        let response_json = serde_json::to_string(&result)
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
        let committed = self
            .store
            .respond_pending_request(
                &self.user_session_id,
                prompt_id,
                &request_id,
                &response_json,
            )
            .await
            .map_err(session_store_error)?;
        match committed {
            PendingRequestResolution::Accepted(event) => {
                self.emit_stored(ctx, events, std::slice::from_ref(&event))?;
            }
            PendingRequestResolution::Terminal { reason, .. } => {
                return Err(SidecarError::InvalidState(format!(
                    "permission_response_conflict: request {request_id} is terminal: {reason}"
                )));
            }
            PendingRequestResolution::NotFound => {
                return Err(SidecarError::InvalidState(format!(
                    "permission_response_conflict: request {request_id} was not found"
                )));
            }
        }
        Ok(result)
    }

    pub(super) async fn handle_notification(
        &mut self,
        ctx: &ExtensionContext<'_>,
        notification: &Value,
        events: &mut Vec<agentos_native_sidecar::wire::EventFrame>,
    ) -> Result<bool, SidecarError> {
        if notification.get("method").and_then(Value::as_str) != Some("session/update") {
            return Ok(false);
        }
        let update = notification
            .get("params")
            .and_then(|params| params.get("update"))
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "acp_protocol_error: session/update missing params.update",
                ))
            })?
            .clone();
        serde_json::from_value::<agent_client_protocol_schema::v1::SessionUpdate>(update.clone())
            .map_err(|error| {
            SidecarError::InvalidState(format!(
                "acp_protocol_error: invalid SessionUpdate: {error}"
            ))
        })?;
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "acp_protocol_error: SessionUpdate missing sessionUpdate discriminator",
                ))
            })?;
        let update_bytes = serde_json::to_vec(&update)
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?
            .len();
        self.turn_output_bytes = checked_acp_bytes(
            &self.user_session_id,
            self.turn_output_bytes,
            update_bytes,
            self.limits.max_turn_output_bytes,
            "limits.acp.maxTurnOutputBytes",
        )?;
        warn_near_acp_limit(
            &mut self.warned_turn_bytes,
            &self.user_session_id,
            "turn output",
            self.turn_output_bytes,
            self.limits.max_turn_output_bytes,
            "limits.acp.maxTurnOutputBytes",
        );
        if matches!(kind, "agent_message_chunk" | "agent_thought_chunk") {
            let message_id = update.get("messageId").cloned();
            if self.buffered_kind.as_deref() != Some(kind) || self.buffered_message_id != message_id
            {
                self.flush(ctx, events).await?;
                self.buffered_kind = Some(kind.to_owned());
                self.buffered_message_id = message_id.clone();
            }
            self.buffered_bytes = checked_acp_bytes(
                &self.user_session_id,
                self.buffered_bytes,
                update_bytes,
                self.limits.max_completed_message_bytes,
                "limits.acp.maxCompletedMessageBytes",
            )?;
            warn_near_acp_limit(
                &mut self.warned_message_bytes,
                &self.user_session_id,
                "completed message",
                self.buffered_bytes,
                self.limits.max_completed_message_bytes,
                "limits.acp.maxCompletedMessageBytes",
            );
            if kind == "agent_message_chunk" {
                if let Some(content) = update.get("content") {
                    self.agent_content.push(content.clone());
                }
                if self.agent_message_id.is_none() {
                    self.agent_message_id =
                        message_id.and_then(|id| id.as_str().map(str::to_owned));
                }
            }
            self.buffered.push(update.clone());
            let payload = encode_event(AcpEvent::AcpEphemeralSessionUpdateEvent(
                AcpEphemeralSessionUpdateEvent {
                    session_id: self.user_session_id.clone(),
                    after_sequence: u64::try_from(self.latest_sequence).map_err(|_| {
                        SidecarError::InvalidState(String::from("invalid durable sequence"))
                    })?,
                    update: serde_json::to_string(&update)
                        .map_err(|error| SidecarError::InvalidState(error.to_string()))?,
                },
            ))?;
            deliver_event(ctx, events, ctx.ext_event_wire(payload)?)?;
            return Ok(true);
        }

        // Tool, plan, mode, and other durable updates may be interleaved with
        // message deltas. Keep their native order inside the in-progress
        // completion buffer instead of treating them as a message boundary.
        if self.buffered_kind.is_some() {
            self.buffered_bytes = checked_acp_bytes(
                &self.user_session_id,
                self.buffered_bytes,
                update_bytes,
                self.limits.max_completed_message_bytes,
                "limits.acp.maxCompletedMessageBytes",
            )?;
            self.buffered.push(update);
        } else {
            self.persist(ctx, events, vec![update]).await?;
        }
        Ok(true)
    }

    pub(super) async fn flush(
        &mut self,
        ctx: &ExtensionContext<'_>,
        events: &mut Vec<agentos_native_sidecar::wire::EventFrame>,
    ) -> Result<(), SidecarError> {
        if self.buffered.is_empty() {
            self.buffered_kind = None;
            self.buffered_message_id = None;
            self.buffered_bytes = 0;
            self.warned_message_bytes = false;
            return Ok(());
        }
        let updates = coalesce_completed_message(std::mem::take(&mut self.buffered))?;
        self.buffered_kind = None;
        self.buffered_message_id = None;
        self.buffered_bytes = 0;
        self.warned_message_bytes = false;
        self.persist(ctx, events, updates).await
    }

    pub(super) async fn persist(
        &mut self,
        ctx: &ExtensionContext<'_>,
        events: &mut Vec<agentos_native_sidecar::wire::EventFrame>,
        updates: Vec<Value>,
    ) -> Result<(), SidecarError> {
        let stored = self
            .store
            .append_updates(
                &self.user_session_id,
                i64::from(ACP_RESUME_PROTOCOL_VERSION),
                &updates,
            )
            .await
            .map_err(session_store_error)?;
        self.emit_stored(ctx, events, &stored)?;
        if let Some(last) = stored.last() {
            self.latest_sequence = last.sequence;
            self.last_output_sequence = Some(last.sequence);
        }
        Ok(())
    }

    pub(super) fn emit_stored(
        &self,
        ctx: &ExtensionContext<'_>,
        events: &mut Vec<agentos_native_sidecar::wire::EventFrame>,
        stored: &[StoredEvent],
    ) -> Result<(), SidecarError> {
        for event in stored {
            let payload = encode_event(AcpEvent::AcpDurableSessionEvent(AcpDurableSessionEvent {
                session_id: self.user_session_id.clone(),
                sequence: u64::try_from(event.sequence).map_err(|_| {
                    SidecarError::InvalidState(String::from("invalid stored sequence"))
                })?,
                timestamp: timestamp(event.occurred_at_ms).map_err(session_store_error)?,
                event: decode_durable_event(&event.event_json)?,
            }))?;
            deliver_event(ctx, events, ctx.ext_event_wire(payload)?)?;
        }
        Ok(())
    }

    pub(super) fn message_json(&self) -> Result<Option<String>, SidecarError> {
        if self.agent_content.is_empty() {
            return Ok(None);
        }
        serde_json::to_string(&json!({
            "id": self.agent_message_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            "role": "agent",
            "content": self.agent_content,
        }))
        .map(Some)
        .map_err(|error| SidecarError::InvalidState(error.to_string()))
    }
}

pub(super) fn decode_durable_event(event_json: &str) -> Result<AcpDurableEvent, SidecarError> {
    let event: Value = serde_json::from_str(event_json).map_err(|error| {
        SidecarError::InvalidState(format!("invalid durable event JSON: {error}"))
    })?;
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(String::from("durable event is missing type")))?;
    match kind {
        "session_update" => Ok(AcpDurableEvent::AcpDurableSessionUpdate(
            AcpDurableSessionUpdate {
                update: serde_json::to_string(event.get("update").ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "durable session update is missing update",
                    ))
                })?)
                .map_err(|error| SidecarError::InvalidState(error.to_string()))?,
            },
        )),
        "permission_request" => Ok(AcpDurableEvent::AcpDurablePermissionRequest(
            AcpDurablePermissionRequest {
                request_id: event
                    .get("requestId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "durable permission request is missing requestId",
                        ))
                    })?
                    .to_owned(),
                request: serde_json::to_string(event.get("request").ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "durable permission request is missing request",
                    ))
                })?)
                .map_err(|error| SidecarError::InvalidState(error.to_string()))?,
            },
        )),
        "permission_response" => Ok(AcpDurableEvent::AcpDurablePermissionResponse(
            AcpDurablePermissionResponse {
                request_id: event
                    .get("requestId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "durable permission response is missing requestId",
                        ))
                    })?
                    .to_owned(),
                response: serde_json::to_string(event.get("response").ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "durable permission response is missing response",
                    ))
                })?)
                .map_err(|error| SidecarError::InvalidState(error.to_string()))?,
                status: event
                    .get("status")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "durable permission response is missing status",
                        ))
                    })?
                    .to_owned(),
                reason: event
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            },
        )),
        other => Err(SidecarError::InvalidState(format!(
            "unknown durable event type {other}"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_permission_signal(
    ctx: &mut ExtensionContext<'_>,
    process_id: &str,
    request_id: &str,
    key: &str,
    mut receiver: tokio::sync::oneshot::Receiver<PendingPermissionSignal>,
    pending: Arc<StdMutex<BTreeMap<String, PendingPermissionResponse>>>,
    mut cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    events: &mut Vec<agentos_native_sidecar::wire::EventFrame>,
) -> Result<PendingPermissionSignal, SidecarError> {
    let mut inactivity = InactivityWarnings::new(format!(
        "emitted permission request {request_id} to the host"
    ));
    loop {
        if let Some(warning) = inactivity.take_due(Instant::now()) {
            tracing::warn!(
                target: "agentos_sidecar::acp_extension",
                request_id,
                process_id,
                elapsed_ms = warning.elapsed.as_millis() as u64,
                inactive_ms = warning.inactive.as_millis() as u64,
                last_activity_elapsed_ms = warning.last_activity_elapsed.as_millis() as u64,
                last_activity = %warning.last_activity,
                "ACP permission request is still waiting for a human response; no timeout was applied",
            );
        }
        if cancellation
            .as_ref()
            .is_some_and(|receiver| *receiver.borrow())
        {
            pending
                .lock()
                .map_err(|_| {
                    SidecarError::InvalidState(String::from(
                        "permission response registry is poisoned",
                    ))
                })?
                .remove(key);
            return Ok(PendingPermissionSignal::Terminal(String::from(
                "prompt_cancelled",
            )));
        }
        let polled = async { ctx.poll_event_wire(Duration::from_secs(1)).await };
        let signal = if let Some(cancellation) = cancellation.as_deref_mut() {
            tokio::select! {
                biased;
                response = &mut receiver => Some(response.map_err(|_| {
                    SidecarError::InvalidState(format!("permission request {request_id} lost its waiter"))
                })?),
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() {
                        pending.lock().map_err(|_| SidecarError::InvalidState(String::from(
                            "permission response registry is poisoned"
                        )))?.remove(key);
                        Some(PendingPermissionSignal::Terminal(String::from("prompt_cancelled")))
                    } else {
                        return Err(SidecarError::InvalidState(format!(
                            "permission request {request_id} cancellation channel changed without cancellation"
                        )));
                    }
                },
                event = polled => {
                    handle_permission_wait_event(ctx, process_id, key, &pending, events, event?)?
                },
            }
        } else {
            tokio::select! {
                biased;
                response = &mut receiver => Some(response.map_err(|_| {
                    SidecarError::InvalidState(format!("permission request {request_id} lost its waiter"))
                })?),
                event = polled => {
                    handle_permission_wait_event(ctx, process_id, key, &pending, events, event?)?
                },
            }
        };
        if let Some(signal) = signal {
            return Ok(signal);
        }
    }
}

fn handle_permission_wait_event(
    ctx: &ExtensionContext<'_>,
    process_id: &str,
    key: &str,
    pending: &Arc<StdMutex<BTreeMap<String, PendingPermissionResponse>>>,
    events: &mut Vec<agentos_native_sidecar::wire::EventFrame>,
    event: Option<agentos_native_sidecar::wire::EventFrame>,
) -> Result<Option<PendingPermissionSignal>, SidecarError> {
    let Some(event) = event else {
        return Ok(None);
    };
    let adapter_exited = matches!(
        &event.payload,
        EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id
    );
    deliver_event(ctx, events, event)?;
    if adapter_exited {
        pending
            .lock()
            .map_err(|_| {
                SidecarError::InvalidState(String::from("permission response registry is poisoned"))
            })?
            .remove(key);
        Ok(Some(PendingPermissionSignal::Terminal(String::from(
            "adapter_exited",
        ))))
    } else {
        Ok(None)
    }
}

pub(super) async fn write_session_cancel_notification(
    ctx: &mut ExtensionContext<'_>,
    process_id: &str,
    session_id: &str,
) -> Result<(), SidecarError> {
    let mut line =
        serde_json::to_vec(&session_cancel_notification(session_id)).map_err(|error| {
            SidecarError::InvalidState(format!(
                "failed to serialize ACP cancel notification: {error}"
            ))
        })?;
    line.push(b'\n');
    ctx.write_stdin_wire(WriteStdinRequest {
        process_id: process_id.to_string(),
        chunk: line,
    })
    .await?;
    Ok(())
}

pub(super) fn session_cancel_notification(session_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": ACP_CANCEL_METHOD,
        "params": {
            "sessionId": session_id,
        },
    })
}

pub(super) fn cancel_notification_fallback_response(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "cancelled": false,
            "requested": true,
            "via": "notification-fallback",
        },
    })
}

pub(super) fn encode_durable_interrupted_prompt(session_id: &str) -> Option<Vec<u8>> {
    encode_response(AcpResponse::AcpPromptResponse(AcpPromptResponse {
        session_id: session_id.to_owned(),
        message: None,
        stop_reason: String::from("cancelled"),
    }))
    .ok()
}

pub(super) fn encode_durable_cancel_response(signalled: bool) -> Option<Vec<u8>> {
    encode_response(AcpResponse::AcpCancelPromptResponse(
        AcpCancelPromptResponse {
            status: if signalled {
                String::from("cancelled")
            } else {
                String::from("no_active_prompt")
            },
        },
    ))
    .ok()
}

pub(super) fn encode_durable_permission_response(accepted: bool) -> Option<Vec<u8>> {
    encode_response(AcpResponse::AcpRespondPermissionResponse(
        AcpRespondPermissionResponse {
            status: if accepted {
                String::from("accepted")
            } else {
                String::from("not_pending")
            },
            reason: (!accepted).then(|| String::from("already_resolved")),
        },
    ))
    .ok()
}

pub(super) fn synthetic_mode_update(mode_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "update": {
                "sessionUpdate": "current_mode_update",
                "currentModeId": mode_id,
            },
        },
    })
}

pub(super) fn synthetic_config_update(config_options: &[String]) -> Value {
    let config_options = config_options
        .iter()
        .filter_map(|option| serde_json::from_str::<Value>(option).ok())
        .collect::<Vec<_>>();
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "update": {
                "sessionUpdate": "config_option_update",
                "configOptions": config_options,
            },
        },
    })
}

pub(super) fn has_matching_session_update(
    events: &[agentos_native_sidecar::wire::EventFrame],
    session_id: &str,
    predicate: impl Fn(&Map<String, Value>) -> bool,
) -> bool {
    events.iter().any(|event| {
        let EventPayload::ExtEnvelope(envelope) = &event.payload else {
            return false;
        };
        if envelope.namespace != ACP_EXTENSION_NAMESPACE {
            return false;
        }
        let Ok(AcpEvent::AcpSessionEvent(event)) =
            serde_bare::from_slice::<AcpEvent>(&envelope.payload)
        else {
            return false;
        };
        if event.session_id != session_id {
            return false;
        }
        let Ok(notification) = serde_json::from_str::<Value>(&event.notification) else {
            return false;
        };
        if notification.get("method").and_then(Value::as_str) != Some("session/update") {
            return false;
        }
        let Some(params) = notification.get("params").and_then(Value::as_object) else {
            return false;
        };
        let update = params
            .get("update")
            .and_then(Value::as_object)
            .unwrap_or(params);
        predicate(update)
    })
}

pub(super) fn is_cancel_method_not_found(response: &Value) -> bool {
    let Some(error) = response.get("error").and_then(Value::as_object) else {
        return false;
    };
    if error.get("code").and_then(Value::as_i64) != Some(-32601) {
        return false;
    }
    if error
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("method"))
        .and_then(Value::as_str)
        .is_some_and(|method| method == ACP_CANCEL_METHOD)
    {
        return true;
    }
    error
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(|message| message.contains(ACP_CANCEL_METHOD))
}

pub(super) fn permission_option_for_kinds(params: &Value, kinds: &[&str]) -> Option<String> {
    let options = params.get("options")?.as_array()?;
    kinds.iter().find_map(|preferred_kind| {
        options.iter().find_map(|option| {
            (option.get("kind").and_then(Value::as_str) == Some(*preferred_kind))
                .then(|| {
                    option
                        .get("optionId")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .flatten()
        })
    })
}

pub(super) fn automatic_permission_option(
    policy: &str,
    params: &Value,
) -> Result<Option<String>, SidecarError> {
    let (kinds, label): (&[&str], &str) = match policy {
        "reject_all" => (&["reject_once", "reject_always"], "reject"),
        "allow_all" => (&["allow_once", "allow_always"], "allow"),
        "ask" => return Ok(None),
        other => {
            return Err(SidecarError::InvalidState(format!(
                "invalid stored permission policy {other}"
            )))
        }
    };
    permission_option_for_kinds(params, kinds)
        .map(Some)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "permission_policy_unsatisfied: adapter offered no compatible {label} option"
            ))
        })
}

pub(super) fn public_permission_request(
    params: &Value,
    user_session_id: &str,
) -> Result<(String, String), SidecarError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut request = params.clone();
    request
        .as_object_mut()
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from("ACP permission params must be an object"))
        })?
        .insert(
            String::from("sessionId"),
            Value::String(user_session_id.to_owned()),
        );
    let request_json = serde_json::to_string(&request)
        .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
    Ok((request_id, request_json))
}

pub(super) fn permission_option_ids(params: &Value) -> BTreeSet<String> {
    params
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| option.get("optionId").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn parse_content_blocks(
    text: &str,
    session_id: &str,
    limits: &AcpLimits,
) -> Result<Vec<Value>, SidecarError> {
    if text.len() > limits.max_prompt_bytes {
        return Err(SidecarError::InvalidState(format!(
            "acp_prompt_bytes_limit: prompt used {} bytes, limit {}; raise limits.acp.maxPromptBytes",
            text.len(), limits.max_prompt_bytes
        )));
    }
    if text.len() >= limits.max_prompt_bytes.saturating_mul(4) / 5 {
        tracing::warn!(
            session_id,
            used = text.len(),
            limit = limits.max_prompt_bytes,
            config_path = "limits.acp.maxPromptBytes",
            "ACP prompt bytes are near the configured limit"
        );
    }
    let blocks = parse_json_array(text, "content")?;
    if blocks.is_empty() || blocks.len() > limits.max_prompt_blocks {
        return Err(SidecarError::InvalidState(format!(
            "acp_prompt_blocks_limit: prompt contains {} blocks, limit {}; raise limits.acp.maxPromptBlocks",
            blocks.len(), limits.max_prompt_blocks
        )));
    }
    if blocks.len() >= limits.max_prompt_blocks.saturating_mul(4) / 5 {
        tracing::warn!(
            session_id,
            used = blocks.len(),
            limit = limits.max_prompt_blocks,
            config_path = "limits.acp.maxPromptBlocks",
            "ACP prompt block count is near the configured limit"
        );
    }
    serde_json::from_value::<Vec<agent_client_protocol_schema::v1::ContentBlock>>(Value::Array(
        blocks.clone(),
    ))
    .map_err(|error| SidecarError::InvalidState(format!("invalid_content_block: {error}")))?;
    Ok(blocks)
}

pub(super) fn serialized_error_json(code: &str, message: &str) -> String {
    serde_json::to_string(&json!({ "code": code, "message": message })).unwrap_or_else(|_| {
        String::from(
            "{\"code\":\"serialization_failed\",\"message\":\"failed to serialize error\"}",
        )
    })
}

pub(super) async fn finish_prompt_failure(
    store: &SessionStore,
    session_id: &str,
    prompt_id: &str,
    last_output_sequence: Option<i64>,
    code: &str,
    error: SidecarError,
) -> SidecarError {
    let message = error.to_string();
    let serialized = serialized_error_json(code, &message);
    match store
        .finish_prompt(
            session_id,
            prompt_id,
            &[],
            last_output_sequence,
            None,
            Some(&serialized),
        )
        .await
    {
        Ok(_) => error,
        Err(commit_error) => SidecarError::InvalidState(format!(
            "{message}; additionally failed to commit terminal prompt state: {commit_error}"
        )),
    }
}

pub(super) fn prompt_response_from_json(text: &str) -> Result<AcpResponse, SidecarError> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        SidecarError::InvalidState(format!("invalid stored prompt result: {error}"))
    })?;
    Ok(AcpResponse::AcpPromptResponse(AcpPromptResponse {
        session_id: value
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from("stored result missing sessionId"))
            })?
            .to_owned(),
        message: value
            .get("message")
            .filter(|message| !message.is_null())
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?,
        stop_reason: value
            .get("stopReason")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from("stored result missing stopReason"))
            })?
            .to_owned(),
    }))
}

pub(super) fn coalesce_completed_message(updates: Vec<Value>) -> Result<Vec<Value>, SidecarError> {
    let mut output = Vec::new();
    let mut text_update: Option<Value> = None;
    for update in updates {
        let is_text = update
            .get("content")
            .and_then(|content| content.get("type"))
            .and_then(Value::as_str)
            == Some("text");
        if !is_text {
            if let Some(text) = text_update.take() {
                output.push(text);
            }
            output.push(update);
            continue;
        }
        if let Some(existing) = text_update.as_mut() {
            if session_updates_differ_only_by_text(existing, &update) {
                let next = update
                    .get("content")
                    .and_then(|content| content.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let target = existing
                    .get_mut("content")
                    .and_then(Value::as_object_mut)
                    .and_then(|content| content.get_mut("text"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .unwrap_or_default();
                existing["content"]["text"] = Value::String(format!("{target}{next}"));
                continue;
            }
        }
        if let Some(text) = text_update.replace(update) {
            output.push(text);
        }
    }
    if let Some(text) = text_update {
        output.push(text);
    }
    Ok(output)
}

/// ACP allows extension metadata on every update. Text chunks are safe to
/// combine only when every field except `content.text` is identical; otherwise
/// both native updates are retained verbatim in their original order.
pub(super) fn session_updates_differ_only_by_text(left: &Value, right: &Value) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left["content"]["text"] = Value::String(String::new());
    right["content"]["text"] = Value::String(String::new());
    left == right
}

pub(super) fn checked_acp_bytes(
    session_id: &str,
    used: usize,
    requested: usize,
    limit: usize,
    config_path: &str,
) -> Result<usize, SidecarError> {
    let next = used.checked_add(requested).ok_or_else(|| {
        SidecarError::ResourceLimit(LimitError {
            scope: format!("session={session_id}"),
            resource: ResourceClass::BufferedBytes,
            used,
            requested,
            limit,
            config_path: config_path.to_owned(),
        })
    })?;
    if next > limit {
        return Err(SidecarError::ResourceLimit(LimitError {
            scope: format!("session={session_id}"),
            resource: ResourceClass::BufferedBytes,
            used,
            requested,
            limit,
            config_path: config_path.to_owned(),
        }));
    }
    Ok(next)
}

pub(super) fn warn_near_acp_limit(
    warned: &mut bool,
    session_id: &str,
    resource: &str,
    used: usize,
    limit: usize,
    config_path: &str,
) {
    if !*warned && used >= limit.saturating_mul(4) / 5 {
        tracing::warn!(
            session_id,
            resource,
            used,
            limit,
            config_path,
            "ACP session resource usage is near its configured limit"
        );
        *warned = true;
    }
}
