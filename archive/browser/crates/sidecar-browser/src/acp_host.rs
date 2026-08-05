//! Browser implementation of `agentos_sidecar_core::AcpHost`.
//!
//! Maps the host-free ACP core's synchronous host operations onto the converged
//! browser executor exposed through `BrowserExtensionContext`: agent launch is the
//! two-step `create_javascript_context` + `start_execution`; stdin/kill/output are
//! keyed by `(vm_id, execution_id)`; `poll_execution_event` returns events for the
//! whole VM so we filter by `execution_id`. `now_ms` is a poll counter (each poll is
//! a real kernel poll over the SAB bridge), so timeouts are interpreted as poll
//! budgets — no wall clock needed.
//!
//! A minimal ACP agent that only uses stdin/stdout emits no `ExecutionEvent::
//! GuestRequest`; servicing those (for agents that make kernel calls) is a
//! follow-up. Cross-execution events are skipped (single-execution-per-session in
//! the minimal path).

use std::collections::BTreeMap;

use agentos_bridge::{
    CreateJavascriptContextRequest, ExecutionEvent, ExecutionHandleRequest, ExecutionSignal,
    KillExecutionRequest, PollExecutionEventRequest, StartExecutionRequest,
    WriteExecutionStdinRequest,
};
use agentos_native_sidecar_browser::{BrowserExtensionContext, BrowserSidecarError};
use agentos_sidecar_core::host::{AcpHost, AgentOutput, SpawnAgentRequest, SpawnedAgent};
use agentos_sidecar_core::AcpCoreError;

/// Per-request adapter: borrows the extension context + the session→execution map.
pub struct BrowserAcpHost<'ctx, 'host> {
    ctx: &'ctx mut BrowserExtensionContext<'host>,
    vm_id: String,
    /// process_id (core's handle) -> execution_id (browser executor handle).
    executions: &'ctx mut BTreeMap<String, String>,
    poll_clock: u64,
}

impl<'ctx, 'host> BrowserAcpHost<'ctx, 'host> {
    pub fn new(
        ctx: &'ctx mut BrowserExtensionContext<'host>,
        vm_id: String,
        executions: &'ctx mut BTreeMap<String, String>,
    ) -> Self {
        Self {
            ctx,
            vm_id,
            executions,
            poll_clock: 0,
        }
    }

    fn execution_id(&self, process_id: &str) -> Result<String, AcpCoreError> {
        self.executions.get(process_id).cloned().ok_or_else(|| {
            AcpCoreError::InvalidState(format!("unknown agent process {process_id}"))
        })
    }
}

fn map_err(error: BrowserSidecarError) -> AcpCoreError {
    AcpCoreError::Execution(error.to_string())
}

impl AcpHost for BrowserAcpHost<'_, '_> {
    fn spawn_agent(&mut self, request: SpawnAgentRequest) -> Result<SpawnedAgent, AcpCoreError> {
        let handle = self
            .ctx
            .create_javascript_context(CreateJavascriptContextRequest {
                vm_id: self.vm_id.clone(),
                bootstrap_module: request.entrypoint.clone(),
            })
            .map_err(map_err)?;
        let mut argv = Vec::new();
        if let Some(entrypoint) = &request.entrypoint {
            argv.push(entrypoint.clone());
        }
        argv.extend(request.args.clone());
        let started = self
            .ctx
            .start_execution(StartExecutionRequest {
                vm_id: self.vm_id.clone(),
                context_id: handle.context_id,
                argv,
                env: request.env.clone(),
                cwd: request.cwd.clone().unwrap_or_default(),
            })
            .map_err(map_err)?;
        self.executions
            .insert(request.process_id.clone(), started.execution_id);
        Ok(SpawnedAgent {
            process_id: request.process_id,
            pid: None,
        })
    }

    fn bind_session(&mut self, _session_id: &str, _process_id: &str) -> Result<(), AcpCoreError> {
        // The browser executor tracks executions by id; no separate bind needed.
        Ok(())
    }

    fn write_stdin(&mut self, process_id: &str, chunk: &[u8]) -> Result<(), AcpCoreError> {
        let execution_id = self.execution_id(process_id)?;
        self.ctx
            .write_stdin(WriteExecutionStdinRequest {
                vm_id: self.vm_id.clone(),
                execution_id,
                chunk: chunk.to_vec(),
            })
            .map_err(map_err)
    }

    fn close_stdin(&mut self, process_id: &str) -> Result<(), AcpCoreError> {
        let execution_id = self.execution_id(process_id)?;
        self.ctx
            .close_stdin(ExecutionHandleRequest {
                vm_id: self.vm_id.clone(),
                execution_id,
            })
            .map_err(map_err)
    }

    fn poll_output(&mut self, process_id: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
        self.poll_clock += 1;
        let execution_id = self.execution_id(process_id)?;
        let event = self
            .ctx
            .poll_execution_event(PollExecutionEventRequest {
                vm_id: self.vm_id.clone(),
            })
            .map_err(map_err)?;
        Ok(match event {
            Some(ExecutionEvent::Stdout(chunk)) if chunk.execution_id == execution_id => {
                Some(AgentOutput::Stdout(chunk.chunk))
            }
            Some(ExecutionEvent::Stderr(chunk)) if chunk.execution_id == execution_id => {
                Some(AgentOutput::Stderr(chunk.chunk))
            }
            Some(ExecutionEvent::Exited(exited)) if exited.execution_id == execution_id => {
                Some(AgentOutput::Exited(Some(exited.exit_code)))
            }
            // Other-execution events, GuestRequest, SignalState: not handled in the
            // minimal path; treat as "nothing for us this poll".
            _ => None,
        })
    }

    fn kill_agent(&mut self, process_id: &str, signal: &str) -> Result<(), AcpCoreError> {
        let execution_id = self.execution_id(process_id)?;
        let signal = match signal {
            "SIGKILL" => ExecutionSignal::Kill,
            "SIGINT" => ExecutionSignal::Interrupt,
            _ => ExecutionSignal::Terminate,
        };
        self.ctx
            .kill_execution(KillExecutionRequest {
                vm_id: self.vm_id.clone(),
                execution_id,
                signal,
            })
            .map_err(map_err)
    }

    fn wait_for_exit(
        &mut self,
        process_id: &str,
        timeout_ms: u64,
    ) -> Result<Option<i32>, AcpCoreError> {
        let deadline = self.poll_clock.saturating_add(timeout_ms);
        while self.poll_clock < deadline {
            if let Some(AgentOutput::Exited(code)) = self.poll_output(process_id)? {
                return Ok(code);
            }
        }
        Ok(None)
    }

    fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), AcpCoreError> {
        self.ctx
            .write_file(&self.vm_id, path, contents.to_vec())
            .map_err(map_err)
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, AcpCoreError> {
        self.ctx.read_file(&self.vm_id, path).map_err(map_err)
    }

    fn now_ms(&self) -> u64 {
        self.poll_clock
    }
}
