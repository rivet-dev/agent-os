//! Explicit WASI-threads group for the `wasmtime-threads` backend.
//!
//! Linux/POSIX semantics remain in the kernel and owned libc. This module
//! only owns engine objects: one imported shared memory and one Store/Instance
//! per native guest thread.

use super::super::StartWasmExecutionRequest;
use super::engine::{WasmtimeEngineHandle, WasmtimeEngineProfile};
use super::linker;
use super::store::{self, WasmtimeHostClient};
use crate::backend::HostServiceError;
use agentos_runtime::RuntimeContext;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use wasmtime::{ExternType, Module, SharedMemory};

const MAX_WASI_THREAD_ID: i32 = 0x1fff_ffff;

#[derive(Debug)]
struct ThreadGroupState {
    next_tid: i32,
    active: usize,
    shutting_down: bool,
    first_failure: Option<HostServiceError>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

pub struct ThreadGroup {
    engine: Arc<WasmtimeEngineHandle>,
    module: Arc<Module>,
    runtime: RuntimeContext,
    host: WasmtimeHostClient,
    request: StartWasmExecutionRequest,
    profile: WasmtimeEngineProfile,
    paused: Arc<std::sync::atomic::AtomicBool>,
    pause_notify: Arc<Notify>,
    memory: SharedMemory,
    maximum_threads: usize,
    debug: bool,
    state: Mutex<ThreadGroupState>,
    failure_notify: Notify,
}

impl std::fmt::Debug for ThreadGroup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ThreadGroup")
            .field("process", &self.host.process())
            .field("maximum_threads", &self.maximum_threads)
            .field("memory_pages", &self.memory.size())
            .finish_non_exhaustive()
    }
}

impl ThreadGroup {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        engine: Arc<WasmtimeEngineHandle>,
        module: Arc<Module>,
        runtime: RuntimeContext,
        host: WasmtimeHostClient,
        request: StartWasmExecutionRequest,
        profile: WasmtimeEngineProfile,
        paused: Arc<std::sync::atomic::AtomicBool>,
        pause_notify: Arc<Notify>,
    ) -> Result<Arc<Self>, HostServiceError> {
        let memory_type = module
            .imports()
            .find_map(|import| {
                (import.module() == "env" && import.name() == "memory").then(|| import.ty())
            })
            .and_then(|ty| match ty {
                ExternType::Memory(memory) => Some(memory),
                _ => None,
            })
            .ok_or_else(|| {
                HostServiceError::new(
                    "ERR_AGENTOS_WASM_THREADS_MEMORY_IMPORT",
                    "threaded WebAssembly must import shared memory as env.memory",
                )
            })?;
        if !memory_type.is_shared() {
            return Err(HostServiceError::new(
                "ERR_AGENTOS_WASM_THREADS_MEMORY_NOT_SHARED",
                "threaded WebAssembly env.memory must use the shared-memory type",
            ));
        }
        let maximum_pages = memory_type.maximum().ok_or_else(|| {
            HostServiceError::new(
                "ERR_AGENTOS_WASM_THREADS_MEMORY_UNBOUNDED",
                "threaded WebAssembly shared memory must declare a maximum",
            )
        })?;
        let maximum_bytes = maximum_pages
            .checked_mul(memory_type.page_size())
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                HostServiceError::new(
                    "ERR_AGENTOS_WASM_THREADS_MEMORY_LIMIT",
                    "threaded WebAssembly shared-memory maximum does not fit this platform",
                )
            })?;
        let configured_maximum = super::limits::max_memory_bytes(&request.limits)?;
        if maximum_bytes > configured_maximum {
            return Err(HostServiceError::new(
                "ERR_AGENTOS_WASM_THREADS_MEMORY_LIMIT",
                "threaded WebAssembly shared-memory maximum exceeds the configured limit",
            )
            .with_details(serde_json::json!({
                "limitName": "limits.resources.maxWasmMemoryBytes",
                "limit": configured_maximum,
                "observed": maximum_bytes,
            })));
        }
        let memory = SharedMemory::new(engine.engine(), memory_type).map_err(|error| {
            eprintln!(
                "ERR_AGENTOS_WASM_THREADS_MEMORY_CREATE: private shared-memory diagnostic: {error:#}"
            );
            HostServiceError::new(
                "ERR_AGENTOS_WASM_THREADS_MEMORY_CREATE",
                "failed to allocate the threaded WebAssembly shared memory",
            )
        })?;
        Ok(Arc::new(Self {
            engine,
            module,
            runtime,
            host,
            maximum_threads: request.limits.max_threads.unwrap_or(16).max(1),
            debug: request
                .env
                .get("AGENTOS_WASM_THREAD_DEBUG")
                .is_some_and(|value| value == "1"),
            request,
            profile,
            paused,
            pause_notify,
            memory,
            state: Mutex::new(ThreadGroupState {
                next_tid: 1,
                active: 1,
                shutting_down: false,
                first_failure: None,
                handles: Vec::new(),
            }),
            failure_notify: Notify::new(),
        }))
    }

    pub fn memory(&self) -> &SharedMemory {
        &self.memory
    }

    /// WASI threads returns a positive TID on success and a negative value on
    /// failure. wasi-libc translates every negative result to `EAGAIN`.
    pub fn spawn(self: &Arc<Self>, start_arg: i32) -> i32 {
        let tid = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(_) => {
                    eprintln!(
                        "ERR_AGENTOS_WASM_THREAD_GROUP_POISONED: cannot admit another pthread"
                    );
                    return -1;
                }
            };
            if state.shutting_down || state.active >= self.maximum_threads {
                return -1;
            }
            let tid = state.next_tid;
            if !(1..=MAX_WASI_THREAD_ID).contains(&tid) {
                return -1;
            }
            state.next_tid = tid.saturating_add(1);
            state.active += 1;
            tid
        };

        let group = Arc::clone(self);
        if self.debug {
            eprintln!("AGENTOS_WASM_THREAD_DEBUG spawn tid={tid} arg={start_arg}");
        }
        // AGENTOS_THREAD_SITE: admitted-threaded-wasmtime-guest
        let handle = match std::thread::Builder::new()
            .name(format!("agentos-wasm-pthread-{tid}"))
            .spawn(move || {
                let result = group
                    .runtime
                    .handle()
                    .block_on(group.run_secondary(tid, start_arg));
                group.finish_secondary(result);
            }) {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("ERR_AGENTOS_WASM_THREAD_SPAWN: native worker spawn failed: {error}");
                match self.state.lock() {
                    Ok(mut state) => state.active = state.active.saturating_sub(1),
                    Err(_) => eprintln!(
                        "ERR_AGENTOS_WASM_THREAD_GROUP_POISONED: native spawn rollback failed"
                    ),
                }
                return -1;
            }
        };
        match self.state.lock() {
            Ok(mut state) => state.handles.push(handle),
            Err(_) => {
                // The spawned thread still owns its complete execution state;
                // dropping the handle detaches it, so mark the process group
                // failed and rely on the outer killable worker boundary.
                eprintln!("ERR_AGENTOS_WASM_THREAD_GROUP_POISONED: lost pthread join handle");
            }
        }
        tid
    }

    async fn run_secondary(
        self: &Arc<Self>,
        tid: i32,
        start_arg: i32,
    ) -> Result<(), HostServiceError> {
        if self.debug {
            eprintln!("AGENTOS_WASM_THREAD_DEBUG start tid={tid} arg={start_arg}");
        }
        let thread_id = u32::try_from(tid).map_err(|_| {
            HostServiceError::new(
                "ERR_AGENTOS_WASM_THREAD_ID",
                "WASI thread id does not fit the kernel signal-thread namespace",
            )
        })?;
        self.host
            .submit(
                crate::host::HostOperation::Signal(crate::host::SignalOperation::RegisterThread {
                    thread_id,
                    inherit_from: 0,
                }),
                std::mem::size_of::<u32>() * 2,
            )
            .await?;
        let result = self.run_registered_secondary(tid, start_arg).await;
        let unregister = self
            .host
            .submit(
                crate::host::HostOperation::Signal(
                    crate::host::SignalOperation::UnregisterThread { thread_id },
                ),
                std::mem::size_of::<u32>(),
            )
            .await;
        match (result, unregister) {
            (Err(error), Err(unregister)) => {
                eprintln!(
                    "{}: pthread failed; signal-thread teardown also failed: {}",
                    error.code, unregister
                );
                Err(error)
            }
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(_)) => Ok(()),
        }
    }

    async fn run_registered_secondary(
        self: &Arc<Self>,
        tid: i32,
        start_arg: i32,
    ) -> Result<(), HostServiceError> {
        let mut store = store::create_store(
            Arc::clone(&self.engine),
            &self.runtime,
            self.host.clone(),
            &self.request,
            self.profile,
            store::thread_cpu_time_ns(),
            Arc::clone(&self.paused),
            Arc::clone(&self.pause_notify),
            true,
            Some(Arc::clone(self)),
            tid,
        )?;
        let mut linker =
            linker::build_linker(self.engine.engine(), self.request.permission_tier, true)
                .map_err(|error| {
                    eprintln!(
                        "ERR_AGENTOS_WASM_THREAD_LINKER: private linker diagnostic: {error:#}"
                    );
                    HostServiceError::new(
                        "ERR_AGENTOS_WASM_THREAD_LINKER",
                        "failed to construct the threaded WebAssembly linker",
                    )
                })?;
        linker
            .define(&store, "env", "memory", self.memory.clone())
            .map_err(|error| {
                eprintln!(
                    "ERR_AGENTOS_WASM_THREAD_MEMORY_LINK: private linker diagnostic: {error:#}"
                );
                HostServiceError::new(
                    "ERR_AGENTOS_WASM_THREAD_MEMORY_LINK",
                    "failed to link the threaded WebAssembly shared memory",
                )
            })?;
        let instance = linker
            .instantiate_async(&mut store, &self.module)
            .await
            .map_err(|error| {
                super::error::normalize("ERR_AGENTOS_WASM_THREAD_INSTANTIATE", &error, false)
            })?;
        linker::initialize_inherited_signal_mask(&mut store, &instance).await?;
        let start = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "wasi_thread_start")
            .map_err(|error| {
                eprintln!(
                    "ERR_AGENTOS_WASM_THREAD_ENTRYPOINT: private entrypoint diagnostic: {error:#}"
                );
                HostServiceError::new(
                    "ERR_AGENTOS_WASM_THREAD_ENTRYPOINT",
                    "threaded WebAssembly does not export a valid wasi_thread_start function",
                )
            })?;
        match start.call_async(&mut store, (tid, start_arg)).await {
            Ok(()) => Ok(()),
            Err(_) if store.data().exit_code.is_some() => Ok(()),
            Err(error) => Err(super::error::normalize(
                "ERR_AGENTOS_WASM_THREAD_TRAP",
                &error,
                store.data().canceled(),
            )),
        }
    }

    fn finish_secondary(&self, result: Result<(), HostServiceError>) {
        if self.debug {
            eprintln!(
                "AGENTOS_WASM_THREAD_DEBUG finish result={}",
                if result.is_ok() { "ok" } else { "error" }
            );
        }
        let failure = result.err();
        let Ok(mut state) = self.state.lock() else {
            eprintln!("ERR_AGENTOS_WASM_THREAD_GROUP_POISONED: pthread completion was lost");
            return;
        };
        state.active = state.active.saturating_sub(1);
        if let Some(error) = failure.as_ref() {
            eprintln!(
                "{}: pthread execution failed: {}",
                error.code, error.message
            );
            state.first_failure.get_or_insert_with(|| error.clone());
            self.failure_notify.notify_one();
        }
        drop(state);
        if let Some(error) = failure {
            self.host.report_thread_group_failure(error);
        }
    }

    /// Resolve as soon as any secondary Store traps. The worker's main Store
    /// races this against `_start`, so one bad pthread terminates the complete
    /// process group instead of leaving another pthread parked indefinitely.
    pub async fn wait_for_failure(&self) -> HostServiceError {
        loop {
            let notified = self.failure_notify.notified();
            match self.state.lock() {
                Ok(state) => {
                    if let Some(error) = state.first_failure.as_ref() {
                        return error.clone();
                    }
                }
                Err(_) => return group_poisoned(),
            }
            notified.await;
        }
    }

    /// Mark the group closed when the process main Store exits. Linux process
    /// exit does not join detached pthreads: the enclosing worker process is
    /// the teardown unit and its exit terminates every remaining native guest
    /// thread. Finished JoinHandles are reaped here; unfinished handles are
    /// deliberately detached immediately before the worker itself exits.
    pub fn settle_main(&self) -> Result<(), HostServiceError> {
        let mut state = self.state.lock().map_err(|_| group_poisoned())?;
        state.shutting_down = true;
        let handles = std::mem::take(&mut state.handles);
        let failure = state.first_failure.take();
        drop(state);
        for handle in handles {
            if handle.is_finished() && handle.join().is_err() {
                return Err(HostServiceError::new(
                    "ERR_AGENTOS_WASM_THREAD_PANIC",
                    "a threaded WebAssembly native worker panicked",
                ));
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

fn group_poisoned() -> HostServiceError {
    HostServiceError::new(
        "ERR_AGENTOS_WASM_THREAD_GROUP_POISONED",
        "threaded WebAssembly group state is poisoned",
    )
}
