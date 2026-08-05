use super::*;

impl AcpExtension {
    pub(super) async fn start_acp_runtime(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpCreateSessionRequest,
        user_session_id: &str,
        route_key: &str,
        additional_directories: Vec<PathBuf>,
    ) -> AcpHandlerOutput {
        let __t0 = Instant::now();
        // Resolve the agent name -> package entrypoint/env/launchArgs from the
        // projected `/opt/agentos/<name>/current/agentos-package.json`. The client
        // is npm-agnostic and sends only the agent name; the sidecar owns this.
        let resolved = match resolve_agent(ctx, &request.agent_type).await {
            Ok(resolved) => resolved,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let process_id = self.allocate_process_id("acp-agent");
        // Manifest launch args first, then any caller-supplied args.
        let mut args = resolved.launch_args.clone();
        args.extend(request.args.iter().cloned());
        let mut env = hash_to_btree(request.env.clone());
        env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
        // ACP adapters are long-lived stdin servers. Register stdin before guest
        // code starts so a slow module graph cannot be mistaken for quiescence.
        env.insert(
            String::from("AGENTOS_EAGER_STDIN_HANDLE"),
            String::from("1"),
        );
        // Manifest env applies as DEFAULTS; caller/base env wins on conflicts.
        for (key, value) in &resolved.env {
            env.entry(key.clone()).or_insert_with(|| value.clone());
        }
        self.apply_prompt_injection(&request, &mut env);
        tracing::info!(target: "agentos_sidecar::perf", phase = "prompt_injection", elapsed_ms = __t0.elapsed().as_millis() as u64, "start_acp_runtime phase");

        let started = match ctx
            .spawn_process_wire(ExecuteRequest {
                process_id: process_id.clone(),
                command: Some(resolved.entrypoint.clone()),
                runtime: None,
                entrypoint: None,
                args,
                env: env.into_iter().collect(),
                cwd: Some(request.cwd.clone()),
                wasm_permission_tier: None,
                wasm_backend: None,
            })
            .await
        {
            Ok(started) => started,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        tracing::info!(target: "agentos_sidecar::perf", phase = "spawn_process", elapsed_ms = __t0.elapsed().as_millis() as u64, "start_acp_runtime phase");

        let bootstrap = self
            .start_acp_runtime_inner(ctx, &request, &process_id, additional_directories)
            .await;
        tracing::info!(target: "agentos_sidecar::perf", phase = "session_inner_done", elapsed_ms = __t0.elapsed().as_millis() as u64, "start_acp_runtime phase");
        if bootstrap.is_err() {
            kill_process_best_effort(ctx, &process_id).await;
        }
        let bootstrap = match bootstrap {
            Ok(bootstrap) => bootstrap,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };

        let session = LiveAcpRuntime {
            acp_session_id: bootstrap.session_id.clone(),
            user_session_id: Some(user_session_id.to_owned()),
            owner_connection_id: ownership_connection_id(ctx.ownership()),
            agent_type: request.agent_type.clone(),
            process_id: process_id.clone(),
            pid: started.pid,
            modes: bootstrap.modes,
            config_options: bootstrap.config_options,
            agent_capabilities: bootstrap.agent_capabilities,
            agent_info: bootstrap.agent_info,
            stdout_buffer: bootstrap.stdout_buffer,
            next_request_id: 3,
            closed: false,
            pending_preamble: None,
        };

        let mut events = Vec::new();
        for notification in bootstrap.notifications {
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
            response: Ok(AcpResponse::AcpSessionCreatedResponse(
                session.created_response(),
            )),
            events,
        }
    }

    /// Enumerate the agents available in this VM from the ALREADY-PROJECTED
    /// `/opt/agentos` packages. Lists `/opt/agentos`, skips the `bin` symlink farm,
    /// and for each package dir reads `<name>/current/agentos-package.json`; a dir
    /// whose manifest carries a non-empty `agent.acpEntrypoint` is an agent. The
    /// client parses no manifests — the sidecar owns agent enumeration too. Sorted
    /// by id.
    pub(super) async fn list_agents(&self, mut ctx: ExtensionContext<'_>) -> AcpHandlerOutput {
        // The sidecar-owned projected-agent state is the SOURCE OF TRUTH for
        // installed agents (it reflects `ConfigureVm` and live `linkSoftware`
        // updates). Packed `.aospkg` packages ship no `agentos-package.json` in
        // the mount tar — the vbare chunk1 manifest is the only runtime
        // manifest — so agents are enumerated from that decoded state, not by
        // reading manifest JSON out of the guest filesystem.
        let launches = ctx.projected_agents().await.unwrap_or_default();
        let mut agents = launches
            .into_iter()
            .map(|launch| AcpAgentEntry {
                adapter_entrypoint: format!("/opt/agentos/bin/{}", launch.acp_entrypoint),
                id: launch.id,
                installed: true,
            })
            .collect::<Vec<_>>();
        agents.sort_by(|a, b| a.id.cmp(&b.id));
        AcpHandlerOutput::response(Ok(AcpResponse::AcpListAgentsResponse(
            AcpListAgentsResponse { agents },
        )))
    }

    pub(super) async fn start_acp_runtime_inner(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: &AcpCreateSessionRequest,
        process_id: &str,
        additional_directories: Vec<PathBuf>,
    ) -> Result<CreateSessionBootstrap, VmError> {
        let __ti = Instant::now();
        let mut stdout = String::new();
        let mut notifications = Vec::new();
        let client_capabilities =
            parse_json_text(&request.client_capabilities, "clientCapabilities")?;
        let mcp_servers = parse_mcp_servers(&request.mcp_servers)?;

        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": request.protocol_version,
                "clientCapabilities": client_capabilities,
            },
        });
        let initialize_response = send_json_rpc_request(
            ctx,
            process_id,
            &request.agent_type,
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
        validate_initialize_result(&init_result, request.protocol_version)?;
        tracing::info!(target: "agentos_sidecar::perf", phase = "acp_initialize", elapsed_ms = __ti.elapsed().as_millis() as u64, "start_acp_runtime_inner phase");

        let session_new_params = serde_json::to_value(
            NewSessionRequest::new(&request.cwd)
                .additional_directories(additional_directories)
                .mcp_servers(mcp_servers),
        )
        .map_err(|error| {
            VmError::InvalidState(format!("failed to encode ACP session/new: {error}"))
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
            &request.agent_type,
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
        let session_id = session_id_from_session_result(&session_result, process_id);
        tracing::info!(target: "agentos_sidecar::perf", phase = "acp_session_new", elapsed_ms = __ti.elapsed().as_millis() as u64, "start_acp_runtime_inner phase");

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
            config_options.extend(derive_config_options(&session_result));
        }

        Ok(CreateSessionBootstrap {
            session_id,
            modes: json_field(&session_result, &init_result, "modes")?,
            config_options: json_array_to_strings(config_options)?,
            agent_capabilities: json_optional_string(init_result.get("agentCapabilities"))?,
            agent_info: json_optional_string(init_result.get("agentInfo"))?,
            stdout_buffer: stdout,
            notifications,
        })
    }

    pub(super) fn apply_prompt_injection(
        &self,
        request: &AcpCreateSessionRequest,
        env: &mut BTreeMap<String, String>,
    ) {
        let prompt = assemble_system_prompt(
            request.skip_os_instructions,
            request.additional_instructions.as_deref(),
        );
        if prompt.is_empty() {
            env.remove(ACP_APPEND_SYSTEM_PROMPT_ENV);
            return;
        }
        env.insert(String::from(ACP_APPEND_SYSTEM_PROMPT_ENV), prompt);
    }

    pub(super) async fn stop_acp_runtime(
        &self,
        ctx: &mut ExtensionContext<'_>,
        route_key: &str,
    ) -> Result<(), VmError> {
        // Enforce per-connection ownership before tearing anything down: only the
        // connection that created the session may close it. A non-owner (or a
        // missing session) fails closed with the same error, so a cross-connection
        // close neither succeeds nor reveals that another connection's session
        // exists — preventing a cross-tenant DoS. Mirrors the ownership check in
        // the other internal runtime ownership checks.
        let caller_connection_id = ownership_connection_id(ctx.ownership());
        let session = {
            let mut sessions = self.sessions.lock().await;
            let owned_by_caller = sessions
                .get(route_key)
                .is_some_and(|session| session.owner_connection_id == caller_connection_id);
            if owned_by_caller {
                sessions.remove(route_key)
            } else {
                None
            }
        };
        let Some(session) = session else {
            return Err(VmError::InvalidState(format!(
                "unknown ACP session {}",
                route_key
            )));
        };
        if supports_session_close(session.agent_capabilities.as_deref()) {
            let mut stdout = session.stdout_buffer.clone();
            let close = json!({
                "jsonrpc": "2.0",
                "id": session.next_request_id,
                "method": "session/close",
                "params": { "sessionId": session.acp_session_id.clone() },
            });
            match send_json_rpc_request(
                ctx,
                &session.process_id,
                &session.agent_type,
                close,
                session.next_request_id,
                Some(SESSION_CLOSE_TIMEOUT),
                &mut stdout,
                Some(&session.acp_session_id),
                None,
                None,
            )
            .await
            {
                Ok(exchange) if exchange.response.get("error").is_none() => {}
                Ok(exchange) => tracing::warn!(
                    route_key,
                    response = %exchange.response,
                    "ACP adapter rejected advertised session/close; forcing process teardown"
                ),
                Err(error) => tracing::warn!(
                    route_key,
                    error = %error,
                    "ACP session/close failed; forcing process teardown"
                ),
            }
        }
        if let Err(error) = ctx
            .close_stdin_wire(CloseStdinRequest {
                process_id: session.process_id.clone(),
            })
            .await
        {
            tracing::warn!(
                target: "agentos_sidecar::acp_extension",
                route_key,
                process_id = session.process_id,
                error = %error,
                "failed to close ACP adapter stdin before termination"
            );
        }
        // The adapter may already be gone: it can crash, OOM, or idle-evict
        // before the client sends stop_acp_runtime, and its `ProcessExitedEvent`
        // has then already been drained from the shared per-ownership event
        // queue (usually by the prompt exchange loop, which records it as
        // `session.closed`). `wait_for_process_exit` only observes *future*
        // events, so without a short-circuit an already-dead adapter burns
        // `SESSION_CLOSE_TIMEOUT` twice (~10s) signalling a PID that no longer
        // exists — and because extension dispatch is serialized, a
        // `start_acp_runtime` issued right after (session recovery for a
        // returning user) stalls behind the dead wait.
        let adapter_already_gone = session.closed || {
            let sigterm = ctx
                .kill_process_wire(KillProcessRequest {
                    process_id: session.process_id.clone(),
                    signal: String::from("SIGTERM"),
                })
                .await;
            matches!(&sigterm, Err(error) if is_process_already_gone_error(error))
        };
        let terminated = if adapter_already_gone
            || wait_for_process_exit(ctx, &session.process_id, SESSION_CLOSE_TIMEOUT).await
        {
            true
        } else {
            let sigkill = ctx
                .kill_process_wire(KillProcessRequest {
                    process_id: session.process_id.clone(),
                    signal: String::from("SIGKILL"),
                })
                .await;
            if matches!(&sigkill, Err(error) if is_process_already_gone_error(error)) {
                true
            } else {
                sigkill.map_err(|error| {
                    VmError::InvalidState(format!(
                        "ACP adapter {} could not be killed: {error}",
                        session.process_id
                    ))
                })?;
                wait_for_process_exit(ctx, &session.process_id, SESSION_CLOSE_TIMEOUT).await
            }
        };
        if !terminated {
            self.sessions
                .lock()
                .await
                .insert(route_key.to_owned(), session.clone());
            return Err(VmError::InvalidState(format!(
                "ACP adapter {} did not terminate after SIGKILL; the live route was retained",
                session.process_id
            )));
        }
        if let Err(error) = ctx.dispose_session_resources_wire(route_key).await {
            return Err(VmError::InvalidState(format!(
                "ACP adapter terminated but session resource disposal failed for {route_key}; the durable session was retained and can be restored: {error}"
            )));
        }
        tracing::info!(
            target: "agentos_sidecar::acp_extension",
            route_key,
            acp_session_id = session.acp_session_id,
            agent_type = session.agent_type,
            process_id = session.process_id,
            "ACP session closed; adapter process terminated",
        );
        Ok(())
    }

    #[allow(clippy::needless_option_as_deref)]
    pub(super) async fn send_runtime_request_with_sink(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpSessionRequest,
        mut durable_sink: Option<&mut DurableUpdateSink>,
        cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    ) -> AcpHandlerOutput {
        let params = match request
            .params
            .as_deref()
            .map(|params| parse_json_text(params, "session request params"))
            .transpose()
        {
            Ok(Some(params)) => to_record(params),
            Ok(None) => Map::new(),
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let request_params = params.clone();
        let mut outbound_params = params;
        let caller_connection_id = ownership_connection_id(ctx.ownership());
        let (process_id, agent_type, acp_session_id, rpc_id, mut stdout_buffer, pending_preamble) = {
            let mut sessions = self.sessions.lock().await;
            let Some(session) = sessions.get_mut(&request.session_id) else {
                return AcpHandlerOutput::response(Err(VmError::InvalidState(format!(
                    "unknown ACP session {}",
                    request.session_id
                ))));
            };
            // Enforce per-connection ownership: a non-owner must not be able to
            // drive (prompt/cancel/set_mode/etc.) another connection's adapter.
            // Fail closed with the same unknown-session error BEFORE mutating any
            // session state, so the attempt has no side effect on the victim (no
            // request id consumed, no stdout drained) and does not leak the
            // session's existence. Mirrors the other runtime ownership checks.
            if session.owner_connection_id != caller_connection_id {
                return AcpHandlerOutput::response(Err(VmError::InvalidState(format!(
                    "unknown ACP session {}",
                    request.session_id
                ))));
            }
            let rpc_id = session.next_request_id;
            session.next_request_id += 1;
            // Take (and clear) any armed transcript-continuation preamble. It is
            // consumed once, on this session's first `session/prompt` after a
            // fallback resume; non-prompt methods leave it untouched.
            let pending_preamble = if request.method == "session/prompt" {
                session.pending_preamble.take()
            } else {
                None
            };
            (
                session.process_id.clone(),
                session.agent_type.clone(),
                session.acp_session_id.clone(),
                rpc_id,
                std::mem::take(&mut session.stdout_buffer),
                pending_preamble,
            )
        };
        outbound_params.insert(
            String::from("sessionId"),
            Value::String(acp_session_id.clone()),
        );
        if let Some(preamble) = pending_preamble.as_deref() {
            prepend_prompt_preamble(&mut outbound_params, preamble);
        }
        let method = request.method.clone();
        let timeout = request_timeout(&method);
        let outbound = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": method,
            "params": Value::Object(outbound_params),
        });
        let mut exchange = match send_json_rpc_request(
            ctx,
            &process_id,
            &agent_type,
            outbound,
            rpc_id,
            timeout,
            &mut stdout_buffer,
            Some(&acp_session_id),
            durable_sink.as_deref_mut(),
            cancellation,
        )
        .await
        {
            Ok(exchange) => exchange,
            Err(error) => {
                // Adapter process exit is terminal for this live route. Evict
                // it and surface the typed event/error; never respawn the
                // adapter or replay the request implicitly.
                if is_adapter_gone_error(&error) {
                    let exit_code = adapter_exit_code_from_error(&error);
                    let (event_frame, error) = self
                        .handle_adapter_exit(ctx, &request.session_id, exit_code, error)
                        .await;
                    let mut events = Vec::new();
                    if let Some(frame) = event_frame {
                        // Event delivery must not mask the underlying adapter
                        // failure, but it still needs a host-visible diagnostic.
                        if let Err(delivery_error) = deliver_event(ctx, &mut events, frame) {
                            eprintln!(
                                "ERR_AGENTOS_AGENT_EXIT_EVENT: failed to deliver adapter exit event: {delivery_error}"
                            );
                        }
                    }
                    return AcpHandlerOutput {
                        response: Err(error),
                        events,
                    };
                } else if let Some(preamble) = pending_preamble {
                    if let Some(session) = self.sessions.lock().await.get_mut(&request.session_id) {
                        if session.pending_preamble.is_none() {
                            session.pending_preamble = Some(preamble);
                        }
                    }
                }
                return AcpHandlerOutput::response(Err(error));
            }
        };

        let max_stdout_buffer_bytes = match ctx.vm_acp_limits().await {
            Ok(limits) => limits.stdout_buffer_byte_limit,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        if let Some(session) = self.sessions.lock().await.get_mut(&request.session_id) {
            cap_stdout_buffer(&mut stdout_buffer, max_stdout_buffer_bytes);
            session.stdout_buffer = stdout_buffer;
        }

        if request.method == ACP_CANCEL_METHOD && is_cancel_method_not_found(&exchange.response) {
            if let Err(error) =
                write_session_cancel_notification(ctx, &process_id, &acp_session_id).await
            {
                return AcpHandlerOutput::response(Err(error));
            }
            let id = exchange
                .response
                .get("id")
                .cloned()
                .unwrap_or_else(|| Value::Number(rpc_id.into()));
            exchange.response = cancel_notification_fallback_response(id);
        }

        if exchange.response.get("error").is_none() {
            let synthetic = {
                let mut sessions = self.sessions.lock().await;
                match sessions.get_mut(&request.session_id) {
                    Some(session) => session.apply_request_success(
                        &request.method,
                        &request_params,
                        &exchange.events,
                    ),
                    None => Ok(None),
                }
            };
            match synthetic {
                Ok(Some(notification)) => {
                    let handled = if let Some(sink) = durable_sink {
                        match serde_json::from_str::<Value>(&notification) {
                            Ok(notification) => match sink
                                .handle_notification(ctx, &notification, &mut exchange.events)
                                .await
                            {
                                Ok(handled) => handled,
                                Err(error) => return AcpHandlerOutput::response(Err(error)),
                            },
                            Err(error) => {
                                return AcpHandlerOutput::response(Err(VmError::InvalidState(
                                    format!("failed to decode synthetic ACP update: {error}"),
                                )));
                            }
                        }
                    } else {
                        false
                    };
                    if !handled {
                        let event = match encode_event(AcpEvent::AcpSessionEvent(AcpSessionEvent {
                            session_id: request.session_id.clone(),
                            notification,
                        })) {
                            Ok(event) => event,
                            Err(error) => return AcpHandlerOutput::response(Err(error)),
                        };
                        match ctx.ext_event_wire(event) {
                            Ok(frame) => {
                                if let Err(error) = deliver_event(ctx, &mut exchange.events, frame)
                                {
                                    return AcpHandlerOutput::response(Err(error));
                                }
                            }
                            Err(error) => return AcpHandlerOutput::response(Err(error)),
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => return AcpHandlerOutput::response(Err(error)),
            }
        }

        let event_count = match u32::try_from(exchange.events.len()) {
            Ok(event_count) => event_count,
            Err(_) => {
                return AcpHandlerOutput::response(Err(VmError::InvalidState(String::from(
                    "ACP request emitted more events than the protocol can represent",
                ))));
            }
        };
        AcpHandlerOutput {
            response: Ok(AcpResponse::AcpSessionRpcResponse(
                agentos_acp_protocol::generated::v1::AcpSessionRpcResponse {
                    session_id: request.session_id,
                    response: match serde_json::to_string(&exchange.response) {
                        Ok(response) => response,
                        Err(error) => {
                            return AcpHandlerOutput::response(Err(VmError::InvalidState(
                                format!("failed to serialize ACP session response: {error}"),
                            )));
                        }
                    },
                    event_count,
                },
            )),
            events: exchange.events,
        }
    }

    // -----------------------------------------------------------------------
    // Resume state machine (spec §6 / §8) — CANONICAL doc comment.
    //
    // `restore_acp_runtime` re-attaches a SQLite-owned durable session that has no
    // live adapter route (for example after its one-actor/one-VM runtime slept).
    // The public AgentOS ID remains stable and scopes the live route; the native
    // adapter ID is private and may collide with an ID returned by another agent.
    //
    // SQLite remains the authoritative public history. Native ACP recovery is an
    // optimization only: adapters implement load/resume inconsistently and ACP
    // does not provide a portable history-reading contract on which AgentOS can
    // base its API. We therefore never replace or reconstruct SQLite history from
    // adapter-owned files, databases, or load responses.
    //
    //   restore(publicSessionId, storedCreationOptions, storedConfig):
    //     # Launch a fresh adapter and probe its real capabilities via `initialize`
    //     # (capabilities cannot be trusted across a wake; we re-probe here).
    //     caps = initialize(agentType)          # agentCapabilities from the adapter
    //
    //     # Tier 1 — native (capability-gated optimization).
    //     if caps.loadSession || caps.resume:
    //         r = session/load (sessionId)       # or session/resume
    //         ok               -> reapply stored config and bind the private id
    //         UNKNOWN_SESSION  -> fall through    # store didn't survive the wake
    //         other error      -> propagate
    //
    //     # Tier 2 — universal fallback (no adapter code, no capability needed).
    //     live = session/new(stored cwd/dirs/env/mcp/instructions)
    //     arm the newest bounded SQLite continuation on the next prompt
    //     reapply stored configuration and bind the private id
    //
    // The `UNKNOWN_SESSION` discriminator is a JSON-RPC error with
    // `error.data.kind === "unknown_session"`; standard ACP ResourceNotFound
    // (-32002) and known non-standard adapter shapes are normalized to it. Only
    // this condition falls back. Transport/timeout errors propagate.
    pub(super) fn allocate_process_id(&self, prefix: &str) -> String {
        let id = self.next_process_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{prefix}-{id}")
    }

    /// Handle an unexpected adapter exit observed while driving `session_id`.
    /// The live route is evicted before the terminal event is emitted so a
    /// caller retry cannot accidentally target the dead process.
    pub(super) async fn handle_adapter_exit(
        &self,
        ctx: &mut ExtensionContext<'_>,
        session_id: &str,
        exit_code: Option<i32>,
        error: VmError,
    ) -> (Option<agentos_vm::wire::EventFrame>, VmError) {
        let Some(session) = ({
            let mut sessions = self.sessions.lock().await;
            sessions.remove(session_id)
        }) else {
            return (None, error);
        };

        tracing::warn!(
            target: "agentos_sidecar::acp_extension",
            session_id,
            agent_type = session.agent_type,
            process_id = session.process_id,
            exit_code = ?exit_code,
            "ACP adapter process exited unexpectedly; live session route evicted",
        );

        let frame = encode_event(AcpEvent::AcpAgentExitedEvent(AcpAgentExitedEvent {
            session_id: session
                .user_session_id
                .clone()
                .unwrap_or_else(|| session_id.to_string()),
            agent_type: session.agent_type,
            process_id: session.process_id,
            pid: session.pid,
            exit_code,
            restart: ADAPTER_RESTART_OUTCOME_NOT_ATTEMPTED.to_string(),
            restart_count: 0,
            max_restarts: 0,
        }))
        .and_then(|payload| ctx.ext_event_wire(payload));
        let frame = match frame {
            Ok(frame) => Some(frame),
            Err(frame_error) => {
                eprintln!(
                    "ERR_AGENTOS_AGENT_EXIT_EVENT: failed to encode adapter exit event: {frame_error}"
                );
                None
            }
        };

        (
            frame,
            VmError::InvalidState(format!(
                "{error}; ACP adapter exited and the live session route was evicted; restore explicitly before retrying"
            )),
        )
    }

    /// Drop every session owned by `connection_id`, returning the adapter process
    /// ids of the removed records so the caller can reap them. This is the
    /// connection-teardown counterpart to explicit runtime cleanup: when
    /// a connection goes away (client disconnect / shutdown) without a
    /// `stop_acp_runtime` per live session, its records — including the potentially
    /// large `stdout_buffer` — must not outlive the connection.
    ///
    /// Invoked from `on_session_disposed` (the host's per-connection teardown
    /// callback, fired on `DisposeReason::ConnectionClosed`); the other live
    /// teardown paths are `on_dispose` (whole-extension) and the process-exit
    /// eviction in `session_request`. Covered by `connection_teardown_evicts_only_*`
    /// tests.
    pub(super) async fn cleanup_sessions_for_connection(&self, connection_id: &str) -> Vec<String> {
        let mut sessions = self.sessions.lock().await;
        evict_sessions_for_connection(&mut sessions, connection_id)
    }
}
impl LiveAcpRuntime {
    pub(super) fn created_response(&self) -> AcpSessionCreatedResponse {
        AcpSessionCreatedResponse {
            session_id: self.acp_session_id.clone(),
            pid: self.pid,
            modes: self.modes.clone(),
            config_options: self.config_options.clone(),
            agent_capabilities: self.agent_capabilities.clone(),
            agent_info: self.agent_info.clone(),
        }
    }

    pub(super) fn apply_request_success(
        &mut self,
        method: &str,
        params: &Map<String, Value>,
        events: &[agentos_vm::wire::EventFrame],
    ) -> Result<Option<String>, VmError> {
        if method == "session/set_mode" {
            let Some(mode_id) = params.get("modeId").and_then(Value::as_str) else {
                return Ok(None);
            };
            self.apply_local_mode_update(mode_id)?;
            if !has_matching_session_update(events, &self.acp_session_id, |update| {
                update
                    .get("sessionUpdate")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value == "current_mode_update")
                    && update
                        .get("currentModeId")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value == mode_id)
            }) {
                return serde_json::to_string(&synthetic_mode_update(mode_id))
                    .map(Some)
                    .map_err(|error| {
                        VmError::InvalidState(format!(
                            "failed to serialize synthetic mode update: {error}"
                        ))
                    });
            }
        }

        if method == "session/set_config_option" {
            let Some(config_id) = params.get("configId").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(value) = params.get("value") else {
                return Ok(None);
            };
            self.apply_local_config_update(config_id, value)?;
            if !has_matching_session_update(events, &self.acp_session_id, |update| {
                update
                    .get("sessionUpdate")
                    .and_then(Value::as_str)
                    .is_some_and(|value| {
                        value == "config_option_update" || value == "config_options_update"
                    })
            }) {
                return serde_json::to_string(&synthetic_config_update(&self.config_options))
                    .map(Some)
                    .map_err(|error| {
                        VmError::InvalidState(format!(
                            "failed to serialize synthetic config update: {error}"
                        ))
                    });
            }
        }

        Ok(None)
    }

    pub(super) fn apply_local_mode_update(&mut self, mode_id: &str) -> Result<(), VmError> {
        let Some(modes) = self.modes.as_mut() else {
            return Ok(());
        };
        let mut modes_value = parse_json_text(modes, "ACP modes")?;
        if let Value::Object(map) = &mut modes_value {
            map.insert(
                String::from("currentModeId"),
                Value::String(String::from(mode_id)),
            );
            *modes = serde_json::to_string(&modes_value).map_err(|error| {
                VmError::InvalidState(format!("failed to serialize ACP modes: {error}"))
            })?;
        }
        Ok(())
    }

    pub(super) fn apply_local_config_update(
        &mut self,
        config_id: &str,
        value: &Value,
    ) -> Result<(), VmError> {
        let mut updated = false;
        let mut config_options = Vec::with_capacity(self.config_options.len());
        for (index, option) in self.config_options.iter().enumerate() {
            let mut option_value = parse_json_text(option, "ACP config option")?;
            let Value::Object(map) = &mut option_value else {
                return Err(VmError::InvalidState(format!(
                    "ACP config option {index} must be an object"
                )));
            };
            let Some(option_id) = map.get("id").and_then(Value::as_str) else {
                return Err(VmError::InvalidState(format!(
                    "ACP config option {index} missing id"
                )));
            };
            if option_id == config_id {
                map.insert(String::from("currentValue"), value.clone());
                updated = true;
            }
            config_options.push(serde_json::to_string(&option_value).map_err(|error| {
                VmError::InvalidState(format!("failed to serialize ACP config option: {error}"))
            })?);
        }
        if !updated {
            return Err(VmError::InvalidState(format!(
                "unknown ACP config option {config_id}"
            )));
        }
        self.config_options = config_options;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
/// Deliver an ACP event frame to the host. Streams it live through the sidecar's
/// event sink (the stdio path) the instant it is produced; only when no live sink
/// is configured (an in-process `VmManager` with no stdout loop) does it fall
/// back to collecting the frame into `events` for the dispatch-result batch. This
/// is what makes `session/update`s arrive mid-turn instead of all arriving at
/// once when the `session/prompt` dispatch finally resolves.
pub(super) fn deliver_event(
    ctx: &ExtensionContext<'_>,
    events: &mut Vec<agentos_vm::wire::EventFrame>,
    frame: agentos_vm::wire::EventFrame,
) -> Result<(), VmError> {
    if let Some(frame) = ctx.emit_event_wire(frame)? {
        events.push(frame);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_json_rpc_request(
    ctx: &mut ExtensionContext<'_>,
    process_id: &str,
    agent_type: &str,
    request: Value,
    response_id: i64,
    timeout: Option<Duration>,
    stdout: &mut String,
    event_session_id: Option<&str>,
    mut durable_sink: Option<&mut DurableUpdateSink>,
    mut cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Result<JsonRpcExchange, VmError> {
    let max_read_line_bytes = ctx.vm_acp_limits().await?.max_read_line_bytes;
    let mut line = serde_json::to_vec(&request).map_err(|error| {
        VmError::InvalidState(format!("failed to serialize ACP request: {error}"))
    })?;
    line.push(b'\n');
    ctx.write_stdin_wire(WriteStdinRequest {
        process_id: process_id.to_string(),
        chunk: line,
    })
    .await?;

    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut events = Vec::new();
    let mut notifications = Vec::new();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let mut recent_activity = Vec::new();
    let mut adapter_stderr = String::new();
    record_recent_activity(
        &mut recent_activity,
        format!("sent request {method} id={response_id}"),
    );
    let mut inactivity = (method == "session/prompt")
        .then(|| InactivityWarnings::new(format!("sent request {method} id={response_id}")));
    let mut response = None;
    let mut response_drain_deadline = None;
    loop {
        let now = Instant::now();
        if let Some(warning) = inactivity
            .as_mut()
            .and_then(|inactivity| inactivity.take_due(now))
        {
            tracing::warn!(
                target: "agentos_sidecar::acp_extension",
                method,
                response_id,
                process_id,
                elapsed_ms = warning.elapsed.as_millis() as u64,
                inactive_ms = warning.inactive.as_millis() as u64,
                last_activity_elapsed_ms = warning.last_activity_elapsed.as_millis() as u64,
                last_activity = %warning.last_activity,
                "ACP prompt remains pending after sustained inactivity; no timeout was applied",
            );
        }
        if let Some(drain_deadline) = response_drain_deadline {
            if now >= drain_deadline {
                return Ok(JsonRpcExchange {
                    response: response.expect("a response arms the drain deadline"),
                    events,
                    notifications,
                });
            }
        } else if deadline.is_some_and(|deadline| now >= deadline) {
            let timeout = timeout.expect("a deadline always has a timeout");
            let cancel_status = if let Some(session_id) = event_session_id {
                match write_session_cancel_notification(ctx, process_id, session_id).await {
                    Ok(()) => String::from("sent session/cancel notification"),
                    Err(error) => format!("failed to send session/cancel notification: {error}"),
                }
            } else {
                String::from("no session/cancel notification for bootstrap request")
            };
            if event_session_id.is_some() {
                return Ok(JsonRpcExchange {
                    response: timeout_error_response(
                        response_id,
                        &method,
                        timeout,
                        process_id,
                        &cancel_status,
                        recent_activity,
                    ),
                    events,
                    notifications,
                });
            }
            let stderr_tail: String = adapter_stderr
                .chars()
                .rev()
                .take(4000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            return Err(VmError::InvalidState(format!(
                "timed out waiting for ACP response id={response_id}; {cancel_status}; recent_activity={recent_activity:?}; adapter_stderr={stderr_tail:?}"
            )));
        }
        // Prompt turns have no AgentOS-imposed deadline. ACP cancellation is an
        // explicit `session/cancel` operation, and legitimate tool-heavy turns
        // or human permission waits may run for hours. The long poll slice is a
        // transport wakeup interval only; expiration never cancels the turn.
        let mut remaining = response_drain_deadline
            .or(deadline)
            .map(|deadline| deadline.saturating_duration_since(now))
            .unwrap_or(Duration::from_secs(24 * 60 * 60));
        if let Some(inactivity) = inactivity.as_ref() {
            remaining = remaining.min(inactivity.wait_duration(now));
        }
        // `poll_event_wire` already waits on the execution event receiver. Use
        // the real request deadline so output/exit wakes this task directly;
        // a sub-millisecond timeout loop only burns runtime turns while idle.
        let event = if let Some(receiver) = cancellation.as_deref_mut() {
            tokio::select! {
                biased;
                changed = receiver.changed() => {
                    if changed.is_ok() && *receiver.borrow() {
                        if let Some(session_id) = event_session_id {
                            write_session_cancel_notification(ctx, process_id, session_id).await?;
                        }
                    }
                    cancellation = None;
                    continue;
                }
                event = ctx.poll_event_wire(remaining) => event?,
            }
        } else {
            ctx.poll_event_wire(remaining).await?
        };
        let Some(event) = event else {
            if response_drain_deadline.is_some() {
                return Ok(JsonRpcExchange {
                    response: response.expect("a response arms the drain deadline"),
                    events,
                    notifications,
                });
            }
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutputEvent(output)
                if output.process_id == process_id && output.channel == StreamChannel::Stdout =>
            {
                if let Some(inactivity) = inactivity.as_mut() {
                    inactivity.record("received adapter stdout");
                }
                for line in append_stdout_chunk(stdout, &output.chunk, max_read_line_bytes)? {
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        record_recent_activity(
                            &mut recent_activity,
                            String::from("invalid_json_rpc code=-32700 Parse error"),
                        );
                        continue;
                    };
                    if message.get("id").is_some()
                        && message.get("method").and_then(Value::as_str).is_some()
                    {
                        if let Some(inbound_method) = message.get("method").and_then(Value::as_str)
                        {
                            if let Some(inactivity) = inactivity.as_mut() {
                                inactivity.record(format!(
                                    "received request {inbound_method} id={}",
                                    json_rpc_id_label(message.get("id"))
                                ));
                            }
                            record_recent_activity(
                                &mut recent_activity,
                                format!(
                                    "received request {inbound_method} id={}",
                                    json_rpc_id_label(message.get("id"))
                                ),
                            );
                        }
                        if let Some(session_id) = event_session_id {
                            handle_inbound_request(
                                ctx,
                                process_id,
                                session_id,
                                &message,
                                &mut events,
                                durable_sink.as_deref_mut(),
                                cancellation.as_deref_mut(),
                            )
                            .await?;
                        }
                        continue;
                    }
                    if message.get("id").and_then(Value::as_i64) == Some(response_id) {
                        if let Some(inactivity) = inactivity.as_mut() {
                            inactivity.record(format!("received response id={response_id}"));
                        }
                        response = Some(message);
                        if method == "session/prompt" {
                            response_drain_deadline =
                                Some(Instant::now() + PROMPT_RESPONSE_DRAIN_QUIET);
                            continue;
                        }
                        // Still process the rest of this stdout chunk before
                        // returning; a notification can follow the response in
                        // the same process write/read batch.
                        continue;
                    }
                    if message.get("method").and_then(Value::as_str).is_some() {
                        if let Some(notification_method) =
                            message.get("method").and_then(Value::as_str)
                        {
                            if let Some(inactivity) = inactivity.as_mut() {
                                inactivity
                                    .record(format!("received notification {notification_method}"));
                            }
                            record_recent_activity(
                                &mut recent_activity,
                                format!("received notification {notification_method}"),
                            );
                        }
                        if let Some(session_id) = event_session_id {
                            if let Some(sink) = durable_sink.as_deref_mut() {
                                if sink.handle_notification(ctx, &message, &mut events).await? {
                                    continue;
                                }
                            }
                            let frame = ctx.ext_event_wire(encode_event(
                                AcpEvent::AcpSessionEvent(AcpSessionEvent {
                                    session_id: session_id.to_string(),
                                    notification: serde_json::to_string(&message).map_err(
                                        |error| {
                                            VmError::InvalidState(format!(
                                                "failed to serialize ACP notification: {error}"
                                            ))
                                        },
                                    )?,
                                }),
                            )?)?;
                            deliver_event(ctx, &mut events, frame)?;
                        } else {
                            notifications.push(serde_json::to_string(&message).map_err(
                                |error| {
                                    VmError::InvalidState(format!(
                                        "failed to serialize ACP bootstrap notification: {error}"
                                    ))
                                },
                            )?);
                        }
                    }
                }
                if method == "session/prompt" {
                    if response.is_some() {
                        // Require a complete quiet window after the most recent
                        // adapter stdout, not merely after the response line.
                        response_drain_deadline =
                            Some(Instant::now() + PROMPT_RESPONSE_DRAIN_QUIET);
                    }
                } else if let Some(response) = response.take() {
                    return Ok(JsonRpcExchange {
                        response,
                        events,
                        notifications,
                    });
                }
            }
            EventPayload::ProcessOutputEvent(output)
                if output.process_id == process_id && output.channel == StreamChannel::Stderr =>
            {
                if let Some(inactivity) = inactivity.as_mut() {
                    inactivity.record("received adapter stderr");
                }
                // Accumulate stderr (borrow before the chunk is moved into the
                // frame) so a non-zero adapter exit can fold the tail into its
                // error for diagnostics.
                adapter_stderr.push_str(&String::from_utf8_lossy(&output.chunk));
                let frame = ctx.ext_event_wire(encode_event(AcpEvent::AcpAgentStderrEvent(
                    AcpAgentStderrEvent {
                        session_id: event_session_id.unwrap_or_default().to_string(),
                        agent_type: agent_type.to_string(),
                        process_id: process_id.to_string(),
                        chunk: output.chunk,
                    },
                ))?)?;
                // Stream live during an owned session turn (prompt/cancel), but
                // keep bootstrap stderr (initialize/session-new/load, which pass
                // no session id) in the batch so it still arrives for callers that
                // only subscribe after create/resume resolves.
                if event_session_id.is_some() {
                    deliver_event(ctx, &mut events, frame)?;
                } else {
                    events.push(frame);
                }
            }
            EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                // Embed ADAPTER_EXITED_ERROR_MARKER directly so is_adapter_exited_error()
                // stays coupled to this producer: changing the wording can't silently
                // disable session eviction (the H4 leak fix) without touching the const.
                let stderr_tail: String = adapter_stderr
                    .chars()
                    .rev()
                    .take(4000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                tracing::warn!(
                    target: "agentos_sidecar::acp_extension",
                    process_id,
                    agent_type,
                    session_id = ?event_session_id,
                    exit_code = exited.exit_code,
                    stderr_tail = %stderr_tail,
                    "ACP adapter process exited before answering request id={response_id}",
                );
                return Err(VmError::InvalidState(format!(
                    "ACP adapter process {process_id} {ADAPTER_EXITED_ERROR_MARKER} {} before response id={response_id}; recent_activity={:?}; adapter_stderr={:?}",
                    exited.exit_code, recent_activity, stderr_tail
                )));
            }
            EventPayload::ProcessOutputEvent(_)
            | EventPayload::ProcessExitedEvent(_)
            | EventPayload::ExecutionOutputEvent(_)
            | EventPayload::ExecutionCompletedEvent(_)
            | EventPayload::VmLifecycleEvent(_)
            | EventPayload::StructuredEvent(_)
            | EventPayload::ExtEnvelope(_) => {}
        }
    }
}

pub(super) fn record_recent_activity(recent_activity: &mut Vec<String>, entry: String) {
    if recent_activity.len() == 16 {
        recent_activity.remove(0);
    }
    recent_activity.push(entry);
}

pub(super) fn json_rpc_id_label(id: Option<&Value>) -> String {
    match id {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Null) => String::from("null"),
        Some(other) => other.to_string(),
        None => String::from("unknown"),
    }
}

pub(super) fn timeout_error_response(
    response_id: i64,
    method: &str,
    timeout: Duration,
    process_id: &str,
    cancel_status: &str,
    recent_activity: Vec<String>,
) -> Value {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let data = json!({
        "kind": "acp_timeout",
        "method": method,
        "id": response_id,
        "timeoutMs": timeout_ms,
        "transportState": cancel_status,
        "recentActivity": recent_activity,
    });
    json!({
        "jsonrpc": "2.0",
        "id": response_id,
        "error": {
            "code": -32000,
            "message": timeout_error_message(method, response_id, timeout_ms, process_id, cancel_status, &data),
            "data": data,
        },
    })
}

pub(super) fn timeout_error_message(
    method: &str,
    response_id: i64,
    timeout_ms: u64,
    process_id: &str,
    cancel_status: &str,
    data: &Value,
) -> String {
    let activity = data
        .get("recentActivity")
        .and_then(Value::as_array)
        .filter(|activity| !activity.is_empty())
        .map(|activity| {
            activity
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_else(|| String::from("no recent ACP activity"));
    format!(
        "ACP request {method} (id={response_id}) timed out after {timeout_ms}ms. adapter process {process_id}. {cancel_status}. Recent ACP activity: {activity}"
    )
}

pub(super) async fn wait_for_process_exit(
    ctx: &mut ExtensionContext<'_>,
    process_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let Ok(event) = ctx
            .poll_event_wire(deadline.saturating_duration_since(now))
            .await
        else {
            return false;
        };
        let Some(event) = event else {
            continue;
        };
        if let EventPayload::ProcessExitedEvent(exited) = event.payload {
            if exited.process_id == process_id {
                return true;
            }
        }
    }
}

pub(super) async fn handle_inbound_request(
    ctx: &mut ExtensionContext<'_>,
    process_id: &str,
    session_id: &str,
    message: &Value,
    events: &mut Vec<agentos_vm::wire::EventFrame>,
    durable_sink: Option<&mut DurableUpdateSink>,
    cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Result<(), VmError> {
    let id = message
        .get("id")
        .cloned()
        .ok_or_else(|| VmError::InvalidState(String::from("ACP inbound request missing id")))?;
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(());
    };
    let response = match method {
        "session/request_permission" => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if let Some(sink) = durable_sink {
                let result = sink
                    .handle_permission_request(
                        ctx,
                        process_id,
                        session_id,
                        message.get("id"),
                        &params,
                        events,
                        cancellation,
                    )
                    .await?;
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                })
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": "permission_requires_durable_session",
                    },
                })
            }
        }
        _ => forward_inbound_host_request(ctx, session_id, message, &id, method)?,
    };
    let mut line = serde_json::to_vec(&response).map_err(|error| {
        VmError::InvalidState(format!("failed to serialize ACP inbound response: {error}"))
    })?;
    line.push(b'\n');
    ctx.write_stdin_wire(WriteStdinRequest {
        process_id: process_id.to_string(),
        chunk: line,
    })
    .await?;
    Ok(())
}

pub(super) fn forward_inbound_host_request(
    ctx: &ExtensionContext<'_>,
    session_id: &str,
    message: &Value,
    id: &Value,
    method: &str,
) -> Result<Value, VmError> {
    let callback = AcpCallback::AcpHostRequestCallback(AcpHostRequestCallback {
        session_id: session_id.to_string(),
        request: serde_json::to_string(message).map_err(|error| {
            VmError::InvalidState(format!("failed to serialize ACP host request: {error}"))
        })?,
    });
    // This path contains only noninteractive filesystem/terminal/internal host
    // RPCs. Human permission requests are handled durably above and deliberately
    // have no deadline.
    let response = ctx.invoke_callback(
        encode_callback(callback)?,
        ACP_MACHINE_HOST_CALLBACK_TIMEOUT,
    )?;
    let response: AcpCallbackResponse = serde_bare::from_slice(&response).map_err(|error| {
        VmError::InvalidState(format!("invalid ACP host request response: {error}"))
    })?;
    let AcpCallbackResponse::AcpHostRequestCallbackResponse(response) = response;
    let Some(response) = response.response else {
        return Ok(method_not_found_response(id.clone(), method));
    };
    let response = parse_json_text(&response, "ACP host request response")?;
    if response.get("id") != Some(id) {
        return Err(VmError::InvalidState(format!(
            "ACP host request response id {} did not match request id {}",
            json_rpc_id_label(response.get("id")),
            json_rpc_id_label(Some(id))
        )));
    }
    Ok(response)
}

pub(super) fn method_not_found_response(id: Value, method: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("method not found: {method}"),
            "data": { "method": method },
        },
    })
}

pub(super) struct JsonRpcExchange {
    pub(super) response: Value,
    pub(super) events: Vec<agentos_vm::wire::EventFrame>,
    pub(super) notifications: Vec<String>,
}

pub(super) fn response_result(response: Value, label: &str) -> Result<Map<String, Value>, VmError> {
    if let Some(error) = response.get("error").and_then(Value::as_object) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown ACP error");
        // Include `error.data` when present — adapters (e.g. Pi) put the real
        // failure detail there while `message` stays a generic "Internal error".
        let data = error
            .get("data")
            .map(|d| format!(" (data: {d})"))
            .unwrap_or_default();
        return Err(VmError::InvalidState(format!(
            "{label} failed: {message}{data}"
        )));
    }
    response
        .get("result")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| VmError::InvalidState(format!("{label} response missing result")))
}

pub(super) fn validate_initialize_result(
    result: &Map<String, Value>,
    requested_protocol_version: i32,
) -> Result<(), VmError> {
    let reported = result
        .get("protocolVersion")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            VmError::InvalidState(String::from(
                "ACP initialize response missing protocolVersion",
            ))
        })?;
    if reported != i64::from(requested_protocol_version) {
        return Err(VmError::ProtocolVersionMismatch(format!(
            "ACP initialize protocolVersion mismatch: requested {requested_protocol_version}, agent reported {reported}"
        )));
    }
    Ok(())
}

pub(super) fn assemble_system_prompt(skip_base: bool, additional: Option<&str>) -> String {
    let mut parts = Vec::new();
    if !skip_base {
        parts.push(AGENTOS_SYSTEM_PROMPT.trim_end());
    }
    if let Some(additional) = additional {
        if !additional.is_empty() {
            parts.push(additional);
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("{}\n\n---", parts.join("\n\n"))
}

pub(super) fn hash_to_btree(map: HashMap<String, String>) -> BTreeMap<String, String> {
    map.into_iter().collect()
}

/// The agent launch parameters resolved from a projected `/opt/agentos` package
/// manifest. The npm-agnostic client sends only the agent name; the sidecar owns
/// this name -> package -> entrypoint/env/launchArgs resolution.
pub(super) struct ResolvedAgent {
    pub(super) entrypoint: String,
    pub(super) env: BTreeMap<String, String>,
    pub(super) launch_args: Vec<String>,
}

/// The `agent` block of an `agentos-package.json`, parsed from its JSON value: a
/// non-empty `acpEntrypoint` plus optional launch env/args.
pub(super) struct AgentPackageAgentBlock {
    acp_entrypoint: String,
    env: BTreeMap<String, String>,
    launch_args: Vec<String>,
}

/// Look up an agent's launch surface from the sidecar-owned projected-agent
/// state (decoded from the packed vbare manifest at configure/link time; packed
/// packages ship no `agentos-package.json` in the guest filesystem). A package
/// without an agent block yields `None`.
pub(super) async fn read_projected_agent_block(
    ctx: &mut ExtensionContext<'_>,
    agent_type: &str,
) -> Option<AgentPackageAgentBlock> {
    let launches = ctx.projected_agents().await.ok()?;
    let launch = launches
        .into_iter()
        .find(|launch| launch.id == agent_type)?;
    if launch.acp_entrypoint.is_empty() {
        return None;
    }
    Some(AgentPackageAgentBlock {
        acp_entrypoint: launch.acp_entrypoint,
        env: launch.env,
        launch_args: launch.launch_args,
    })
}

/// Resolve an agent name to its launch parameters from the projected manifest. A
/// missing file, a missing `agent` block, or an empty `agent.acpEntrypoint` all map
/// to a single typed "unknown agent" error naming the agent and how to fix it.
pub(super) async fn resolve_agent(
    ctx: &mut ExtensionContext<'_>,
    agent_type: &str,
) -> Result<ResolvedAgent, VmError> {
    match read_projected_agent_block(ctx, agent_type).await {
        Some(agent) => Ok(ResolvedAgent {
            entrypoint: format!("/opt/agentos/bin/{}", agent.acp_entrypoint),
            env: agent.env,
            launch_args: agent.launch_args,
        }),
        None => Err(VmError::InvalidState(format!(
            "unknown agent type \"{agent_type}\": no projected /opt/agentos/pkgs/{agent_type} package \
             with an agent.acpEntrypoint — pass its package to AgentOs software"
        ))),
    }
}

/// Extract the owning connection id from an ownership scope. Every scope carries
/// a connection id, which is the tenant boundary AgentOS enforces; ACP
/// session ownership is keyed off this same connection id.
/// Remove every session in `sessions` owned by `connection_id`, returning the
/// adapter process ids of the dropped records. Split out from
/// [`AcpExtension::cleanup_sessions_for_connection`] as a pure helper so the
/// connection-teardown eviction is unit-testable without locking the mutex.
pub(super) fn evict_sessions_for_connection(
    sessions: &mut BTreeMap<String, LiveAcpRuntime>,
    connection_id: &str,
) -> Vec<String> {
    let owned = sessions
        .iter()
        .filter(|(_, session)| session.owner_connection_id == connection_id)
        .map(|(session_id, _)| session_id.clone())
        .collect::<Vec<_>>();
    owned
        .into_iter()
        .filter_map(|session_id| {
            sessions
                .remove(&session_id)
                .map(|session| session.process_id)
        })
        .collect()
}

/// Trim a retained `stdout_buffer` so it never exceeds the VM's configured ACP
/// read-line limit, keeping the most recent (trailing) bytes
/// — the partial line still being assembled — and truncating at a UTF-8 char
/// boundary so the `String` stays valid.
pub(super) fn cap_stdout_buffer(buffer: &mut String, max_bytes: usize) {
    if buffer.len() <= max_bytes {
        return;
    }
    let mut start = buffer.len() - max_bytes;
    while start < buffer.len() && !buffer.is_char_boundary(start) {
        start += 1;
    }
    *buffer = buffer.split_off(start);
}

/// True when `error` is the `send_json_rpc_request` failure raised because the
/// adapter process exited before answering — the in-crate signal that a session
/// has torn down and its record can be evicted.
pub(super) fn is_adapter_exited_error(error: &VmError) -> bool {
    matches!(error, VmError::InvalidState(message) if message.contains(ADAPTER_EXITED_ERROR_MARKER))
}

/// True when `error` means the adapter process is gone: either the in-pump exit
/// observation (`is_adapter_exited_error`) or an AgentOS process-table
/// lookup failure from operating on an adapter that already exited — the lazy
/// observation of an idle-time crash (`ADAPTER_NO_ACTIVE_PROCESS_MARKER`).
pub(super) fn is_adapter_gone_error(error: &VmError) -> bool {
    if is_adapter_exited_error(error) {
        return true;
    }
    matches!(error, VmError::InvalidState(message) if message.contains(ADAPTER_NO_ACTIVE_PROCESS_MARKER))
}

/// True when a signal/kill request failed because the target process no longer
/// exists: either the adapter-gone classification the prompt path uses
/// (`is_adapter_gone_error`) or the lower-level process-table `ESRCH` /
/// "no such process" error the signal path returns for an already-reaped PID.
/// `stop_acp_runtime` uses this to skip `wait_for_process_exit` — which can only
/// observe a *future* exit event — when the process is already gone.
pub(super) fn is_process_already_gone_error(error: &VmError) -> bool {
    if is_adapter_gone_error(error) {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    message.contains("esrch") || message.contains("no such process")
}

/// Extract the adapter exit code from an `ADAPTER_EXITED_ERROR_MARKER` error
/// message (`"... exited with code <code> before response ..."`). Returns
/// `None` for indirect observations (e.g. a stdin write that failed because
/// the process was already gone), where no exit code was seen.
pub(super) fn adapter_exit_code_from_error(error: &VmError) -> Option<i32> {
    let VmError::InvalidState(message) = error else {
        return None;
    };
    let tail =
        &message[message.find(ADAPTER_EXITED_ERROR_MARKER)? + ADAPTER_EXITED_ERROR_MARKER.len()..];
    tail.split_whitespace().next()?.parse().ok()
}

pub(super) fn parse_json_text(text: &str, label: &str) -> Result<Value, VmError> {
    serde_json::from_str(text)
        .map_err(|error| VmError::InvalidState(format!("invalid {label} JSON: {error}")))
}

pub(super) fn to_record(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => Map::from_iter([(String::from("value"), other)]),
    }
}

pub(super) fn session_id_from_session_result(
    session_result: &Map<String, Value>,
    fallback: &str,
) -> String {
    session_result
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback.to_string())
}

/// Prepend the transcript-continuation preamble as a leading text content block
/// on a `session/prompt`'s `prompt` array. This is the fallback-tier mechanism
/// for handing the agent a pointer to the prior transcript: it rides in-band on
/// the user's first post-resume prompt (a single turn) rather than as a separate
/// RPC, so the agent sees one coherent prompt. A missing/non-array `prompt` is
/// initialized to a single-element array so the preamble is still delivered.
pub(super) fn prepend_prompt_preamble(params: &mut Map<String, Value>, preamble: &str) {
    let block = json!({ "type": "text", "text": preamble });
    match params.get_mut("prompt").and_then(Value::as_array_mut) {
        Some(prompt) => prompt.insert(0, block),
        None => {
            params.insert(String::from("prompt"), Value::Array(vec![block]));
        }
    }
}

pub(super) async fn kill_process_best_effort(ctx: &mut ExtensionContext<'_>, process_id: &str) {
    if let Err(error) = ctx
        .kill_process_wire(KillProcessRequest {
            process_id: process_id.to_owned(),
            signal: String::from("SIGTERM"),
        })
        .await
    {
        eprintln!(
            "ERR_AGENTOS_ADAPTER_CLEANUP: failed to terminate adapter process {process_id}: {error}"
        );
    }
}

/// Return the native restore method from re-probed ACP capabilities. The ACP
/// resume extension is preferred when `sessionCapabilities.resume` is present;
/// stable `loadSession` is the fallback. Legacy top-level `resume: true` remains
/// accepted at this protocol-version boundary for non-standard adapters.
pub(super) fn append_stdout_chunk(
    buffer: &mut String,
    chunk: &[u8],
    max_line_bytes: usize,
) -> Result<Vec<String>, VmError> {
    buffer.push_str(&String::from_utf8_lossy(chunk));

    let mut lines = Vec::new();
    while let Some(index) = buffer.find('\n') {
        if index > max_line_bytes {
            return Err(VmError::InvalidState(format!(
                "ACP adapter emitted a line longer than {max_line_bytes} bytes"
            )));
        }
        let line = buffer[..index].trim().to_owned();
        *buffer = buffer[index + 1..].to_owned();
        if !line.is_empty() {
            lines.push(line);
        }
    }

    if buffer.len() > max_line_bytes {
        return Err(VmError::InvalidState(format!(
            "ACP adapter emitted a line longer than {max_line_bytes} bytes"
        )));
    }

    Ok(lines)
}

pub(crate) fn request_timeout(method: &str) -> Option<Duration> {
    match method {
        "session/prompt" => None,
        "initialize" => Some(INITIALIZE_TIMEOUT),
        "session/new" => Some(SESSION_NEW_TIMEOUT),
        _ => Some(Duration::from_secs(120)),
    }
}

pub(super) fn json_field(
    primary: &Map<String, Value>,
    fallback: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, VmError> {
    match primary.get(key).or_else(|| fallback.get(key)) {
        Some(value) => json_optional_string(Some(value)),
        None => Ok(None),
    }
}

pub(super) fn json_optional_string(value: Option<&Value>) -> Result<Option<String>, VmError> {
    value
        .map(|value| {
            serde_json::to_string(value).map_err(|error| {
                VmError::InvalidState(format!("failed to serialize ACP JSON field: {error}"))
            })
        })
        .transpose()
}

pub(super) fn json_array_to_strings(values: Vec<Value>) -> Result<Vec<String>, VmError> {
    values
        .iter()
        .map(|value| {
            serde_json::to_string(value).map_err(|error| {
                VmError::InvalidState(format!("failed to serialize ACP JSON field: {error}"))
            })
        })
        .collect()
}

pub(super) fn is_model_config_option(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        map.get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == "model")
            || map
                .get("category")
                .and_then(Value::as_str)
                .is_some_and(|category| category == "model")
    })
}

pub(super) fn derive_config_options(session_result: &Map<String, Value>) -> Vec<Value> {
    let Some(models) = session_result.get("models").and_then(Value::as_object) else {
        return Vec::new();
    };
    let current_model_id = models
        .get("currentModelId")
        .and_then(Value::as_str)
        .map(String::from);
    let allowed_values = models
        .get("availableModels")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|model| {
                    let model_id = model.get("modelId")?.as_str()?;
                    let mut item = Map::from_iter([(
                        String::from("id"),
                        Value::String(String::from(model_id)),
                    )]);
                    if let Some(name) = model.get("name").and_then(Value::as_str) {
                        item.insert(String::from("label"), Value::String(String::from(name)));
                    }
                    Some(Value::Object(item))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if current_model_id.is_none() && allowed_values.is_empty() {
        return Vec::new();
    }

    let mut option = Map::from_iter([
        (String::from("id"), Value::String(String::from("model"))),
        (
            String::from("category"),
            Value::String(String::from("model")),
        ),
        (String::from("label"), Value::String(String::from("Model"))),
        (String::from("allowedValues"), Value::Array(allowed_values)),
        (String::from("readOnly"), Value::Bool(false)),
    ]);
    if let Some(current_model_id) = current_model_id {
        option.insert(
            String::from("currentValue"),
            Value::String(current_model_id),
        );
    }
    vec![Value::Object(option)]
}
