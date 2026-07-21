use super::*;

impl AcpExtension {
    pub(super) async fn ensure_durable_runtime(
        &self,
        ctx: &mut ExtensionContext<'_>,
        store: &SessionStore,
        session: StoredSession,
    ) -> Result<StoredSession, SidecarError> {
        let route_key = durable_route_key(ctx.ownership(), &session.session_id);
        if self.sessions.lock().await.contains_key(&route_key) {
            return Ok(session);
        }
        let env = serde_json::from_str::<HashMap<String, String>>(&session.env_json)
            .map_err(|error| SidecarError::InvalidState(format!("invalid stored env: {error}")))?;
        let additional_directories = serde_json::from_str::<Vec<PathBuf>>(
            &session.additional_directories_json,
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid stored additionalDirectories: {error}"))
        })?;
        let mcp_servers = serde_json::from_str::<Vec<McpServer>>(&session.mcp_servers_json)
            .map_err(|error| {
                SidecarError::InvalidState(format!("invalid stored ACP mcpServers: {error}"))
            })?;
        let skip_os_instructions = session.skip_os_instructions;
        let additional_instructions = session.additional_instructions.clone();
        let acp_session_id = session.acp_session_id.clone().ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "session_restore_failed: {} has no private ACP session id",
                session.session_id
            ))
        })?;
        let outcome = self
            .restore_acp_runtime(
                ctx,
                RestoreRuntimeRequest {
                    acp_session_id,
                    agent_type: session.agent.clone(),
                    cwd: session.cwd.clone(),
                    env,
                    additional_directories,
                    mcp_servers,
                    skip_os_instructions,
                    additional_instructions,
                },
                &session.session_id,
                &route_key,
            )
            .await;
        let AcpHandlerOutput {
            response: Ok(AcpResponse::AcpSessionResumedResponse(resumed)),
            events: _replayed_updates,
        } = outcome
        else {
            return match outcome.response {
                Err(error) => Err(error),
                Ok(response) => Err(SidecarError::InvalidState(format!(
                    "invalid restore response: {response:?}"
                ))),
            };
        };
        let finalized = async {
            if resumed.mode == "fallback" {
                let continuation_limit = ctx.vm_acp_limits().await?.max_fallback_continuation_bytes;
                let continuation =
                    build_sqlite_continuation(store, &session, continuation_limit).await?;
                let mut routes = self.sessions.lock().await;
                let runtime = routes.get_mut(&route_key).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "restored ACP route disappeared before continuation was armed",
                    ))
                })?;
                runtime.pending_preamble = Some(continuation);
            }
            self.reapply_stored_config(ctx, &route_key, &session.config_options_json)
                .await?;
            let runtime = self
                .sessions
                .lock()
                .await
                .get(&route_key)
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "restored ACP route disappeared before it could be cached",
                    ))
                })?;
            let config_options = json_strings_to_array_text(&runtime.config_options)?;
            store
                .update_negotiated(
                    &session.session_id,
                    &resumed.session_id,
                    runtime.agent_capabilities.as_deref(),
                    runtime.agent_info.as_deref(),
                    &config_options,
                )
                .await
                .map_err(session_store_error)?;
            required_stored_session(store, &session.session_id).await
        }
        .await;
        if let Err(error) = &finalized {
            if let Err(cleanup_error) = self.stop_acp_runtime(ctx, &route_key).await {
                tracing::error!(
                    session_id = session.session_id,
                    error = %cleanup_error,
                    "failed to clean up partially restored ACP runtime"
                );
            }
            tracing::error!(
                session_id = session.session_id,
                error = %error,
                "ACP runtime restoration did not reach a usable committed state"
            );
        }
        finalized
    }

    pub(super) async fn reapply_stored_config(
        &self,
        ctx: &mut ExtensionContext<'_>,
        route_key: &str,
        config_options_json: &str,
    ) -> Result<(), SidecarError> {
        let options = serde_json::from_str::<Vec<Value>>(config_options_json).map_err(|error| {
            SidecarError::InvalidState(format!("invalid stored ACP config options: {error}"))
        })?;
        for option in options {
            let Some(config_id) = option.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(value) = option.get("currentValue") else {
                continue;
            };
            if !value.is_string() && !value.is_boolean() {
                return Err(SidecarError::InvalidState(format!(
                    "invalid stored ACP config value for {config_id}: expected string or boolean"
                )));
            }
            let mut params = json!({ "configId": config_id, "value": value });
            if value.is_boolean() {
                params["type"] = Value::String(String::from("boolean"));
            }
            let output = self
                .send_runtime_request_with_sink(
                    ctx,
                    AcpSessionRequest {
                        session_id: route_key.to_owned(),
                        method: String::from("session/set_config_option"),
                        params: Some(
                            serde_json::to_string(&params)
                                .map_err(|error| SidecarError::InvalidState(error.to_string()))?,
                        ),
                    },
                    None,
                    None,
                )
                .await;
            let AcpResponse::AcpSessionRpcResponse(response) = output.response? else {
                return Err(SidecarError::InvalidState(String::from(
                    "invalid ACP config replay response",
                )));
            };
            let response: Value = serde_json::from_str(&response.response).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid ACP config replay response JSON: {error}"
                ))
            })?;
            response_result(response, "ACP session/set_config_option during restoration")?;
        }
        Ok(())
    }

    pub(super) async fn restore_acp_runtime(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: RestoreRuntimeRequest,
        user_session_id: &str,
        route_key: &str,
    ) -> AcpHandlerOutput {
        // Resolve the agent name -> package entrypoint/env/launchArgs from the
        // projected manifest, exactly as start_acp_runtime does. The client is
        // npm-agnostic and sends only the agent name.
        let resolved = match resolve_agent(ctx, &request.agent_type).await {
            Ok(resolved) => resolved,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };

        // Reconstruct a create-shaped request so restoration reuses the exact adapter
        // launch + initialize flow. Native session/load does not accept MCP or prompt
        // fields: the adapter restores its session-owned MCP configuration, while the
        // new adapter process receives the stored AgentOS prompt-launch configuration
        // so native load and session/new fallback observe the original instructions.
        let create_like = AcpCreateSessionRequest {
            agent_type: request.agent_type.clone(),
            runtime: AcpRuntimeKind::JavaScript,
            cwd: request.cwd.clone(),
            additional_directories: request
                .additional_directories
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            args: Vec::new(),
            env: request.env.clone(),
            protocol_version: ACP_RESUME_PROTOCOL_VERSION,
            client_capabilities: DEFAULT_RESUME_CLIENT_CAPABILITIES.to_string(),
            mcp_servers: "[]".to_string(),
            skip_os_instructions: request.skip_os_instructions,
            additional_instructions: request.additional_instructions.clone(),
        };

        let process_id = self.allocate_process_id("acp-agent");
        // Manifest launch args first, then any caller-supplied args.
        let mut args = resolved.launch_args.clone();
        args.extend(create_like.args.iter().cloned());
        let mut env = hash_to_btree(create_like.env.clone());
        env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
        env.insert(
            String::from("AGENTOS_EAGER_STDIN_HANDLE"),
            String::from("1"),
        );
        // Manifest env applies as DEFAULTS; caller/base env wins on conflicts.
        for (key, value) in &resolved.env {
            env.entry(key.clone()).or_insert_with(|| value.clone());
        }
        self.apply_prompt_injection(&create_like, &mut env);

        let started = match ctx
            .spawn_process_wire(ExecuteRequest {
                process_id: process_id.clone(),
                command: Some(resolved.entrypoint.clone()),
                runtime: None,
                entrypoint: None,
                args,
                env: env.into_iter().collect(),
                cwd: Some(create_like.cwd.clone()),
                wasm_permission_tier: None,
                wasm_backend: None,
            })
            .await
        {
            Ok(started) => started,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };

        let outcome = self
            .restore_acp_runtime_inner(ctx, &request, &create_like, &process_id)
            .await;
        if outcome.is_err() {
            kill_process_best_effort(ctx, &process_id).await;
        }
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };

        let session = LiveAcpRuntime {
            acp_session_id: outcome.bootstrap.session_id.clone(),
            user_session_id: Some(user_session_id.to_owned()),
            owner_connection_id: ownership_connection_id(ctx.ownership()),
            agent_type: request.agent_type.clone(),
            process_id: process_id.clone(),
            pid: started.pid,
            modes: outcome.bootstrap.modes,
            config_options: outcome.bootstrap.config_options,
            agent_capabilities: outcome.bootstrap.agent_capabilities,
            agent_info: outcome.bootstrap.agent_info,
            stdout_buffer: outcome.bootstrap.stdout_buffer,
            next_request_id: outcome.next_request_id,
            closed: false,
            // Fallback arms the transcript-continuation preamble for the first prompt.
            pending_preamble: outcome.pending_preamble,
        };

        let mut events = Vec::new();
        for notification in outcome.bootstrap.notifications {
            let event = match encode_event(AcpEvent::AcpSessionEvent(AcpSessionEvent {
                session_id: session.acp_session_id.clone(),
                notification,
            })) {
                Ok(event) => event,
                Err(error) => {
                    kill_process_best_effort(ctx, &process_id).await;
                    return AcpHandlerOutput::response(Err(error));
                }
            };
            match ctx.ext_event_wire(event) {
                Ok(event) => events.push(event),
                Err(error) => {
                    kill_process_best_effort(ctx, &process_id).await;
                    return AcpHandlerOutput::response(Err(error));
                }
            }
        }

        if let Err(error) = ctx.bind_process_to_session(route_key, &process_id).await {
            kill_process_best_effort(ctx, &process_id).await;
            return AcpHandlerOutput::response(Err(error));
        }
        self.sessions
            .lock()
            .await
            .insert(route_key.to_owned(), session.clone());

        AcpHandlerOutput {
            response: Ok(AcpResponse::AcpSessionResumedResponse(
                AcpSessionResumedResponse {
                    session_id: session.acp_session_id,
                    mode: outcome.mode,
                },
            )),
            events,
        }
    }

    /// Drive the resume handshake: `initialize`, then native `session/load` (when
    /// the adapter advertises it) or the `session/new` fallback. Returns the
    /// bootstrap state plus the chosen `mode` and any armed preamble.
    pub(super) async fn restore_acp_runtime_inner(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: &RestoreRuntimeRequest,
        create_like: &AcpCreateSessionRequest,
        process_id: &str,
    ) -> Result<ResumeOutcome, SidecarError> {
        let mut stdout = String::new();
        let mut notifications = Vec::new();
        let client_capabilities =
            parse_json_text(&create_like.client_capabilities, "clientCapabilities")?;

        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": create_like.protocol_version,
                "clientCapabilities": client_capabilities,
            },
        });
        let initialize_response = send_json_rpc_request(
            ctx,
            process_id,
            &create_like.agent_type,
            initialize,
            1,
            Some(INITIALIZE_TIMEOUT),
            &mut stdout,
            None,
            None,
            None,
        )
        .await?;
        notifications.extend(initialize_response.notifications);
        let init_result = response_result(initialize_response.response, "ACP initialize")?;
        validate_initialize_result(&init_result, create_like.protocol_version)?;

        let agent_capabilities = init_result.get("agentCapabilities").cloned();

        // Tier 1 — native (capability-gated). Re-probed caps decide eligibility.
        if let Some(native_resume_method) = native_resume_method(agent_capabilities.as_ref()) {
            let load = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": native_resume_method,
                "params": {
                    "sessionId": request.acp_session_id,
                    "cwd": request.cwd,
                    "additionalDirectories": request.additional_directories,
                    "mcpServers": request.mcp_servers,
                },
            });
            let mut load_response = send_json_rpc_request(
                ctx,
                process_id,
                &create_like.agent_type,
                load,
                2,
                Some(SESSION_NEW_TIMEOUT),
                &mut stdout,
                None,
                None,
                None,
            )
            .await?;
            notifications.extend(load_response.notifications);
            trace_acp_response(native_resume_method, &load_response.response);
            normalize_unknown_session_error(&mut load_response.response);

            if load_response.response.get("error").is_none() {
                let load_result = response_result(
                    load_response.response,
                    &format!("ACP {native_resume_method}"),
                )?;
                let bootstrap = build_resume_bootstrap(
                    request.acp_session_id.clone(),
                    &init_result,
                    &load_result,
                    agent_capabilities.as_ref(),
                    stdout,
                    notifications,
                )?;
                return Ok(ResumeOutcome {
                    bootstrap,
                    mode: String::from("native"),
                    next_request_id: 3,
                    pending_preamble: None,
                });
            }

            // Native load failed. Only the `unknown_session` sentinel falls through
            // to the universal fallback; every other error propagates (surfaced
            // verbatim via `response_result`, which returns Err when `error` is set).
            if !is_unknown_session_error(&load_response.response) {
                return Err(response_result(
                    load_response.response,
                    &format!("ACP {native_resume_method}"),
                )
                .expect_err("native resume error object must map to a SidecarError"));
            }
            // fall through to Tier 2
        }

        // Tier 2 — universal fallback. A fresh session, plus the transcript pointer.
        let session_new_params = serde_json::to_value(
            NewSessionRequest::new(&request.cwd)
                .additional_directories(request.additional_directories.clone())
                .mcp_servers(request.mcp_servers.clone()),
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode ACP session/new: {error}"))
        })?;
        let session_new = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": session_new_params,
        });
        let session_response = send_json_rpc_request(
            ctx,
            process_id,
            &create_like.agent_type,
            session_new,
            2,
            Some(SESSION_NEW_TIMEOUT),
            &mut stdout,
            None,
            None,
            None,
        )
        .await?;
        notifications.extend(session_response.notifications);
        let session_result = response_result(session_response.response, "ACP session/new")?;
        let live_session_id = session_id_from_session_result(&session_result, process_id);

        let pending_preamble = None;

        let bootstrap = build_resume_bootstrap(
            live_session_id,
            &init_result,
            &session_result,
            agent_capabilities.as_ref(),
            stdout,
            notifications,
        )?;
        Ok(ResumeOutcome {
            bootstrap,
            mode: String::from("fallback"),
            next_request_id: 3,
            pending_preamble,
        })
    }
}
#[derive(Debug)]
pub(super) struct CreateSessionBootstrap {
    pub(super) session_id: String,
    pub(super) modes: Option<String>,
    pub(super) config_options: Vec<String>,
    pub(super) agent_capabilities: Option<String>,
    pub(super) agent_info: Option<String>,
    pub(super) stdout_buffer: String,
    pub(super) notifications: Vec<String>,
}

/// Result of the resume state machine (`restore_acp_runtime_inner`).
#[derive(Debug)]
pub(super) struct ResumeOutcome {
    bootstrap: CreateSessionBootstrap,
    /// `"native"` (session/load|resume) or `"fallback"` (session/new + preamble).
    mode: String,
    /// First request id available for post-resume RPCs (initialize=1, load/new=2).
    next_request_id: i64,
    /// Transcript-continuation preamble armed for the first prompt (fallback only).
    pending_preamble: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct RestoreRuntimeRequest {
    acp_session_id: String,
    agent_type: String,
    cwd: String,
    env: HashMap<String, String>,
    additional_directories: Vec<PathBuf>,
    mcp_servers: Vec<McpServer>,
    skip_os_instructions: bool,
    additional_instructions: Option<String>,
}

pub(super) async fn build_sqlite_continuation(
    store: &SessionStore,
    session: &StoredSession,
    max_continuation_bytes: usize,
) -> Result<String, SidecarError> {
    let session = store
        .enforce_history_retention(&session.session_id)
        .await
        .map_err(session_store_error)?
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("session_not_found: {}", session.session_id))
        })?;
    let page = store
        .read_history(&session, None, None, 200)
        .await
        .map_err(session_store_error)?;
    let mut transcript = String::from(
        "You are continuing an AgentOS session whose adapter could not restore native context. The authoritative recent ACP session updates follow. Do not repeat actions merely because they appear below.\n\n",
    );
    if transcript.len() > max_continuation_bytes {
        return Err(SidecarError::InvalidState(format!(
            "acp_fallback_continuation_limit: continuation preamble requires at least {} bytes, limit {}; raise limits.acp.maxFallbackContinuationBytes",
            transcript.len(), max_continuation_bytes
        )));
    }
    let mut selected = Vec::new();
    let mut selected_bytes = 0usize;
    for event in page.events.into_iter().rev() {
        let stored: Value = serde_json::from_str(&event.event_json).map_err(|error| {
            SidecarError::InvalidState(format!("invalid durable continuation event: {error}"))
        })?;
        let Some(update) = stored
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| *kind == "session_update")
            .and_then(|_| stored.get("update"))
        else {
            continue;
        };
        let line = format!(
            "{}\n",
            serde_json::to_string(update).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "failed to serialize continuation update: {error}"
                ))
            })?
        );
        if transcript
            .len()
            .saturating_add(selected_bytes)
            .saturating_add(line.len())
            > max_continuation_bytes
        {
            break;
        }
        selected_bytes = selected_bytes.saturating_add(line.len());
        selected.push(line);
    }
    for line in selected.into_iter().rev() {
        transcript.push_str(&line);
    }
    if transcript.len() >= max_continuation_bytes.saturating_mul(4) / 5 {
        tracing::warn!(
            session_id = session.session_id,
            used = transcript.len(),
            limit = max_continuation_bytes,
            config_path = "limits.acp.maxFallbackContinuationBytes",
            "ACP fallback continuation is near the configured limit"
        );
    }
    Ok(transcript)
}

pub(super) fn native_resume_method(agent_capabilities: Option<&Value>) -> Option<&'static str> {
    let caps = agent_capabilities.and_then(Value::as_object)?;
    if caps
        .get("sessionCapabilities")
        .and_then(Value::as_object)
        .and_then(|session| session.get("resume"))
        .is_some_and(Value::is_object)
        || caps.get("resume").and_then(Value::as_bool).unwrap_or(false)
    {
        return Some("session/resume");
    }
    if caps
        .get("loadSession")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("session/load");
    }
    None
}

pub(super) fn supports_session_close(agent_capabilities: Option<&str>) -> bool {
    let capabilities = match agent_capabilities {
        Some(capabilities) => match serde_json::from_str::<Value>(capabilities) {
            Ok(capabilities) => Some(capabilities),
            Err(error) => {
                eprintln!(
                    "ERR_AGENTOS_ACP_CAPABILITIES: invalid cached adapter capabilities while closing: {error}"
                );
                None
            }
        },
        None => None,
    };
    capabilities
        .as_ref()
        .and_then(|capabilities| {
            capabilities
                .get("sessionCapabilities")
                .and_then(Value::as_object)
                .and_then(|session| session.get("close"))
                .cloned()
        })
        .is_some_and(|close| close.is_object() || close.as_bool() == Some(true))
}

pub(super) fn trace_acp_response(method: &str, response: &Value) {
    // Test-only diagnostics for compatibility regressions: the resume
    // test captures the raw native-resume response before normalization so we
    // notice upstream error-shape changes. The env var is sidecar-process
    // trusted input, not guest-controlled runtime surface.
    let Ok(path) = std::env::var(ACP_TRACE_PATH_ENV) else {
        return;
    };
    if path.is_empty() {
        return;
    }
    let payload = json!({
        "method": method,
        "response": response,
    });
    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("ERR_AGENTOS_ACP_TRACE: failed to open {path}: {error}");
            return;
        }
    };
    if let Err(error) = writeln!(file, "{payload}") {
        eprintln!("ERR_AGENTOS_ACP_TRACE: failed to write {path}: {error}");
    }
}

/// Normalize adapter-specific "no such session" errors from `session/load` into
/// the shared `unknown_session` discriminator used by the resume state machine.
///
/// Some adapters report a missing session as JSON-RPC `-32603` with
/// `error.data.details == "NotFoundError"`: the ACP server converts thrown
/// non-`RequestError` exceptions into `internalError({ details: error.message })`,
/// and `Session.get` throws a `NotFoundError` whose message is the class name.
/// Convert exactly that shape into `error.data.kind = "unknown_session"` before
/// fallback matching. Do not broaden this to message substrings or all
/// `-32603`/`-32602` errors; malformed `session/load` must still propagate.
pub(super) fn normalize_unknown_session_error(response: &mut Value) {
    let Some(error) = response.get_mut("error").and_then(Value::as_object_mut) else {
        return;
    };
    let code = error.get("code").and_then(Value::as_i64);
    if code == Some(-32002) {
        let data = error
            .entry(String::from("data"))
            .or_insert_with(|| Value::Object(Map::new()));
        if !data.is_object() {
            *data = Value::Object(Map::new());
        }
        data.as_object_mut().expect("object assigned above").insert(
            String::from("kind"),
            Value::String(String::from("unknown_session")),
        );
        return;
    }
    let Some(data) = error.get_mut("data").and_then(Value::as_object_mut) else {
        return;
    };
    let details = data.get("details").and_then(Value::as_str);
    if code == Some(-32603) && details == Some("NotFoundError") {
        data.insert(
            String::from("kind"),
            Value::String(String::from("unknown_session")),
        );
    }
}

/// Detect a normalized adapter "no such session" error from `session/load` and
/// treat it as the `unknown_session` fallthrough sentinel (the durable store did
/// not survive the VM teardown). Only this triggers the Tier 2 fallback;
/// transport/timeout errors propagate.
///
/// The matcher is intentionally strict: by the time it runs, adapter-specific
/// shapes must already be normalized by [`normalize_unknown_session_error`].
/// This prevents a malformed load request or unrelated internal error from
/// silently resetting the user's context via a fresh fallback session.
pub(super) fn is_unknown_session_error(response: &Value) -> bool {
    response
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("data"))
        .and_then(Value::as_object)
        .and_then(|d| d.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "unknown_session")
}

/// Build the post-resume bootstrap state from `initialize` + `session/load|new`
/// results. Mirrors the config-option / modes / capabilities derivation at the
/// tail of `start_acp_runtime_inner` so a resumed session hydrates identically to a
/// freshly created one.
pub(super) fn build_resume_bootstrap(
    session_id: String,
    init_result: &Map<String, Value>,
    session_result: &Map<String, Value>,
    agent_capabilities: Option<&Value>,
    stdout_buffer: String,
    notifications: Vec<String>,
) -> Result<CreateSessionBootstrap, SidecarError> {
    let mut config_options = init_result
        .get("configOptions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(overrides) = session_result
        .get("configOptions")
        .and_then(Value::as_array)
    {
        config_options = overrides.clone();
    }
    if !config_options.iter().any(is_model_config_option) {
        config_options.extend(derive_config_options(session_result));
    }

    Ok(CreateSessionBootstrap {
        session_id,
        modes: json_field(session_result, init_result, "modes")?,
        config_options: json_array_to_strings(config_options)?,
        agent_capabilities: json_optional_string(agent_capabilities)?,
        agent_info: json_optional_string(init_result.get("agentInfo"))?,
        stdout_buffer,
        notifications,
    })
}
