# agentOS TODO

## Deferred

- **Typed session events**: `onSessionEvent` currently returns raw JSON-RPC envelopes. Add typed/parsed event objects (TextUpdate, ToolCallUpdate, StatusUpdate, etc.) as a discriminated union.
- **OpenCode testing**: Agent config exists for OpenCode but only PI is tested. Add OpenCode integration tests once PI is stable.
- **Session persistence**: Support resuming sessions across VM restarts (ACP `session/load`).
- **MCP server passthrough**: Forward MCP server configs to agents via `session/new` params.
- **Permission model**: Currently defaults to allow-all. Add configurable permission policies.
- **Resource budgets**: Expose secure-exec resource budgets (CPU time, memory, output caps) through AgentOs config.
- **Timing mitigation parity across JS and Wasm**: Ensure the new runtime applies the same timing-mitigation semantics to guest JavaScript and guest WebAssembly in both native and browser implementations. This must be designed into the new kernel/execution/sidecar bridge rather than left as a V8-only behavior.
- **Experimental Wasm flags to evaluate in the new sidecar**: Add a controlled experiment matrix for V8 Wasm flags that are plausibly useful for Agent OS: `--wasm-staging`, `--experimental-wasm-js-interop`, `--experimental-wasm-type-reflection`, `--experimental-wasm-memory-control`, `--experimental-wasm-fp16`, `--experimental-wasm-compilation-hints`, and `--experimental-wasm-growable-stacks`. Evaluate correctness, browser-parity impact, module-loader impact, and whether any of these materially improve the runtime model versus just increasing risk.
- **Build-gated Wasm experiments to evaluate from source builds**: If we build V8 from source for sidecar experiments, try the gated combinations that may matter operationally: `v8_enable_drumbrake=true` with `--wasm-jitless`, `v8_enable_wasm_simd256_revec=true` with `--experimental-wasm-revectorize`, and `v8_enable_wasm_gdb_remote_debugging=true` with `--wasm-gdb-remote`. Treat these as research tracks, not default runtime settings.
- ~~**Network test broken**~~: Resolved by ARC-051 Rust kernel cutover. The network test passes against the Rust sidecar.
- **ESM module linking for host modules**: The V8 Rust runtime's ESM module linker doesn't forward named exports from host-loaded modules (via ModuleAccessFileSystem overlay). VFS modules work fine. This blocks running complex npm packages (like PI) in ESM mode inside the VM. Fix requires changes to the Rust V8 runtime's module linking callback.
- **CJS event loop processing**: CJS session mode ("exec") doesn't pump the event loop after synchronous code finishes. Async main() functions return Promises that never resolve. Needed for running agent CLIs (PI, OpenCode) that use async entry points. Fix requires the V8 Rust runtime to process the event loop in exec mode, or adding a "run" mode that does.
- **Full PI headless test**: Tests in pi-headless.test.ts verify mock API + PI module loading, but full PI CLI execution (main() → API call → output) is blocked by the ESM and CJS issues above. Once those are fixed, add a test that runs PI end-to-end with the mock server.
- ~~**VM stdout doubling**~~: Resolved by ARC-051 Rust kernel cutover. Root cause was in the deleted TypeScript kernel's stdio handling. Verified: single delivery against Rust sidecar.
- ~~**VM stdin doubling**~~: Resolved by ARC-051 Rust kernel cutover. Root cause was in the deleted TypeScript kernel's pipe handling. Verified: single delivery against Rust sidecar.
- **Concurrent VM processes and stdin**: When two processes are running inside the same VM with `streamStdin: true`, `writeStdin()` to one process appears to block or deadlock. Multi-agent example works around this by running sessions sequentially (close one before opening the next). Root cause was originally in secure-exec's process/pipe management — needs re-verification against Rust sidecar.
- **File watching (inotify, fs.watch)**: Not implemented in secure-exec. Agents cannot watch for filesystem changes. Needs kernel-level support for watch descriptors and change notification callbacks.
