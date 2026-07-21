# AgentOS xfstests correctness suite

`make -C tests/xfstests run` is the only entrypoint. It stages the pinned
upstream xfstests revision, applies the tracked AgentOS-only harness patches,
generates exact exclusions, and invokes the ignored Rust integration runner.
The run is strict: every selected test must execute and pass unless its exact
test/backend outcome has a reviewed record in `exceptions.toml`.
The runner classifies each completed case immediately and stops scheduling new
cases on the first strict violation. Already-running cases finish and tear down,
then the partial full-selection report is written before the command fails. Fix
the first reported violation with focused coverage and rerun the complete pinned
selection; a qualifying full run completes every selected case.

## Result policy

An upstream `notrun` is a coverage hole and fails by default. `excluded` is
reserved for tests whose subject is mount construction or mkfs administration;
correctness-relevant existing-mount policy such as read-only and atime behavior
must execute. `allowed-notrun` is reserved for an exact test/backend absence of
a named non-POSIX filesystem feature whose architecture does not apply to that
backend; its literal upstream reason must match or the run fails closed.
`deferred` is real remount/crash/fault coverage awaiting a named host hook.
`expected-failure` tests still execute and must match their normalized output
digest; an unexpected pass or changed failure is an error. `reduced` is reserved
for an exact whole-object test/backend repetition reduction with named full and
reduced counts plus focused semantic coverage; the reduced test still executes
and any non-pass outcome fails. Wildcards,
auto-blessing, unused records, duplicate records, and stale records are errors.

Each WASM engine writes reports under `report/<wasm-backend>/`: the main files
are `results.md`, `agentos-gaps.md`, and `surface-audit.md`, with
driver-specific summaries under `backends/<storage-backend>/results.md`.
`XFSTESTS_WASM_BACKENDS` defaults to the required `v8 wasmtime` executor
matrix. `XFSTESTS_BACKENDS` defaults to the
`chunked_local,memory,chunked_s3` supported writable-engine matrix and rejects
unknown or duplicate entries. The dormant `object_s3` harness paths are retained
for focused return-to-service validation, but the plugin is intentionally not
registered or user-selectable while its whole-object write-back, durability, and
request-amplification contract remains incomplete. Each enabled S3 case owns an
ephemeral local server and isolated object prefixes; it never depends on shared
cloud state.
Per-test backing roots are deleted on success,
failure, timeout, and unwind; a Make-owned run root and shell trap also cover
external cancellation. `XFSTESTS_CONCURRENCY` bounds simultaneous VMs;
`XFSTESTS_MAX_TEMP_BYTES` bounds aggregate backing storage. The default
per-test watchdog is 3,600 seconds, calibrated from the pinned helper workloads
with byte-for-byte verification enabled;
a timeout remains a strict harness failure.
Nightly CI also runs the ignored 1,000-file `dirstress` process matrix once per
WASM engine on `chunked_local`; it covers the one-process, five-process shared,
and five-process/five-directory layouts without multiplying the same workload
across every storage-plugin leg.

## Correctness constraints

- Guest commands have a shadow tree, so the runner first proves bidirectional
  I/O through the mounted kernel VFS. A shadow-only result aborts the suite.
- The runner reads `/proc/mounts` in the guest and requires exactly one writable
  `agentos` entry for both `/mnt/test` and `/mnt/scratch`.
- Mountpoints must exist in the kernel VFS and the guest shadow tree.
- Every test gets a fresh VM and fresh test/scratch engines. Mkfs and unmount
  reset calls are scaffolding no-ops, while root-only supported remount policy is
  applied to the existing mount; VM disposal is the storage reset boundary.
- `xfs_io`, `fsstress`, `fsx`, xfstests `src/` helpers, xattr tools, ACL tools,
  and their syscall imports are genuine porting surfaces. Missing callers are
  reported, never replaced with successful stubs.
- `/proc/mounts` must describe host-applied mounts truthfully; procfs-only
  hardcoding is not acceptable.
- VM-local users and groups must use production credential transitions and DAC.
  Capability policy does not bypass owner/group/other permissions.
- Teardown copies results out first, then removes metadata databases, blocks,
  shadow roots, and transient result mounts. Retention is opt-in and bounded.
- The source projection is read-only while `RESULT_BASE` is a separate writable
  mount. Startup probes must prove every required source/result write location.
- Toolchain or brush incompatibility is a harness failure. Harness patches may
  alter mount scaffolding and device gates; helper portability patches may
  replace unavailable process-launch primitives while preserving concurrency,
  credentials, and exit status. No patch may weaken an assertion.
- Patch staging fails closed on the first check or apply error and always starts
  by cleaning back to the pinned SHA.
- Inherited pipe readers consume bytes only when they actually read. Buffered
  bytes precede EOF and each chunk has one consumer; the verify-first gate runs
  an external command inside a `while read` loop to enforce this ordering.
- `_check_generic_filesystem` is a scaffolding no-op for virtual `agentos`
  mounts because there is no guest-visible block format. It never suppresses
  test-level corruption or data comparisons.
- Worker logs and inline report details are capped. Full bounded detail,
  stdout, and stderr remain linked as raw per-test artifacts.
- Remount durability, freeze/thaw, resize, shutdown, and error injection remain
  visible deferred coverage until real host lifecycle hooks exist.
- `generic/` includes Linux extensions as well as POSIX behavior. The generated
  surface audit, pinned tests, and exact built helpers define the required ABI.
- The final matrix covers every writable native plugin plus bare, one-lower,
  multi-lower, and meaningful mixed-engine overlay configurations separately.
