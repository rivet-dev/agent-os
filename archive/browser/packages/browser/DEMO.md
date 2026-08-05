# Agent OS in the browser — demo

A runnable demo of the **converged Agent OS sidecar compiled to WebAssembly**,
booting and answering a real ACP wire request inside a Chromium tab. The same
Rust kernel + ACP engine that runs natively, running in the browser, with the
wasm kernel as the sole enforcement point.

## Run it

```bash
cd packages/browser
# Builds the wasm-web package + the ACP codec bundle, then serves the demo.
node ./scripts/build-wasm-test-assets.mjs
PORT=43175 node ./tests/browser-wasm/serve.mjs
# open http://localhost:43175/demo.html and click "Run demo"
```

Or run it headless (the demo page is verified by Playwright):

```bash
pnpm --filter @rivet-dev/agentos-browser test:browser-wasm
```

## What the demo does

1. **Boots** the `agentos-sidecar-browser` wasm module in the page and reports its
   `sidecarId`.
2. **Authenticates** over the BARE wire protocol (`@rivet-dev/agentos-runtime-core` codec).
3. Sends an ACP **`get_session_state`** request as a wire `ExtEnvelope`.
4. The frame is routed `BrowserAcpExtension → AcpCore` (the host-free engine
   shared verbatim with the native sidecar), which returns a real ACP response.

This exercises the full converged path in the browser: wire decode → auth →
extension dispatch → host-free ACP core → wire encode, with no host I/O and no
guest-side permission evaluation (the wasm kernel enforces).

## Why this path and not a full `pi` session (yet)

The demo round-trips an ACP request that needs no agent process. Running a full
agent such as **`pi`** end-to-end *in the browser* additionally requires two
pieces that are out of scope for the current browser-convergence milestone:

- **Browser agent-process executor (a deep architectural blocker, not a wiring).**
  The converged sidecar + the synchronous host-free `AcpCore` run on the **main
  thread**; `pushFrame` is synchronous and `create_session` must block-wait for the
  agent's stdout. But an agent runs in a **Worker** (async), and the main thread
  cannot block-wait for it: `postMessage` output can't arrive while `pushFrame` is
  blocking, and `Atomics.wait` is **forbidden on the main thread** in browsers. So
  the converged executor's `startExecution`/`writeExecutionStdin`/
  `pollExecutionEvent` callbacks are deliberate no-ops. Resolving it requires either
  running the sidecar+`AcpCore` **inside a worker** (so `Atomics.wait` is legal) or
  making the ACP orchestration **asynchronous/resumable** — both substantial
  redesigns. The host-free `AcpCore` + `BrowserAcpHost` seam are in place
  (`archive/browser/crates/sidecar-core`, `archive/browser/crates/sidecar-browser/src/acp_host.rs`)
  and proven end-to-end natively; only the in-browser agent-process *host* is gated
  on this redesign.
- **Host network egress.** `pi` calls the Anthropic API. Browser convergence is
  loopback-only by design; host egress is out of scope. So `pi`'s API calls
  cannot leave the browser sandbox as currently scoped, independent of the
  executor.

The native side already proves the engine end-to-end: `AcpCore` drives a real
agent process through a full `initialize` + `session/new` handshake in
`archive/browser/crates/sidecar-core/tests/real_agent_round_trip.rs` (native `AcpHost`
over `std::process`). The browser demo proves the same engine answers real ACP
wire requests in Chromium. Closing the gap between them is the
agent-process-executor work tracked in `AGENTOS-WEB-CONVERGENCE.md`.
