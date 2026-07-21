# VERIFIED: tokio::process compiles+works on wasm32-wasip1 (2026-06-23)

Proven in isolation (/tmp/tokio-dev + [patch], --cfg tokio_unstable, -Z build-std):
`Command::new("echo").arg("hi").output().await` COMPILES for wasm32-wasip1.
This de-risks codex's entire exec path under the pipeline-only approach.

## The exact changes (to capture as agentos patches):

### A. tokio crate patch (std-patches/crates/tokio/):
1. src/macros/cfg.rs — in `macro_rules! cfg_process`, remove `#[cfg(not(target_os = "wasi"))]`.
2. src/process/mod.rs — after the windows imp block, add:
       #[path = "wasi.rs"]
       #[cfg(target_os = "wasi")]
       mod imp;
3. src/process/wasi.rs — NEW file = ./wasi-process-imp.rs (this dir). Routes to std::process.

### B. std patch (patches/*.patch — also fixes the stale 0001 patch):
1. library/std/src/sys/process/wasi.rs — add to ChildPipe:
       impl crate::os::fd::AsFd for ChildPipe { fn as_fd(&self)->BorrowedFd<'_>{ self.0.as_fd() } }
       impl crate::sys::IntoInner<crate::os::fd::OwnedFd> for ChildPipe {
           fn into_inner(self)->OwnedFd { self.0.into_inner() } }
2. library/std/src/os/wasi/process.rs — NEW file (public ChildStdin/out/err fd impls:
   AsRawFd/IntoRawFd/AsFd/From<_> for OwnedFd via as_inner().as_fd() / into_inner().into_inner()).
3. library/std/src/os/wasi/mod.rs — add `pub mod process;` after `pub mod net;`.

### C. Makefile: add `--cfg tokio_unstable` to the wasm-target RUSTFLAGS.

## NEXT (remaining pipeline-only): same pattern for tokio::net (host_net sockets) → unblocks
reqwest/tungstenite/rmcp → vendor codex into toolchain (resolve version conflicts) →
make wasm builds codex-core UNCHANGED → un-stub codex-exec --session-turn → EE adapter + a5 test → matrix.

## UPDATE — VERIFIED 2026-06-23 (compile): full hard stack compiles on wasm32-wasip1
`tokio` (features process+net+rt+macros+io-util+time) + `reqwest` (rustls-tls) COMPILE TOGETHER
for wasm32-wasip1 with the tokio patch + `--cfg tokio_unstable` (mio compiles as a limited impl).
=> codex-core's hardest deps (tokio::process, tokio::net, reqwest, rmcp) compile pipeline-only with
NO codex source changes. tokio::process is verified to also RUN. tokio::net/reqwest COMPILE; RUNTIME
HTTP needs routing to wasi-http (reqwest connector patch) because wasi preview1 has no outbound connect.

## Remaining for codex-core to COMPILE pipeline-only (toolchain-level, codex source unchanged):
- C-dep crates: `zip` (xz/lzma) → `[patch.crates-io] zip` defeatured (deflate only); `sqlx`/sqlite
  (codex-state) → stub or [patch] (no wasi sqlite). These are [patch]/vendoring, not codex edits.
- stub crates codex-network-proxy/codex-otel 0.0.0 already exist.
Then: vendor codex into toolchain → make wasm builds codex-core unchanged.
## Remaining for codex to WORK: reqwest→wasi-http connector patch (runtime HTTP) → un-stub
codex-exec --session-turn → EE adapter + a5 test → matrix.
