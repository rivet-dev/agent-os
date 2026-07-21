# AgentOS language execution Benchmarks

These benchmarks measure the public `agentos` SDK paths used by consumers.

## Cold Start Matrix

`coldstart.bench.ts` writes machine-readable JSON to stdout and human-readable progress to stderr. It measures:

- `owned-sidecar`: `NodeRuntime.create()` owns a fresh sidecar for each runtime.
- `shared-sidecar`: a `Sidecar` is created once per batch and passed to `NodeRuntime.create({ sidecar })`; sidecar setup is measured separately and excluded from cold start.
- `resident-runner`: a shared sidecar plus `runtime.createResidentRunner()`, so repeated tiny snippets reuse one live guest Node process.

The result JSON includes hardware metadata, aggregate cold/warm latency, and phase timings such as `session_open`, `vm_create`, `runtime_mount_wasm`, `first_exec`, and resident-runner phases.

## Run

Build a release sidecar first for meaningful timings:

```bash
cargo build --release -p agentos-native-sidecar
```

Run the full benchmark suite:

```bash
pnpm --dir packages/benchmarks bench
```

This writes timestamped files under `packages/benchmarks/results/`:

```text
coldstart-YYYYMMDD-HHMMSS.json
coldstart-YYYYMMDD-HHMMSS.log
memory-YYYYMMDD-HHMMSS.json
memory-YYYYMMDD-HHMMSS.log
```

Run one focused lane by name:

```bash
BENCH_ONLY=sync-bridge-floor pnpm --dir packages/benchmarks bench
BENCH_ONLY=ls-serial pnpm --dir packages/benchmarks bench
BENCH_ONLY=process-spawn pnpm --dir packages/benchmarks bench
```

Run one matrix family:

```bash
BENCH_FAMILIES=fs pnpm --dir packages/benchmarks bench:matrix
BENCH_FAMILIES=modules pnpm --dir packages/benchmarks bench:matrix
BENCH_FAMILIES=ecosystem pnpm --dir packages/benchmarks bench:matrix
BENCH_FAMILIES=permissions pnpm --dir packages/benchmarks bench:matrix
```

Run one matrix op:

```bash
BENCH_FAMILIES=net BENCH_OP_FILTER=tls_loopback_get pnpm --dir packages/benchmarks bench:matrix
```

## Baseline And PR Gate

The committed local baseline is `packages/benchmarks/results/baseline-local.json`. It is generated from the full 70-row latency matrix and records:

- **Hardware**: `getHardware()` output for the canonical machine.
- **Sidecar provenance**: binary path, inferred profile, mtime, and size.
- **Engine defaults**: iterations, warmup, cold/shared-VM mode, and row count.
- **Rows**: per-row lane p50 values and memory columns where available.

Regenerate it only from a release sidecar on the canonical machine:

```bash
pnpm install --frozen-lockfile --filter "@rivet-dev/agentos-runtime-benchmarks..."
cargo build --release -p agentos-native-sidecar
cargo build --release -p native-baseline
cargo build --release -p native-baseline --target wasm32-wasip1
AGENTOS_SIDECAR_BIN="$PWD/target/release/agentos-native-sidecar" \
	pnpm --dir packages/benchmarks bench:baseline
```

`bench:baseline` refuses debug sidecars and refuses filtered matrix runs. A complete baseline has `engine.rowCount = 70`.

`pnpm --dir packages/benchmarks bench:gate` runs the deterministic PR subset with 9 measured iterations and 3 warmup iterations, then compares p50 against the baseline:

- **Threshold**: fail when current p50 is greater than `2.0x` the baseline p50. Override with `BENCH_GATE_THRESHOLD`.
- **Tiny-row floor**: rows with baseline p50 below `0.3ms` are ignored unless current p50 reaches at least `1ms`, so sub-millisecond timer noise does not flap the gate.
- **Row override**: use `BENCH_GATE_ROWS=family/op[:lane],...` to run a smaller or different subset.
- **Release-only**: the gate exits `2` if `AGENTOS_SIDECAR_BIN` resolves to a debug sidecar.

Gate rows:

| Row | Lane | Why it is gated |
| --- | --- | --- |
| `fs/fs_write_small` | `guest` | Tiny sync write hot path, with the 1ms tiny-row floor. |
| `fs/fs_write_big` | `guest` | Large write payload catches whole-buffer copy regressions. |
| `fs/fs_read_small` | `guest` | Small read bridge/VFS floor. |
| `fs/stat_storm` | `guest` | Metadata syscall hot path. |
| `fs/readdir_small` | `guest` | Directory enumeration without the high variance of large listings. |
| `net/tcp_connect_close` | `guest` | TCP socket lifecycle floor. |
| `net/tcp_echo_small` | `guest` | Small TCP payload round trip. |
| `net/http_loopback_get` | `guest` | HTTP over kernel sockets with a loopback server. |
| `modules/import_fresh_file` | `guest` | Dynamic import and filesystem resolution. |
| `modules/require_100_small` | `guest` | CommonJS resolver/cache behavior. |
| `control/cpu_loop` | `wasm` | WASM runtime lane sanity check. |
| `ecosystem/ls_100` | `vmCmd` | End-to-end WASM command tier. |

The CI baseline is environment-specific because GitHub-hosted runner hardware differs from the canonical machine. In CI, `bench:gate` uses `packages/benchmarks/results/baseline-ci.json`. If it is missing, PR gates skip loudly. The nightly workflow runs the full matrix, writes a candidate `baseline-ci.json`, and uploads it as an artifact. Bootstrap is:

1. Let the first nightly workflow finish.
2. Download `baseline-ci.json` from the `nightly-benchmark-results` artifact.
3. Commit it at `packages/benchmarks/results/baseline-ci.json`.
4. Future PRs enforce the quick gate against that CI baseline.

### Latency Matrix VM Lifecycle

The latency matrix gives each guest-backed benchmark op a dedicated sidecar and VM by default. Host-only lanes (`native`, `node`, and `hostCmd`) run before that VM is created; guest-backed lanes (`guest`, `wasm`, and `vmCmd`) run inside the op's VM, which is disposed before the next op.

Each row also reports peak memory where the lane can be measured. Guest-backed lanes (`guest`, `wasm`, and `vmCmd`) use Linux `/proc/<sidecarPid>/clear_refs=5`, then subtract baseline `VmRSS` from post-lane `VmHWM` so the value is above the prewarmed-sidecar baseline. Native and default host Node lanes spawn the measured child directly and sample `/proc/<pid>/status` `VmHWM`, minus a startup no-op baseline (`native-baseline cpu_loop --iters 1 --warmup 0` and `node -e ""`), floored to one page. Non-Linux runs print one reason and render memory columns as `-`.

Warmup contract:

- **Guest Node prewarm**: `prewarmBenchVm(vm, op)` runs one trivial guest Node program to force isolate creation, bridge snapshot load, and first-exec paths before timed sampling.
- **Native-baseline WASM prewarm**: when an op has a supported `vm-wasm` lane, the helper runs `native-baseline --op cpu_loop --iters 1 --warmup 0` once so module compilation is outside the measured samples.
- **Command WASM prewarm**: command ops run one discarded VM-command sample (`iters=1`, `warmup=0`) so command module compilation is outside the measured samples.
- **Op warmup**: `BENCH_WARMUP` still runs inside each op and is discarded from the reported stats.

Useful latency-matrix knobs:

```text
BENCH_COLD=1        Skip the VM prewarm contract above. Cold-start lanes remain the canonical cold measurements.
BENCH_SHARED_VM=1   Reuse a VM where op-specific VM options are not required. Default is per-op VM isolation.
```

Each matrix JSON records the sidecar binary used for the run:

- **`sidecar.path`**: `NodeRuntime`-resolved sidecar binary path, including `AGENTOS_SIDECAR_BIN` overrides and local checkout fallbacks.
- **`sidecar.profile`**: inferred from the binary path (`debug`, `release`, or `unknown`).
- **`sidecar.mtimeMs` / `sidecar.mtimeIso`**: sidecar binary modification time.
- **`sidecar.sizeBytes`**: sidecar binary size in bytes.

## Focused Lanes

Focused lanes live under `src/focused/` and preserve the legacy CLI flags, env vars, JSON shape, and stderr tables from the Agent OS benchmark scripts. They use `src/lib/vm.ts` over `NodeRuntime`.

- **`sync-bridge-floor`**: no-op sync bridge RPC floor. Knobs: `BENCH_SYNC_BRIDGE_ITERATIONS`, `BENCH_SYNC_BRIDGE_WARMUP`, `BENCH_SYNC_BRIDGE_CALL_COUNTS`, `BENCH_SYNC_BRIDGE_PAYLOAD_BYTES`, `BENCH_SYNC_BRIDGE_RPC_LATENCY`, `BENCH_SYNC_BRIDGE_PHASES`.
- **`sync-bridge-floor-phases`**: bridge floor with latency and phase diagnostics enabled.
- **`sync-bridge-floor-bigargs`**: bridge floor with a larger payload, default 64 KiB.
- **`fs-sync-ops`**: focused sync filesystem operation bundles. Knobs: `BENCH_FS_SYNC_ITERATIONS`, `BENCH_FS_SYNC_WARMUP`, `BENCH_FS_SYNC_OPS`, `BENCH_FS_SYNC_CALL_COUNTS`, `BENCH_FS_SYNC_FIXTURES`, `BENCH_FS_SYNC_PAYLOAD_BYTES`, `BENCH_FS_SYNC_RPC_LATENCY`, `BENCH_FS_SYNC_PHASES`.
- **`fs-sync-ops-phases`**: sync filesystem floor with phase diagnostics enabled.
- **`dns-lookup-floor`**: warm, repeated, concurrent, and fresh-process DNS lookup rows. Knobs: `BENCH_DNS_LOOKUP_ITERATIONS`, `BENCH_DNS_LOOKUP_WARMUP`, `BENCH_DNS_LOOKUP_ROWS`.
- **`net-tcp-event-floor`**: TCP loopback event-floor rows. Knobs: `BENCH_NET_TCP_ITERATIONS`, `BENCH_NET_TCP_WARMUP`, `BENCH_NET_TCP_ROWS`, `BENCH_NET_TCP_POLL_DELAY_MS`, `BENCH_NET_TCP_TRACE`.
- **`net-tcp-cadence-trace`**: TCP trace attribution rows with bridge tracing enabled.
- **`concurrency-vms`**: N owned sidecars/VMs concurrently run sustained `tcp_echo_small` loops for a fixed wall window. Knobs: `BENCH_CONCURRENCY_COUNTS` (default `1,4,8`), `BENCH_CONCURRENCY_DURATION_MS` (default `5000`).
- **`interference`**: one busy VM alternates CPU spin and filesystem write churn while a second VM samples `fs_write_small`. Knobs: `BENCH_INTERFERENCE_DURATION_MS` (default `5000`), `BENCH_INTERFERENCE_BUSY_DURATION_MS` (default probe duration plus `1000`).
- **`concurrent-processes`**: one VM runs N concurrent guest Node processes doing sustained `fs_write_small` loops, exposing the per-VM service ceiling. Knobs: `BENCH_PROCESS_COUNTS` (default `1,4,8`), `BENCH_PROCESS_DURATION_MS` (default `5000`).
- **`readdir-scaling`**: pure readdir scaling with setup outside the timed loop. Knobs: `BENCH_READDIR_ITERATIONS`, `BENCH_READDIR_WARMUP`, `BENCH_READDIR_ENTRY_COUNTS`, `BENCH_READDIR_MODES`, `BENCH_READDIR_FIXTURES`, `BENCH_READDIR_WORKLOADS`.
- **`readdir-probe`**: guarded/probe readdir shapes.
- **`mount-readdir`**: host mount-table readdir scaling. Knobs: `BENCH_MOUNT_READDIR_ITERATIONS`, `BENCH_MOUNT_READDIR_WARMUP`, `BENCH_MOUNT_READDIR_COUNTS`, `BENCH_MOUNT_READDIR_ENTRY_COUNT`.
- **`overlay-readdir`**: explicitly skipped in agentos because Agent OS TypeScript overlay layer-store APIs are not exposed here.
- **`process-spawn`**: native baseline, host Node, and guest VM process-spawn floor. Knobs: `BENCH_ITERATIONS`, `BENCH_WARMUP`, `BENCH_PROCESS_LIFECYCLE_TRACE`.
- **`wasm-command-floor`**: direct WASM command startup/capture floor. Knobs: `BENCH_WASM_COMMAND_FLOOR_ITERATIONS`, `BENCH_WASM_COMMAND_FLOOR_WARMUP`, `BENCH_WASM_COMMAND_FLOOR_SERIAL_RUNS`, `BENCH_WASM_COMMAND_FLOOR_STDOUT_SIZES`, `BENCH_WASM_COMMAND_FLOOR_WARMUP_DEBUG`.
- **`wasm-command-floor-debug`**: command floor with WASM warmup diagnostics.
- **`echo-cold-warm`**: cold/warm WASM shell `echo hello`.
- **`ls-serial`**: VM startup plus serial `ls`. Knobs: `BENCH_LS_ITERATIONS`, `BENCH_LS_WARMUP`, `BENCH_LS_SERIAL_RUNS`, `BENCH_LS_FILE_COUNTS`, `BENCH_LS_WASM_WARMUP_DEBUG`.
- **`wasi-ls-scaling`**: focused `ls` command scaling. Knobs: `BENCH_WASI_LS_ITERATIONS`, `BENCH_WASI_LS_WARMUP`, `BENCH_WASI_LS_SERIAL_RUNS`, `BENCH_WASI_LS_FILE_COUNTS`, `BENCH_WASI_LS_VARIANTS`, `BENCH_WASI_LS_WASM_WARMUP_DEBUG`, `BENCH_WASI_LS_SYSCALL_COUNTERS`.
- **`wasi-ls-scaling-counters`**: `ls` scaling with syscall counters.

The shell/coreutils focused lanes use the local `NodeRuntime` command-dir resolution, which prefers `toolchain/target/wasm32-wasip1/release/commands` when `make -C toolchain wasm` has been run.

## Net Family

`BENCH_FAMILIES=net pnpm --dir packages/benchmarks bench:matrix` runs loopback networking rows through the host Node and guest VM lanes, plus native/WASM lanes where a baseline exists.

Rows:

- **`udp_echo_small` / `udp_echo_big`**: UDP loopback echo of one 16 byte datagram or one 60 KiB datagram, currently expected to surface unsupported guest behavior.
- **`unix_echo_small` / `unix_echo_big`**: Unix-domain socket echo of one 16 byte or 64 KiB payload.
- **`http_loopback_get`**: persistent `node:http` loopback server, fresh GET per iteration.
- **`fetch_loopback_get`**: persistent HTTP loopback server, fresh global `fetch()` per iteration.
- **`tls_loopback_get`**: persistent `node:https` loopback server, fresh `https.get` per iteration, verifies `hello-loopback-tls`. Native is unsupported until a native TLS-loopback pair exists, and WASM is unsupported because the native baseline has no TLS lane.
- **`tcp_connect_close`**: TCP client connects to a loopback server and closes.
- **`tcp_echo_small` / `tcp_echo_big`**: TCP loopback echo of one 16 byte or 64 KiB payload. The big tier carries forward the old throughput row's full-payload verification.
- **`tcp_concurrent_4`**: four concurrent TCP loopback clients connect to one server.
- **`tcp_tiny_writes_16`**: TCP loopback echo using sixteen one-byte writes. This is a write-count/cadence row, not a payload-size tier.

## Payload Tier Renames

Payload-sensitive matrix rows use exactly two tiers named `<op>_small` and `<op>_big`. Retired names map to these successors:

| Retired row | Successor row |
| --- | --- |
| `small_write` | `fs_write_small` |
| `big_read` | `fs_read_big` |
| `readdir_large` | `readdir_small` |
| `stream_copy_1m` | `stream_copy_big` |
| `tcp_echo` | `tcp_echo_small` |
| `tcp_throughput_64k` | `tcp_echo_big` |
| `unix_echo_small` | `unix_echo_small` (kept, added `unix_echo_big`) |
| `udp_echo_small` | `udp_echo_small` (kept, added `udp_echo_big`) |
| `throughput_64k` | `pass_through_big` |
| `spawn_stdout_256k_capture` | `spawn_stdout_capture_big` |

## Permissions Family

`BENCH_FAMILIES=permissions pnpm --dir packages/benchmarks bench:matrix` runs a guest-only policy-overhead A/B over hot fs and net rows. Each reused op has an `<op>_allow` row using the benchmark default allow-all VM permissions and an `<op>_policy` row using a realistic restrictive policy.

Policy shape:

- **Filesystem**: default deny, then 15 allow glob rules for runtime paths plus benchmark script and working paths. The op's actual `/tmp/fuzz-perf-*` paths are placed at the tail so last-match policy evaluation walks the list.
- **Network**: default deny, then five TCP loopback allowlist rules. The real `tcp://127.0.0.1:*` rule is last so loopback listen/connect checks walk the list.
- **Other scopes**: `childProcess`, `process`, and `env` stay allowed so the measurement isolates fs and network syscall permission overhead.

The matrix prints a dedicated permissions table and writes `permissionPolicyTax` to `results/latency-matrix.json` and `results/findings.json`. `policyTax = policy guest p50 / allow guest p50`; any row above `1.2` emits a finding. A one-off 200-rule local variant can validate methodology by increasing the rule list temporarily, running one op, and removing that variant before committing.

## Modules Family

`BENCH_FAMILIES=modules pnpm --dir packages/benchmarks bench:matrix` runs JavaScript module-resolution and import-heavy rows through the host Node and guest VM lanes. Native and WASM lanes are unsupported because module resolution is a JS-runtime surface.

These rows cap the default matrix sample budget at five measured iterations plus one warmup so each row still loads 100 unique files per iteration without making debug-sidecar runs impractical.

Rows:

- **`require_100_small`**: stages 100 unique tiny CommonJS files per warmup/measured iteration, then requires that iteration's directory and verifies the exported-value sum.
- **`import_100_small_esm`**: stages 100 unique tiny ESM files per warmup/measured iteration, then dynamic-imports that iteration's directory and verifies the exported-value sum.
- **`import_npm_package`**: dynamic-imports `zod@4.3.6` from the workspace `node_modules` tree mounted read-only in the guest. The inspected static ESM graph from `index.js` contains 76 transitive module files. The benchmark uses one fresh Node process per measured iteration so the whole graph reloads instead of only cache-busting the entry URL.
- **`import_fresh_file`**: moved from `fs/module_import_fresh`; writes a unique `.mjs` file, dynamic-imports it, verifies the exported value, and unlinks it.

## Ecosystem Family

`BENCH_FAMILIES=ecosystem pnpm --dir packages/benchmarks bench:matrix` runs end-to-end command workloads through two lanes:

- **`hostCmd`**: real host binaries via `child_process`.
- **`vmCmd`**: the same command in the VM through the WASM command tier.

Rows:

- **`ls_100`**: `ls -1` over a 100-file directory.
- **`sh_pipeline`**: `sh -c "ls -1 | grep -c ."` over the 100-file directory.
- **`cd_tmp_pwd`**: `sh -c "cd /tmp && pwd"` with `/tmp` verification.
- **`echo_hello`**: `echo` emits a known line.
- **`cat_small` / `cat_big`**: `cat` emits exact 4 KiB and 1 MiB fixture contents.
- **`grep_small` / `grep_big`**: `grep -l needle` over 32-file and 1000-file fixtures with known match counts.
- **`sed_substitution`**: `sed` replaces every known token in a fixture.
- **`find_1000`**: `find` walks a 1000-file tree and verifies the file count.
- **`tar_small` / `tar_big`**: `tar` creates, extracts, diffs a sentinel file, and lists 32-file and 128-file archives.
- **`gzip_small` / `gzip_big`**: `gzip` compresses/decompresses 4 KiB and 64 KiB fixtures, then diffs the round trip.
- **`jq_extract`**: `jq` extracts a known JSON field.
- **`git_init_commit`**: `git init && git add . && git commit`, then verifies `rev-parse HEAD`.

Run only the cold-start matrix:

```bash
AGENTOS_SIDECAR_BIN="$PWD/target/release/agentos-native-sidecar" \
	pnpm --silent --dir packages/benchmarks bench:coldstart \
	> packages/benchmarks/results/coldstart-local.json \
	2> packages/benchmarks/results/coldstart-local.log
```

Quick smoke run:

```bash
BENCH_BATCH_SIZES=1 \
BENCH_ITERATIONS=1 \
BENCH_WARMUP=0 \
BENCH_SCENARIOS=owned-sidecar,shared-sidecar,resident-runner \
AGENTOS_SIDECAR_BIN="$PWD/target/release/agentos-native-sidecar" \
	pnpm --silent --dir packages/benchmarks bench:coldstart
```

Useful knobs:

```text
BENCH_BATCH_SIZES=1,10,50,100,200
BENCH_ITERATIONS=5
BENCH_WARMUP=1
BENCH_SCENARIOS=owned-sidecar,shared-sidecar,resident-runner
BENCH_MAX_LIVE_RUNTIMES=8
BENCH_MAX_RESIDENT_RUNNERS=1
BENCH_EXEC_TIMEOUT_MS=30000
AGENTOS_SIDECAR_BIN=/abs/path/to/agentos-native-sidecar
```

## Checked-In Results

Current captured results:

- `results/coldstart-final.json`
- `results/coldstart-final.log`
- `results/coldstart-resident-full-matrix-20260619.json`
- `results/coldstart-resident-full-matrix-20260619.log`

`coldstart-final.*` is the latest full run from June 19, 2026. It includes the three SDK scenarios above. It also contains an `isolate-only` reference row from a lower-level one-off V8 snapshot/restore benchmark; that row is captured for comparison but is not part of the normal SDK benchmark command.
