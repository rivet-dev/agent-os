# Required codex SOURCE patches for wasm32-wasip1

These are minimal, upstreamable additions to codex's own source where it has a
hard `compile_error!`/missing-platform arm that NO toolchain patch can bypass
(first-party compile-time gates). Each adds REAL wasi support (wasi has the API),
not a hack. Applied to the vendored codex source during the agentos build.

1. utils/git/src/platform.rs — add `#[cfg(target_os = "wasi")] create_symlink`
   using `std::os::wasi::fs::symlink_path`, and widen the fallback
   `compile_error!` cfg to `not(any(unix, windows, target_os = "wasi"))`.
   wasi supports symlinks via the VM's `path_symlink` host call.

2. rmcp-client/src/program_resolver.rs:26 — widen `#[cfg(unix)]` on `resolve()` to
   `any(unix, target_os = "wasi")` (the unix impl returns the program unchanged,
   correct for wasi).
3. rmcp-client/src/utils.rs:91 — widen `#[cfg(unix)]` on `DEFAULT_ENV_VARS` to
   `any(unix, target_os = "wasi")`.

4. core/Cargo.toml — add `[target.'cfg(target_os = "wasi")'.dependencies] codex-utils-pty`
   (it compiles via the portable-pty wasi backend). codex-network-proxy stays excluded
   (real crate pulls the rama proxy-server stack + tokio "full"); satisfied by stub.

## codex-core OWN-code remaining (69 errors, characterized — this session got dep graph to 0)
The codex authors gated codex-network-proxy + codex-utils-pty OFF wasi as DEPS but did
NOT gate the SOURCE usage — an incomplete author wasi port. Breakdown:
- ~50: codex-core uses codex-network-proxy's FULL API (NetworkProxyConfig, NetworkDecision,
  NetworkPolicyDecider/BlockedRequestObserver traits, NetworkProxyHandle, ConfigReloader,
  build_config_state, host_and_port_from_network_addr, normalize_host,
  validate_policy_against_constraints, NetworkProxyConstraints/Error, NetworkProtocol,
  NetworkProxyState, NetworkProxyAuditMetadata, NetworkPolicyRequest, …). The real crate
  can't compile on wasi (rama proxy stack); EXPAND the agentos codex-network-proxy
  stub to this full API (no-op/inert — the VM kernel brokers network policy host-side).
- ~6: codex_otel SessionTelemetry methods the stub lacks → expand codex-otel stub.
- tokio::signal (no signals on wasi) → gate/stub codex-core's signal usage.
- ~4 platform fns (ensure_owner_only_permissions, synthetic_exit_status,
  system_config_toml_file, system_requirements_toml_file) → add wasi arms.
- ~5 type mismatches (E0308) → case-by-case.
Then codex-exec compiles → un-stub --session-turn (real codex-core + EE protocol) → matrix.

## network-proxy / otel: MUST be in-place wasi-gating of the REAL codex crates
Attempted an external stub crate (stubs/codex-network-proxy-wasi) — does NOT work:
codex workspace crates (codex-utils-absolute-path, etc.) use `workspace = true`, which
can't resolve from a path crate OUTSIDE codex's workspace. So the network-proxy/otel
stubbing must be done IN-PLACE in codex-rs/{network-proxy,otel} (workspace members):
- network-proxy/Cargo.toml: move `rama-*` + `tokio "full"` + rustls-provider to
  `[target.'cfg(not(target_os="wasi"))'.dependencies]`; reduce wasi tokio features.
- network-proxy/src: gate the 6 rama files (certs,http_proxy,mitm,responses,socks5,
  upstream) to `#[cfg(not(wasi))]` and add `#[cfg(wasi)]` stub versions. The 6 stub
  bodies are already written in stubs/codex-network-proxy-wasi/src/{http_proxy,socks5,
  mitm,certs,responses,upstream}.rs (run_http_proxy/run_socks5 → Ok(()); MitmState → unit).
  Only 5 symbols are referenced by pure code: http_proxy::run_http_proxy{,_with_std_listener},
  socks5::run_socks5{,_with_std_listener}, mitm::MitmState. lib.rs mod decls stay (pure
  modules config/network_policy/policy/proxy/reasons/runtime/state compile as-is).
- core/Cargo.toml: add codex-network-proxy to `[target.'cfg(target_os="wasi")'.dependencies]`.
- otel: same pattern — gate opentelemetry-otlp (grpc/tonic/reqwest exporter) off wasi,
  stub the exporter init; keep the SessionTelemetry API (codex-core uses ~6 methods).
This is completing the codex authors' OWN incomplete wasi port (they gated the DEPS off
wasi but not the source). It is codex SOURCE modification, unavoidable (workspace constraint).

## DONE this iteration: network-proxy in-place wasi-gating WORKS (codex-core 69→49)
Applied to REAL codex-rs/network-proxy (workspace member, so workspace=true resolves):
- network-proxy/Cargo.toml: rama-* + rustls-provider + tokio "full" → moved to
  [target.'cfg(not(target_os="wasi"))'.dependencies]; base tokio reduced to
  ["macros","rt","sync","time","io-util"].
- network-proxy/src/lib.rs: the 6 rama modules (certs/http_proxy/mitm/responses/
  socks5/upstream) cfg-selected: #[cfg(not(wasi))] real vs #[cfg(wasi)] #[path="*_wasi.rs"].
- network-proxy/src/{certs,http_proxy,mitm,responses,socks5,upstream}_wasi.rs: stub bodies
  (run_http_proxy/run_socks5 → Ok(()); MitmState → unit). VERIFIED: codex_network_proxy
  refs now resolve (0 unresolved). Restored workspace redirect to path="network-proxy"
  + re-added as member + codex-core wasi dep.

## NEXT: otel in-place gating (same pattern; ~6 SessionTelemetry method errors remain)
otel's opentelemetry-otlp exporter is spread across provider.rs(63)/client.rs(40)/
otlp.rs(11)/config.rs(24). Gate those off wasi (cfg-select stub modules), keep
events/session_telemetry.rs (the SessionTelemetry API codex-core calls:
record_api_request, record_websocket_request, record_auth_recovery, log_sse_event,
tool_decision, tool_result_with_tags, on_request — these use codex-protocol types, so
MUST stay in-crate, not an external stub). Then: tokio::signal gate, ~4 platform fns
(synthetic_exit_status, system_*_toml_file, ensure_owner_only_permissions), ~5 E0308.
Then codex-exec → session-turn engine → matrix.

## ✅ MILESTONE: codex-core AND codex-exec COMPILE for wasm32-wasip1
All in-crate wasi-gating done (committed to the rivet codex fork, branch
wasi-port-codex-core, commit 0c3f4eb73, 25 files): network-proxy + otel in-place
gating, core platform fns (tokio::signal/synthetic_exit_status/toml/permissions),
git symlink, rmcp-client resolver. Plus std ExitStatusExt patch (std-patches/
0008-wasi-exit-status-ext.md). `cargo build -p codex-core -p codex-exec
--target wasm32-wasip1 -Z build-std` → Finished, EXIT 0.

REMAINING to e2e: (1) the agentos command crate crates/commands/codex-exec
un-stub --session-turn to delegate to the real codex-core/codex-exec engine and emit
the EE newline-JSON protocol (start/text_delta/tool_call_update/permission_request/
done) — the real functional integration; (2) build via `make -C toolchain wasm`
(vendoring the codex fork + these patches); (3) wire the EE codex descriptor into the
agent-os matrix + un-skip the codex session test.

## FINAL PIECE: session-turn engine (replace codex-exec wasi_stub_main)
codex-exec/src/lib.rs `wasi_stub_main()` is a placeholder ("WASI runtime support is
under development"). codex-core compiles now, so replace it with the real engine
(mirror lib_native.rs::run_exec_session, ~200-300 lines, in codex's workspace so it has
codex-core access). The built codex-exec.wasm IS the agentos `codex-exec` command.

PROTOCOL (newline-JSON, the EE adapter @rivet-ee/agent-os-codex-agent drives it):
- stdin: first line {type:"start", cwd, mode, model, thought_level,
  developer_instructions, history, prompt}; then {type:"permission_response", id, decision}.
- stdout events to EMIT (one JSON/line):
  - {type:"start"} on boot
  - {type:"text_delta", delta}          ← EventMsg::AgentMessageDelta
  - {type:"tool_call_update", tool_call_id, status, content}
                                         ← EventMsg::ExecCommandBegin / ExecCommandEnd
  - {type:"permission_request", tool_call_id, command}
                                         ← EventMsg::ExecApprovalRequest /
                                            ApplyPatchApprovalRequest; then read
                                            permission_response from stdin → submit Op approval
  - {type:"done"}                        ← EventMsg::TurnComplete
  - {type:"error", message}             ← EventMsg::Error

ENGINE (codex-core API, verified):
1. parse start JSON → build codex_core Config (cwd/model/instructions; sandbox+network
   proxy already gated off on wasi; approval policy = OnRequest so approvals surface).
2. ConversationManager::new_conversation(config) → Codex (or new_conversation_with_auth).
3. tokio current_thread runtime (rt feature works on wasi). seed history, then
   submit(Op::UserInput{ items:[InputItem::Text{prompt}] }).
4. loop next_event() → match EventMsg → emit per table above; on approval requests,
   block reading stdin for permission_response → submit(Op::ExecApproval/PatchApproval).
5. TurnComplete → emit done, exit 0.
Then: `make -C toolchain wasm` (vendor the codex fork wasi-port-codex-core branch +
all patches) → copies codex/codex-exec into software/codex/wasm → wire EE descriptor into
the agent-os matrix (software/codex already exists) → un-skip the codex session test.

### session-turn engine — Config is loadable (tractable)
core/src/config/mod.rs has `Config::load_with_cli_overrides(cli_overrides, harness)`
(811) — build Config from {model, cwd, approval_policy=OnRequest, ...} overrides rather
than manual field construction. Op/InputItem: protocol.rs Op::UserInput{ items:
Vec<UserInput> } (235). The Codex client (core/src/codex.rs submit/next_event) is created
via the conversation/thread entry (codex_thread.rs / conversation flow) — confirm the
exact constructor (ConversationManager or Codex::spawn) when implementing. Engine ≈
200-300 lines in codex-exec wasi_stub_main; tokio current_thread runtime (works on wasi).

## ✅ DONE: session-turn engine compiles (the final FUNCTIONAL piece)
codex-exec/src/session_turn_wasi.rs (fork commit b86a5dfc0) drives the REAL codex-core
agent and emits the EE protocol. `cargo build -p codex-exec --target wasm32-wasip1
-Z build-std` → EXIT 0. So codex-core + codex-exec + the session-turn engine ALL compile.

## Remaining = build pipeline + ACP wiring + test run (integration, not functional)
1. Produce optimized wasm artifacts from the fork (branch wasi-port-codex-core):
   `cargo build -p codex-exec -p codex --release --target wasm32-wasip1 -Z build-std`
   (+ wasm-opt) → copy to software/codex/wasm/{codex-exec,codex}. NOTE: the
   agentos `make wasm` currently builds the STUB crates/commands/{codex,codex-exec};
   to ship the real engine, either build from the fork directly (above) or vendor the
   fork into toolchain and point cmd-codex-exec at the real codex-exec.
2. ACP adapter: the codex ACP bridge (spawns `codex-exec --session-turn`, newline-JSON↔ACP)
   lives in EE (@rivet-ee/agent-os-codex-agent, agent-os-ee/packages/codex/src/adapter.ts).
   The open-source software/codex only re-exports the wasm software package. To test
   codex e2e in the agent-os matrix, either bring that adapter into agent-os or run the
   EE codex-session.test.ts (describe.skipIf gated on wasm presence — un-skips once the
   wasm artifacts above exist).
3. e2e: with codex-exec.wasm present, the EE adapter spawns it in the VM (wasi-spawn/http
   host bridges), the engine talks to the (mock) model via wasi-http, streams text_delta/
   tool_call_update, emits done. Run codex-session.test.ts to verify.

## ✅ ARTIFACT BUILT + SPAWNS IN VM; last blocker = 1 host import
Built the real codex-exec from the fork (release, wasm-opt'd to 28MB) and placed it in
software/codex/wasm/{codex-exec,codex}. Smoke test (vm.exec("... | codex-exec
--session-turn")) shows it SPAWNS and attempts WebAssembly instantiation in the VM —
44 imports, of which 43 (wasi_snapshot_preview1×30, host_net×6, host_process×5,
host_fs×2) are provided by the agentos runtime. The ONLY unprovided one was
`env.sqlite3_load_extension` (sqlx's sqlite). Fixed by defining a #[no_mangle] no-op
stub in the engine (fork commit). Rebuild + re-place → instantiation should succeed.
Then the smoke test ({"type":"start"} before the model call) and the full EE
codex-session.test.ts (with the responses mock) verify the e2e turn.

## ✅✅ VERIFIED: codex-exec RUNS IN THE VM and emits the EE protocol
Smoke test (packages/core/tests/codex-smoke.test.ts) PASSES:
  vm.exec("printf '<start json>' | codex-exec --session-turn")  →  stdout: {"type":"start"}
The real codex agent (codex-rs → wasip1, session-turn engine driving codex-core) BOOTS
in the agentos VM, instantiates all 44 imports (after adding host
path_filestat_set_times), runs the engine, and emits the EE protocol's start event.
(exit 1 only because the smoke test provides no model mock/auth, so the turn errors
AFTER start — proving the boot + protocol path works.)

FULL turn (text_delta→done): run the EE codex-session.test.ts with startResponsesMock
(OpenAI Responses mock) + OPENAI_API_KEY/base_url pointed at it. That exercises a
complete agent turn. The boot+protocol path is now VERIFIED end-to-end in the VM.

## ✅✅✅ codex runs the FULL session event loop in the VM
DBG markers (engine) confirm the real codex agent runs the entire lifecycle in the VM:
load config → AuthManager::shared → ThreadManager::new → start_thread → submit
Op::UserInput → LIVE next_event loop (received a Warning event, processing). 8 runtime
blockers fixed and committed (host imports, agentos host wasi shim
path_filestat_set_times, tokio fs-asyncify + spawn_blocking inline on wasi, std
split_paths, now_local→UTC). FINAL GAP: the model HTTP POST to the OpenAI Responses
mock does not reach it (mock requests=0, then hang). codex honors OPENAI_BASE_URL
(deprecated warning), so base_url=mock; the codex-exec wasi-http/host_net connect to the
HOST loopback mock blocks. Resolve: confirm host_net egress to the test's 127.0.0.1:PORT
is permitted for wasi commands (loopbackExemptPorts is for the kernel adapter; host_net
may need its own allow), and that the Responses path matches (/v1/responses). Then the
turn streams text_delta→done. The agent itself runs e2e in the VM today.

## CORRECTED final diagnosis (via sidecar instrumentation): NOT a network issue
Added file-logging to the sidecar net.connect handler ENTRY + require_network_access +
resolve_tcp_connect_addr, ran the full-turn test. RESULT: the log stays EMPTY — codex's
connect NEVER reaches the sidecar. So codex is NOT making the model HTTP call; it HANGS
in its OWN agent loop after the first event (a config Warning), before any network call.
Root cause: codex's agent loop uses tokio concurrency that assumes OS threads
(spawn_blocking on a dedicated blocking pool, background tasks). wasm32-wasip1 is
single-threaded (no OS threads); the spawn_blocking-as-current-thread-task workaround
(0003) can DEADLOCK — a blocking task occupies the only thread while waiting on another
task that can't run. This is an architectural mismatch (codex's threading model vs
single-thread wasi), the genuinely hard core of running codex on wasi. Resolving it needs
codex-internal tracing to find the exact blocking construct and make it cooperative, or a
different runtime strategy. (Sidecar instrumentation reverted; agentos is clean.)
STATUS: codex compiles + runs the full session lifecycle in the VM (boot → EE protocol →
config → auth → ThreadManager → start_thread → submit → live event loop) but hangs in the
agent loop before the model call on the single-threaded runtime.

## ✅✅✅ RESOLVED: codex completes a full model turn end-to-end on wasm32-wasip1

The "agent loop hangs" diagnosis above was *close but mislabeled* — it is NOT a tokio
concurrency/threading-model mismatch. The current-thread runtime drives codex's spawned
submission_loop correctly. The real root cause, found by instrumenting tokio's
current-thread scheduler (Handle::spawn / schedule / the block_on for-loop / per-task
spawn-location), was a **single blocking task that pins the only executor thread**:

- `codex-core` spawns a shell-snapshot task (`core/src/shell_snapshot.rs`) during session
  init that launches a **shell subprocess** to capture the environment and blocks on its
  exit. On wasm32-wasip1 the VM's child-process bridge cannot deliver that wait on the
  single-threaded runtime (the "could not retrieve pid for child process" warnings), so the
  task blocks the executor thread **forever**. The scheduler then never drains the rest of
  the local run queue — including the submission_loop — so the agent turn never advances and
  no model HTTP call is ever made. (Hence the sidecar net.connect handler stayed empty.)

Fixes (all in the codex fork, branch `wasi-port-codex-core`, all `#[cfg(target_os="wasi")]`):
1. `core/src/shell_snapshot.rs` — skip the snapshot on wasi (send `None`, return). A shell
   environment snapshot is meaningless in the VM and the subprocess wait deadlocks the runtime.
2. `core/src/state_db.rs` — `init` / `get_state_db` / `open_if_present` return `None` on wasi.
   sqlx-sqlite's blocking worker can't run on the thread-less runtime (open fails ENOTSUP,
   os error 58); skipping it also avoids perturbing session init.
3. `exec/src/session_turn_wasi.rs` — runtime is `new_current_thread().enable_time()` (no I/O
   reactor needed; network I/O is host-brokered/synchronous) and a plain
   `block_on(session_turn())`. The earlier yield-pump / `global_queue_interval(1)`
   experiments were chasing the wrong theory and are NOT needed once the blocking task is gone.

Test (`packages/core/tests/codex-fullturn.test.ts`): codex-exec --session-turn runs the real
codex-core agent in the VM, calls the mock OpenAI Responses API over wasi-http, streams the
SSE response, and emits `{"type":"start"}` … `{"type":"done"}` on a clean protocol channel
(stdout carries ONLY the EE protocol JSON). The mock (`tests/helpers/openai-responses-mock.ts`)
emits a faithful Responses SSE stream (`response.created` → `response.output_text.delta` →
`response.output_item.done` → `response.completed`); `OPENAI_BASE_URL` includes `/v1` to match
codex's URL convention (`{base}/responses`).

Build (reproducible): the wasi build uses `-Z build-std` with a patched rust-src sysroot.
Because `-Z build-std` injects `panic_unwind` via `--extern` while the wasm target needs
`panic_abort` resolved from the sysroot, the build:
  - stashes the prebuilt `wasm32-wasip1` `*.rlib`/`*.rmeta` out of the toolchain lib dir
    (keeping `self-contained/` crt) so build-std's own `libcore` is the only one (avoids the
    `E0152 duplicate lang item core` at the bin link), and
  - copies build-std's freshly-built `libpanic_abort-*.rlib` into that sysroot lib dir (so the
    bin's `-Cpanic=abort` resolves a `panic_abort` that is ABI-matched to build-std's core),
  - builds with `--config 'profile.release.panic="abort"'`.
See `/tmp/relink-codex.sh` for the exact invocation. (This sysroot massaging should be folded
into the agentos wasm toolchain so it is not a manual step.)

KNOWN REMAINING (non-blocking; turn works e2e):
- Rollout recorder logs `failed to queue rollout items: channel closed` (the writer task exits
  early on wasi — likely the git-info subprocess in `write_session_meta`, same blocking-child
  class as shell_snapshot). Non-fatal: the turn completes; agent-os resume uses adapter-passed
  history, not codex's on-disk rollout. Gating git-info on wasi would silence it.
- The session-turn engine reads `prompt` but not `history`; multi-turn resume via the EE ACP
  adapter needs history replay wired in.
- ACP-level integration into the unified agent-matrix lives in the EE codex adapter.
