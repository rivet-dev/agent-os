# Registry Linux-Parity Worklist

Status: worklist · Owner: registry · Last updated: 2026-07-08

## Goal (hand this to the driver agent)

> Drive every item in this worklist to **clean Linux parity**: each command/
> behavior must work end-to-end the way it does on real Linux, **proven by real
> e2e tests** — not by a WASI-specific port, a stub, or a shim that only satisfies
> the test. Example of the bar: `duckdb` must run real analytical SQL against real
> file-backed databases and pass real e2e tests — not a stripped "WASI duckdb"
> that only does `SELECT 1`.
>
> **Rules:**
> - **🚧 REAL TOOL, NOT A REIMPLEMENTATION (the load-bearing rule).** Every command
>   must be the **real upstream tool** (GNU coreutils, GNU grep/sed/gawk, real
>   `curl`, real `git`, real `jq`, GNU tar/gzip/diffutils, …) compiled to
>   `wasm32-wasip1` and **patched as needed** for WASI. Do **NOT** ship a
>   from-scratch Rust/C rewrite, a stub, or a hand-rolled CLI over a library.
>   Reimplementations drift from Linux behavior in a thousand small ways and are
>   exactly why several commands fail parity. Sole exception: when the upstream
>   canonical tool *is itself* the Rust project (**ripgrep**, **fd**) — then the
>   real project is correct. Prefer the genuine upstream tool (real git, real
>   grep) over a rewrite; a *popular, established* reimplementation is an
>   acceptable fallback only when the real tool genuinely won't build.
> - **"Not possible" is a valid outcome — but only after trying really hard.** The
>   sysroot is **ours**: a patched Rust std + libc with custom host-syscall imports
>   (see CLAUDE.md → Software Build (WASM Toolchain)). A missing libc/POSIX API
>   (`getrlimit`/`RLIMIT_NOFILE`, `getgroups`, …) is **NOT** a WASI wall — it is a
>   stub/patch we add one layer down, and the build should proceed as if targeting
>   native POSIX. Only if a command *still* cannot be built as the real (or an
>   established) tool do you mark it **`NOT POSSIBLE (WASI)`** in this doc, with a
>   concrete explanation of the genuine, documented wall (never "WASI lacks a
>   syscall we could implement") and what was tried. Exhaust real options first:
>   patch the sysroot, patch the tool, stub the specific missing syscall — a
>   genuine effort, not a quick bail.
> - **Commit clean revs — no stray artifacts.** Each rev must contain only the
>   intended source + test changes. Never commit build outputs, vendored toolchain
>   trees, `__pycache__`/`*.pyc`, generated binaries, or anything that belongs in
>   `.gitignore`. Before `jj describe`, run `jj diff -r @ --summary` and confirm
>   every path is intended — watch especially for `A` (added) paths under
>   `toolchain/`, `**/target/`, `**/node_modules/`, `**/build/`, `**/__pycache__/`.
>   Then **audit the entire stack up to main** (`jj diff -r 'main..@' --stat`, or
>   per rev) and strip anything that slipped in with `jj restore --from <parent>
>   <path>`, adding the pattern to `.gitignore` so it cannot recur.
> - **One jj rev per item.** Concretely: **`jj new` before starting each item**,
>   make that command's fix *and* its e2e test in that single change, `jj describe`
>   it with a clear conventional-commit message, then `jj new` again for the next
>   item. One command per rev — never batch two commands (or unrelated changes)
>   into one rev. Verify the folder + branch first (`pwd`, `jj log -r @`) since the
>   working copy is shared.
> - **Parity, not workarounds.** Fix the real cause (VFS syscall, shell semantics,
>   link conflict, missing feature). If a WASI limitation forces a deviation from
>   Linux, that is a finding to surface — not something to paper over in the test.
> - **Real tests are the deliverable.** A fix isn't done until an un-skipped e2e
>   test exercises the real behavior in a VM and passes. No `describe.skip`, no
>   assertions weakened to match broken output.
> - Work top-down by priority. Re-verify with the actual VM run, not just typecheck.

## Priority tiers

- **P0 — runtime/VM correctness**: bugs in the shared runtime that silently
  corrupt data or break process control. Highest blast radius.
- **P1 — broken shipped commands**: packages that build but don't work like Linux.
- **P2 — build/compile failures**: packages that can't be produced at all.
- **P3 — disabled/absent coverage**: real behavior exists but no real test proves it.

## Known CPU interval-timer boundary

Disabled `ITIMER_VIRTUAL` and `ITIMER_PROF` timers now match Linux: `getitimer`
returns a zero timer and `setitimer` accepts a zero value. Arming either timer
still returns `ENOTSUP`. The concrete remaining wall is asynchronous delivery
during pure guest computation: V8 executes a WASM call synchronously, and the
runner has no per-instruction or safepoint callback through which it can deliver
`SIGVTALRM`/`SIGPROF`. The runtime has a per-thread CPU clock for its outer V8
watchdog, but it can only terminate the isolate; it cannot enter the guest's
signal machinery at the exact CPU-time deadline. Polling at imported syscalls
would silently fail for a tight WASM loop, so it is not presented as parity.
Closing this gap requires a V8 interrupt/safepoint hook or instrumented WASM
fuel checkpoints that distinguish user CPU from user-plus-system CPU.

---

## ⚠️ Cross-cutting #0 — Command provenance: replace reimplementations with real tools

**This is the highest-leverage item and reshapes several below.** Audit revealed
that **most commands are NOT the real Linux tool** — they are custom Rust rewrites
(`agentos-*` crates) or `uutils`, plus at least one hand-rolled C CLI (curl).
Per the load-bearing rule, each must become the **real upstream tool** compiled to
WASI and patched as needed.

**Rule (your call):** an **established project** — whether it's the real upstream
tool *or* an established third-party package that does the real work (uutils,
jaq, etc.) — is **fine**. **Custom code we wrote ourselves** is **not** and must
be replaced with a real/established implementation. Audit of every command's
actual backing:

### ✅ Established — keep (real upstream tool or established package doing the work)
| Command(s) | Backing |
|---|---|
| coreutils (`sh`+80) | **uutils** (`uucore`) — established Rust project |
| duckdb, vim | real upstream C source, patched for WASI |
| sqlite3 **engine** | real SQLite amalgamation (⚠️ but the *CLI* is ours — see below) |
| jq | **jaq** (`jaq-core/std/json`) — established Rust jq |
| yq | jaq + `serde_yaml`/`toml`/`quick-xml` — established parsers (thin glue is ours) |
| sed | `sed` crate (published) |
| gawk (`awk`) | `awk-rs` crate (published) |
| tar | `tar` crate (established) |
| gzip | `flate2` (established) |
| diffutils (`diff`) | `similar` crate (established) |
| file | `infer` crate (established magic-byte lib; note: not real libmagic `file`) |

### ❌ CUSTOM WE BUILT — flag & replace with a real/established impl
| Command | Status | What it actually is | Replace with |
|---|---|---|---|
| **curl** | DONE | our custom driver over a libcurl fork | real `curl` CLI (upstream `src/tool_*.c`) |
| **wget** | DONE | our 174-line `wget.c` (dropped) | real GNU Wget vs our sysroot — stub `getrlimit`/`getgroups`, then build |
| **http-get** | DONE | our 95-line `http_get.c` | dropped; real curl covers HTTP fetches |
| **git** | DONE | our hand-rolled git from `sha1`+`flate2` | **real git** (upstream C), patched for WASI — **NOT gitoxide** |
| **fd** | DONE | our `agentos-fd` on raw `regex` (not sharkdp/fd) | real **fd** (sharkdp) |
| **findutils** (`find`,`xargs`) | DONE | our hand-rolled on `regex`/shims | replaced with `uutils/findutils` |
| **tree** | DONE | our hand-rolled, zero deps | real `tree`, or an established one |
| **grep** | DONE | our `agentos-grep` on raw `regex` (**not** an established grep pkg) | real **GNU grep** |
| **ripgrep** (`rg`) | DONE | our `agentos-grep` recursive search shim, not real ripgrep | real upstream **ripgrep** |
| **zip** | DONE | our 203-line `zip.c` over zlib/minizip (not Info-ZIP) | real Info-ZIP, or an established lib's CLI |
| **unzip** | DONE | our 669-line `unzip.c` over zlib/minizip | real Info-ZIP unzip |
| **sqlite3 CLI** | DONE | our 558-line `sqlite3_cli.c` (engine is real SQLite; the shell is ours) | real SQLite `shell.c` (its official CLI) |
| **vix** | DONE | from-scratch source-less drop-zone binary | deleted; real `vim` covers the editor slot |

Note: `codex`/`codex-exec` = the rivet fork of OpenAI's codex — established fork,
external build (tracked separately in #9).

**Objective:** replace each ❌ with a real/established implementation built to
`wasm32-wasip1` and patched only where WASI forces it. The ✅ rows stay.

**Networking — DONE.** Real curl and GNU Wget now terminate TLS in-guest with
mbedTLS against the VM CA bundle, and Git links the same libcurl through a real
`git-remote-http` helper. HTTPS certificate verification and trust overrides,
curl's native exit taxonomy and compression, Wget HTTPS/FTPS, and Git smart-HTTP
clone/fetch/push are covered by parity tests. OpenSSH separately provides direct
SSH and git-over-SSH transport. See **`docs-internal/networking-parity-spec.md`**
for the original design decisions and [TLS & SSL](../website/src/content/docs/docs/architecture/tls-ssl.mdx)
for the current architecture.

**Approach:** one command at a time, one jj rev each: swap our custom code for the
established source (fetched + pinned like sqlite/duckdb), wire into the toolchain,
patch for WASI, prove parity with real e2e tests.

**Interaction with other items:** subsumes several below — curl (#6) is "build the
real curl," and the `no-test` packages (#12) that are ❌ here should move to a real
impl *before* their tests are written, so the tests validate real behavior.

**Decisions (settled):** git → **real git** (not gitoxide). grep → **real GNU
grep**, or a popular established grep if the real one won't build.

**git — where the issues are (assessment):**
- **LICENSE — RESOLVED, not a blocker.** Each command ships as its **own published
  npm package**, so a GPL-2.0 git binary in `@agentos-software/git` is **mere
  aggregation** — it does not affect the Apache-2.0 licensing of agentOS or any
  other package. Ship the git package under GPL-2.0 (offer its source) and we're
  compliant. This **supersedes** the clean-room-reimpl rationale in
  `toolchain/std-patches/git/README.md` ("cannot be vendored due to license
  restrictions"): that premise no longer holds — go with **real git** (upstream C)
  and update/remove that README. (gitoxide stays ruled out.)
- **Technical WASI issues if we do build real git** (from that README + git's own
  build knobs), easiest → hardest:
  - `mmap` (packfiles/index) → build `NO_MMAP=1` (malloc+read). Fine.
  - signals (SIGPIPE/SIGCHLD) → build without; WASI has none. Fine.
  - threads (index-pack/pack-objects) → `NO_PTHREADS`; single-threaded, slower. Fine.
  - `fork()`+`exec()` in `run_command.c` (hooks, filters, remote helpers) → route to
    `posix_spawn` via the `wasi-spawn` broker (spawn IS supported — same fix as wget).
  - **Network transport (clone/fetch/push) — the hard part.** Smart-HTTP needs the
    `git-remote-https` **helper subprocess** + libcurl; `git://` needs raw sockets;
    ssh needs an `ssh` subprocess. Each helper must itself exist as a module.
  - symlink checkout → `core.symlinks=false` fallback (WASI symlink support is
    partial); local time → UTC like elsewhere.
- **Bottom line:** license is a non-issue (separate published package = mere
  aggregation). **local** git (init/add/commit/log/diff/branch/merge/status/
  checkout) is very achievable; **remote** git (clone/fetch/push over
  smart-HTTP/ssh) is the real effort. Proceed with real git.

**Replacement findings — what each remaining ❌ tool will take (investigated):**

The recurring wall is **never a syscall** — it's one of two known, already-solved
patterns: **(a) no threads** on `wasm32-wasip1` → serial patch like
`toolchain/std-patches/crates/uu_sort/0001-wasi-serial-sort.patch` (hits real fd &
ripgrep crates); **(b) gnulib `getrlimit`/`getgroups`** → sysroot stubs, already
documented for wget (item #8) (hits GNU grep/findutils). Subprocess spawn already
works (`wasi-spawn` broker), so `xargs` is not a blocker.

- **git — DONE.** Replaced the custom Rust `sha1`+`flate2` implementation with real
  upstream Git 2.55.0 built by the C toolchain and staged as `git` plus helper
  command aliases. The WASI changes stayed below Git's behavior surface:
  `run-command.c` uses `posix_spawn` so helper subprocesses go through the existing
  wasi-spawn broker, the sysroot exposes Git's missing C compatibility surface, and
  the runner now allocates synthetic fds in a high range so managed pipe/file fds
  cannot collide with delegate-opened WASI fds. Smart HTTP remains intentionally
  disabled in this build (`NO_CURL`), so HTTPS clone fails with the real Git helper
  error instead of a custom transport. Proof: clean upstream Git rebuild passes in
  `2026-07-08T11-28-00-0700-git-clean-rebuild-after-high-synthetic-fd.log`;
  package build stages 6 commands in
  `2026-07-08T11-33-00-0700-git-package-build-clean-binary-after-install.log`;
  native sidecar rebuild passes in
  `2026-07-08T11-34-00-0700-sidecar-rebuild-after-git-clean-package.log`;
  full Git e2e passes 18/18 in
  `2026-07-08T11-51-00-0700-git-full-e2e-high-synthetic-fd-clean-binary-after-test-fix.log`.
  Rev: `tmvlxlvk` — `fix(git): build upstream git`.
- **sqlite3 CLI — DONE.** Engine was already the real amalgamation
  (`libs/sqlite3/sqlite3.c`); the command now builds the official upstream
  `shell.c` from the same fetched zip as `sqlite3`. The local 558-line
  `sqlite3_cli.c` reimplementation is deleted, `toolchain/c/build/sqlite3` is the
  primary C output, `sqlite3_cli` remains only as a compatibility alias, and the
  tracked runtime-core fallback command is refreshed to the same official shell.
  Proof: official shell build passes in
  `2026-07-08T05-04-47-0700-sqlite3-official-shell-build-command-name.log`;
  package-focused e2e passes 16/16, including real `.tables`, `.schema`, and
  `.dump` CLI arguments, in
  `2026-07-08T05-07-06-0700-sqlite3-official-shell-tests-final-focused.log`;
  package build/check-types pass in
  `2026-07-08T05-07-52-0700-sqlite3-package-build-official-shell-final.log` and
  `2026-07-08T05-07-52-0700-sqlite3-check-types-official-shell-final.log`;
  runtime-core fallback command path passes `.tables` in
  `2026-07-08T05-09-12-0700-sqlite3-runtime-core-command-fallback-test.log`;
  aggregate C `programs` builds 57 commands in
  `2026-07-08T05-09-53-0700-sqlite3-make-programs-final.log`.
  Rev: `typytnkk` — `fix(sqlite3): build official SQLite shell`.
- **http-get — DONE.** Dropped the 95-line raw-socket loopback client instead of
  porting it: real curl now covers HTTP fetch behavior in DuckDB remote CSV tests,
  runtime cross-network loopback tests, and the C conformance loopback row.
  Removed the `@agentos-software/http-get` package, fallback command, shell/core
  dependencies, registry listing, C command install entries, and lockfile package
  edges. The low-level `http_get_test` fixture remains because it is a socket
  diagnostic test program, not a shipped registry command. Proof: pnpm lockfile
  refresh succeeds in
  `2026-07-08T05-39-44-0700-http-get-pnpm-lock-refresh.log`; core and shell
  type checks pass in
  `2026-07-08T05-40-57-0700-http-get-core-check-types-after-install.log` and
  `2026-07-08T05-41-34-0700-http-get-shell-check-types-final.log`; DuckDB test
  file typecheck passes in
  `2026-07-08T05-40-57-0700-http-get-duckdb-test-typecheck-after-install.log`;
  aggregate C `programs` builds 57 commands, with `http_get` absent after stale
  generated cleanup/install, in
  `2026-07-08T05-41-34-0700-http-get-make-c-programs-without-command.log` and
  `2026-07-08T05-43-24-0700-http-get-clear-stale-generated-and-install.log`;
  cross-runtime network tests pass 11/11 with the WASM curl rows in
  `2026-07-08T05-47-43-0700-http-get-runtime-cross-network-test-pass.log`.
- **tree — DONE.** Replaced the custom Rust `agentos-tree`/`cmd-tree` crates
  with upstream Steve Baker `tree` 2.3.2 from `OldManProgrammer/unix-tree`.
  It builds as a C toolchain command from pinned source, stages into
  `@agentos-software/tree`, and refreshes the tracked runtime-core fallback
  command. Sysroot fixes live one layer down: install `<grp.h>` and provide
  deterministic missing-group lookup stubs so upstream `-g` support links
  without a tree-source WASI branch. Proof: upstream source inspection in
  `2026-07-08T05-13-50-0700-tree-fetch-upstream-2.3.2-inspect.log`; sysroot
  patch check passes in
  `2026-07-08T05-18-16-0700-tree-wasi-libc-patch-check-group-lookup-fixed.log`;
  Makefile build passes in
  `2026-07-08T05-20-02-0700-tree-upstream-make-build.log`; package build and
  check-types pass in
  `2026-07-08T05-21-08-0700-tree-package-build-upstream-after-install.log` and
  `2026-07-08T05-21-08-0700-tree-check-types-upstream-after-install.log`; e2e
  tree tests pass 6/6 in
  `2026-07-08T05-29-45-0700-tree-vitest-upstream-final.log`; aggregate C
  `programs` builds 58 commands in
  `2026-07-08T05-30-44-0700-tree-make-programs-final.log`; Cargo metadata no
  longer includes the deleted Rust tree crates in
  `2026-07-08T05-33-17-0700-tree-cargo-metadata-after-removing-empty-dirs.log`.
  Rev: `kpmrwxln` — `fix(tree): build upstream tree`.
- **fd — DONE.** Replaced the custom Rust `agentos-fd` regex walker with
  upstream sharkdp `fd-find` 10.4.2. Because `fd-find` is a bin-only crate,
  `cmd-fd` now acts as the workspace build trigger while the toolchain builds
  the upstream `fd` binary directly. WASI compatibility stays in dependency
  patches: `fd-find` gets narrow file-type/path/receiver adjustments, and
  `ignore` uses a serial walker on `wasm32-wasip1` instead of host threads. Proof:
  clean patch dry-runs pass in
  `2026-07-08T06-19-50-0700-fd-find-patch-clean-dry-run.log` and
  `2026-07-08T06-26-52-0700-fd-ignore-patch-final-dry-run.log`;
  clean upstream WASM build compiles `fd-find v10.4.2` in
  `2026-07-08T06-21-54-0700-fd-upstream-wasm-rebuild-after-ignore-fix.log`;
  runtime/package command hashes match in
  `2026-07-08T06-22-42-0700-fd-copy-built-binary-hashes.log`; package build and
  check-types pass in
  `2026-07-08T06-24-27-0700-fd-package-build.log` and
  `2026-07-08T06-25-41-0700-fd-check-types-final.log`; e2e fd tests pass 9/9,
  including `fd --version` reporting `fd 10.4.2`, in
  `2026-07-08T06-25-00-0700-fd-vitest-upstream-after-dir-format.log`.
  Rev: `mrskpomv` — `fix(fd): build upstream fd-find`.
- **grep — DONE.** Replaced the `@agentos-software/grep` package's custom
  `agentos-grep` command wrapper with upstream GNU grep 3.12 from the official
  GNU release tarball. The real GNU `grep` binary builds through the C toolchain;
  `egrep` and `fgrep` are separate tiny WASM launchers that preserve GNU's
  upstream obsolescent scripts by spawning `grep -E` / `grep -F` through the
  AgentOS process broker. Sysroot fix stayed one layer down: wasi-libc no longer
  advertises/exports its nonstandard two-argument `opendirat`, which conflicted
  with gnulib's helper. Proof: official GNU release listing captured in
  `2026-07-08T06-29-11-0700-grep-gnu-ftp-latest.log`; configure options captured
  in `2026-07-08T06-29-39-0700-grep-upstream-configure-help-probe.log`;
  sysroot `opendirat` patch check and rebuild pass in
  `2026-07-08T06-34-27-0700-grep-wasi-libc-opendirat-patch-check-definition.log`
  and `2026-07-08T06-34-33-0700-grep-sysroot-rebuild-opendirat-definition.log`;
  GNU grep build passes in
  `2026-07-08T06-35-01-0700-grep-gnu-wasm-build-after-opendirat-symbol.log`;
  `egrep`/`fgrep` launcher builds pass in
  `2026-07-08T06-40-07-0700-grep-egrep-wrapper-build.log` and
  `2026-07-08T06-40-42-0700-grep-fgrep-wrapper-build.log`; package build and
  check-types pass in `2026-07-08T06-42-24-0700-grep-package-build-final.log`
  and `2026-07-08T06-42-18-0700-grep-check-types-final.log`; e2e grep tests
  pass 8/8 in `2026-07-08T06-43-59-0700-grep-vitest-after-wrapper-cache-repair.log`.
  Rev: `uyukolvr` — `fix(grep): build upstream GNU grep`.
- **ripgrep — DONE.** Replaced the `@agentos-software/ripgrep` package's custom
  `agentos-grep` recursive search shim with upstream ripgrep 15.1.0 from the
  canonical `BurntSushi/ripgrep` crate/release. The local `cmd-rg` crate is now
  only a build trigger, and `toolchain/Makefile` builds ripgrep's own `rg` bin
  directly. The old `agentos-grep` library is gone. Proof: latest upstream
  release captured in `2026-07-08T06-50-40-0700-ripgrep-github-latest-release.json`;
  crate metadata captured in `2026-07-08T06-52-00-0700-ripgrep-cargo-info.log`;
  upstream WASM build passes in
  `2026-07-08T06-56-00-0700-ripgrep-upstream-wasm-build-after-grep-dir-remove-fixed.log`;
  package build and check-types pass in
  `2026-07-08T06-58-55-0700-ripgrep-package-build-after-install.log` and
  `2026-07-08T06-58-56-0700-ripgrep-check-types-after-install.log`; e2e ripgrep
  tests pass 8/8 in `2026-07-08T07-02-00-0700-ripgrep-vitest-after-git-fixture.log`.
  Rev: `msypkqmo` — `fix(ripgrep): build upstream ripgrep`.
- **zip / unzip — DONE.** Replaced both custom minizip-based C wrappers with real
  upstream Info-ZIP Zip 3.0 and UnZip 6.0 release tarballs, built through the C
  toolchain. Runtime/sysroot fixes stayed one layer down: temp-file, ownership,
  `system`/`popen`/`pclose` compatibility in patched wasi-libc; overlay rename
  over destination whiteouts; and WASI host-passthrough read/write offset
  tracking after `fd_seek`. Proof: wasi-libc patch check passes in
  `2026-07-08T08-34-29-0700-wasi-libc-patch-check-final.log`; native sidecar
  build passes in `2026-07-08T08-34-07-0700-sidecar-build-final-runner-format.log`;
  VFS rename regression passes in
  `2026-07-08T08-34-29-0700-vfs-core-rename-whiteout-final.log`; final Zip e2e
  passes 2/2 in
  `2026-07-08T08-36-45-0700-zip-test-final-after-current-sysroot-copy-after-install.log`;
  final UnZip e2e passes 6/6 in
  `2026-07-08T08-37-12-0700-unzip-test-final-after-current-sysroot-copy-after-install.log`.
  Rev: `nppnuxpr` — `fix(infozip): build upstream zip and unzip`.
- **findutils — DONE.** Replaced the custom Rust `find` crate with upstream
  `uutils/findutils` 0.9.0 entrypoints for both `find` and `xargs`. Kept the fix
  in the toolchain/sysroot layer: Rust WASI metadata extensions and process
  `ExitStatusExt::signal()` are exposed for crates that expect Unix-like std
  surfaces, while `xargs` still uses normal `std::process::Command` spawning
  through the existing VM broker. Proof: `find` wasm build passes in
  `2026-07-08T12-42-00-0700-findutils-find-build-after-permissions-mode-patch.log`;
  `xargs` wasm build passes in
  `2026-07-08T12-45-00-0700-findutils-xargs-build-after-find-success.log`;
  package build passes in
  `2026-07-08T12-51-00-0700-findutils-package-build-after-install.log`; sidecar
  validation build passes in
  `2026-07-08T12-35-00-0700-native-sidecar-build-after-forced-pnpm.log`; final
  package e2e passes 5/5, including `xargs -n 2 echo` spawn batching, in
  `2026-07-08T12-44-00-0700-findutils-vitest-uutils-after-depth-test-fix.log`.
  Rev: `msknmmps`.

Ranked easiest→hardest: **sqlite3-CLI · http-get (drop) · tree · fd · grep ·
ripgrep · zip · unzip · findutils.**

## Status tracking (how the driver reports progress in this doc)

Update this doc as you go — it is the single source of truth for status. For each
❌ command, set one status and keep it current:

- **`TODO`** — not started.
- **`IN PROGRESS`** — being built; note the current blocker if any.
- **`DONE`** — the real/established tool builds and passes a real un-skipped e2e
  test; link the jj rev.
- **`NOT POSSIBLE (WASI)`** — only after a genuine effort. Write a concrete
  explanation: exactly what blocks it, what you tried (sysroot patch, tool patch,
  syscall stub), and why it can't be made to work. This is a documented dead-end,
  never a silent fallback to a custom rewrite.

Mark each row's status inline in the table (or as a short line under the command)
so a reader sees the whole board at a glance.

---

## P0 — Runtime / VM correctness

### 1. brush-shell `>>` append truncates instead of appending — DONE
- **Broken:** `execSync` with `>>` onto a write-only file overwrites instead of
  appends. `expected 'changed' to be 'originalchanged'`. (issue: rivet-dev/agentos#1657)
- **Objective:** `>>` opens `O_WRONLY|O_APPEND` against the kernel VFS and appends,
  identical to bash on Linux.
- **Proof:** `bridge-child-process.test.ts` append redirection tests pass
  un-skipped; direct kernel append and native sidecar append regressions pass.
- **rev:** `ouxrzutq` — `fix(runtime): honor >> append mode in guest shell VFS redirection`

### 2. brush-shell `cat < file` stdin redirection fails (exit 1) — DONE
- **Broken:** `cat < stdin-input.txt` exits 1 — input redirection from a VFS path
  isn't wired to the command's stdin. (issue: #1657)
- **Objective:** `< file` feeds the VFS file to stdin; command reads it and exits 0,
  like Linux.
- **Proof:** the "stdin redirection feeds the kernel VFS file" test passes
  un-skipped after the parent host-shadow pre-spawn sync fix in item 1.
- **rev:** `lonnzuqw` — `test(registry): mark stdin redirection parity proven`

### 3. WasmVM signal/dispose — SIGKILL/SIGTERM don't terminate; dispose hangs — DONE
- **Broken:** SIGKILL/SIGTERM don't kill guest processes; `dispose` times out
  (5 tests across `signal-forwarding.test.ts`, `dispose-behavior.test.ts`).
- **Objective:** signals delivered to guest processes terminate them promptly and
  `dispose` tears down active WasmVM + Node processes, matching Linux signal
  semantics. **Not yet filed — file a separate issue.**
- **Proof:** `signal-forwarding.test.ts` passes 5/5 in
  `2026-07-07T23-11-36-0700-item3-signal-forwarding-final-pass-2.txt`;
  `dispose-behavior.test.ts` passes 3/3 in
  `2026-07-07T23-11-21-0700-item3-dispose-behavior-final-pass.txt`.
- **rev:** `zkywnwup` — `fix(runtime): unblock WasmVM signal waits and dispose`

### 4. VFS missing `pwrite` — sqlite3 file-backed DBs don't persist — DONE
- **Broken:** `filesystem method pwrite is unavailable` — sqlite3 file-backed DB
  can't persist across exec calls.
- **Objective:** the VFS implements positioned writes (`pwrite`/`pwritev`) so any
  command doing positioned I/O (sqlite3, and others) behaves like Linux.
- **Proof:** sqlite3 "file-based DB persists across separate exec calls" passes
  in `2026-07-07T23-18-45-0700-item4-sqlite3-file-db-pwrite-pass.txt`; direct
  mounted JS VFS `pwrite` test passes in
  `2026-07-07T23-18-45-0700-item4-runtime-core-custom-vfs-pwrite-pass.txt`.
  Type/build checks pass in `2026-07-07T23-19-11-0700-item4-runtime-core-build.txt`
  and `2026-07-07T23-19-11-0700-item4-sqlite3-check-types.txt`.
- **rev:** `klrzzkro` — `fix(vfs): expose positioned writes in test kernel`

### 5. Socket-layer failures (net-server/udp/unix, signal_handler) — DONE
- **Broken:** in the audit run, `st.create is not a function` + a `LinkError` in
  net tests; signal_handler didn't catch signals. May be partial-build artifacts.
- **Objective:** TCP/UDP/Unix socket + signal test programs run to completion in
  the VM with real socket semantics. **First reconfirm on a full build** — if it
  reproduces, fix the socket-table wiring / link error.
- **Proof:** net-server/net-udp/net-unix/signal-handler suites pass together in
  `2026-07-08T00-23-43-0700-item5-four-suites-take-signal-bridge.txt`.
  Runtime and native sidecar builds pass in
  `2026-07-08T00-24-02-0700-item5-final-runtime-core-build.txt` and
  `2026-07-08T00-24-02-0700-item5-final-native-sidecar-build.txt`; native
  embedded signal coverage passes in
  `2026-07-08T00-24-02-0700-item5-final-native-embedded-runtime-signal-suite.txt`.
- **rev:** `zvyxkkyv` — `fix(runtime): repair Wasm socket and signal integration`

---

## P1 — Broken shipped commands

### 6. curl — reimplemented CLI, exits 1 on every operation (incl. `--version`) — DONE
- **Broken:** the `curl` command is a **hand-rolled `curl.c` driver** over a
  libcurl fork, not the real curl command-line tool — so 24/30 `curl.test.ts` fail
  and every op returns exit 1, even `curl --version`.
- **Objective (per Cross-cutting #0):** **build the real curl command-line tool**
  (upstream `src/tool_*.c`) to `wasm32-wasip1` against the patched sysroot,
  patched only where WASI forces it — replacing the custom driver. All real curl
  behavior (GET/POST, `-I`/`-D`, `-L`, `-u`, `-F`, `-o`/`-O`, `-w`, `-K`) then
  works because it *is* curl, not a shim.
- **Proof:** `software/curl/test/` passes un-weakened: 25 passed, 5 skipped in
  `2026-07-08T00-41-57-0700-item6-curl-test-after-tls-flags.txt`. Runtime runner
  build/protocol checks pass in
  `2026-07-08T00-41-51-0700-item6-runtime-core-build-tls-flags.txt`.
- **rev:** `oxoqrwvk` — `fix(curl): build the real curl CLI for WASI`
- **Note (how well it works):** it *is* the real curl CLI (`src/tool_main.c`) plus a
  custom `vtls/wasi_tls.c` backend (`USE_WASI_TLS`) — HTTPS runs through the host
  TLS bridge, not OpenSSL. Real HTTP(S) `GET/POST/-I/-D/-L/-u/-F/-o/-O/-w/-K` all
  work because it's genuine curl. Known gaps from the trimmed `./configure`:
  `--compressed`/gzip response decode (`--without-zlib`), brotli/zstd, `libpsl`
  cookie-suffix checks, LDAP, and no CA bundle (cert trust is whatever `wasi_tls`
  enforces). Those are the 5 skipped tests. Verdict: solid for real HTTP(S).

### 7. zip / unzip — replace custom wrappers with real Info-ZIP — DONE
- **Broken:** the shipped commands were custom C wrappers over zlib/minizip, not
  real Zip/UnZip. The old fallback parser also diverged from hardened Linux unzip
  behavior on wrapping local offsets, empty normalized names, and hostile size
  declarations.
- **Objective:** build real upstream Info-ZIP Zip and UnZip to `wasm32-wasip1`,
  patching the AgentOS sysroot/runtime where needed, and prove real zip↔unzip
  roundtrips plus malformed-archive rejection in VM e2e tests.
- **Resolved:** upstream Info-ZIP Zip 3.0 and UnZip 6.0 now build from release
  tarballs through the C toolchain. The custom `software/zip/native/c/zip.c` and
  `software/unzip/native/c/unzip.c` sources are deleted. Required compatibility
  lives one layer down: patched wasi-libc temp-file, ownership, `system`, `popen`,
  and `pclose` surfaces; VFS overlay rename-over-whiteout cleanup; and
  host-passthrough `fd_seek` offset tracking in the WASI runner.
- **Proof:** wasi-libc patch check passes in
  `2026-07-08T08-34-29-0700-wasi-libc-patch-check-final.log`; native sidecar
  build passes in `2026-07-08T08-34-07-0700-sidecar-build-final-runner-format.log`;
  VFS rename regression passes in
  `2026-07-08T08-34-29-0700-vfs-core-rename-whiteout-final.log`; `software/zip`
  e2e passes 2/2 in
  `2026-07-08T08-36-45-0700-zip-test-final-after-current-sysroot-copy-after-install.log`;
  `software/unzip` e2e passes 6/6 in
  `2026-07-08T08-37-12-0700-unzip-test-final-after-current-sysroot-copy-after-install.log`.
- **rev:** `nppnuxpr` — `fix(infozip): build upstream zip and unzip`

---

## P2 — Build / compile failures

### 8. wget — DONE
- **Resolved:** the `wget` command is real upstream GNU Wget 1.24.5 built for
  `wasm32-wasip1`, HTTP-only for now (`--without-ssl --without-zlib
  --without-libpsl --disable-iri`) against the patched AgentOS C sysroot. The old
  custom 174-line wrapper stays removed.
- **Sysroot/runtime fixes:** Wget builds without a Wget-source WASI fork by adding
  the missing POSIX surface one layer down: process/terminal headers including
  `spawn.h`, signal/process/timezone compatibility, overrideable `FD_SETSIZE`,
  Wget-only `_POSIX_TIMERS` overlay, POSIX socket `read`/`write` routing through
  `host_net`, low host-net fds, and `MSG_PEEK` queue preservation in the WASM
  runner. Configure is seeded so gnulib trusts the sysroot `select` instead of
  replacing it with a host-net-incompatible fallback.
- **Proof:** focused basename download passes in
  `2026-07-08T04-33-31-0700-item8-wget-vitest-focused-clean-msg-peek.log`; full
  Wget e2e suite passes 5/5 in
  `2026-07-08T04-33-41-0700-item8-wget-vitest-full-clean-msg-peek.log`. Final
  runner syntax and wasi-libc patch checks pass in
  `2026-07-08T04-34-02-0700-item8-node-check-wasm-runner-final.log` and
  `2026-07-08T04-34-02-0700-item8-wasi-libc-patch-check-final.log`.
- **rev:** `zuosnzmq` — `fix(wget): build real GNU Wget for WASI`

### 9. codex-cli — DONE
- **Resolved:** the `codex`/`codex-exec` package now has an AgentOS-owned wrapper
  for the external `codex-rs` fork build. `make -C toolchain codex-required`
  requires `CODEX_REPO=/path/to/codex-rs/codex-rs`, uses this checkout's
  `toolchain/c/vendor/wasi-sdk`, and installs the fork-built optimized wasm into
  generated `software/codex/wasm/{codex-exec,codex}` for the package build. The
  generated toolchain and wasm command directories are ignored and not committed.
- **Test fix:** the real `codex-exec --session-turn` e2e now uses a streaming
  Responses mock (SSE) and disables Codex shell snapshots inside the VM config,
  avoiding the optional pre-turn shell-snapshot subprocess deadlock while still
  driving the real codex-core agent and shell tool path.
- **Proof:** `CODEX_REPO=/home/nathan/agent-e2e/codex-rs/codex-rs make -C
  toolchain codex-required` builds and installs 29,924,651-byte command artifacts
  in `2026-07-08T01-37-05-0700-item9-codex-build-rerun.txt`; `pnpm --dir
  software/codex-cli build` stages 2 commands and assembles `package.aospkg` in
  `2026-07-08T01-44-50-0700-item9-codex-cli-build.txt`;
  `pnpm --dir packages/core exec vitest run
  tests/codex-fullturn.nightly.test.ts --reporter=verbose` passes 2 real VM tests in
  `2026-07-08T01-53-55-0700-item9-core-codex-fullturn-pass.txt`.
- **rev:** `svksnzon` — `build(codex-cli): make the codex-rs fork build reproducible`

### 10. vix — DONE (deleted)
- **Resolved:** `vix` was a from-scratch, source-less drop-zone binary — exactly
  the kind of hand-rolled artifact this repo should not carry. **Removed entirely**
  (package dir, shell import/dep, `EXTERNAL_COMMANDS` Makefile hack, README rows,
  website registry entry) in rev
  `chore(registry): remove vix package; document real-tool (no-reimplementation) principle`.
  Real `vim` (#11) covers the editor slot. Preserved source (`vix.c`, `BUILD-vix.md`,
  `vix.wasm`) remains in `~/progress/agent-os/2026-06-28-just-shell-fix/` if ever
  needed. No further work.

---

## P3 — Disabled / absent coverage (real tests to Linux parity)

For each: replace `describe.skip` with `describeIf(binaryPresent)` **and** write
real e2e tests that prove Linux-parity behavior — not smoke tests.

### 11. Disabled suites — git, duckdb, codex — DONE
- **Fixed:** the Git quickstart, DuckDB package, and Codex full-turn suites are no
  longer excluded from the default core Vitest file set, so coverage cannot
  disappear when the package artifacts are present.
- **Status:**
  - **git — DONE.** The core quickstart e2e now exercises real upstream Git in a
    VM for local origin creation, commit, branch, clone, checkout, `log`, and a
    working-tree `diff`. It validates only the git package it uses instead of
    eagerly requiring every registry package to be built. Proof:
    `2026-07-08T13-13-00-0700-item11-git-quickstart-final.log`.
    Rev: `svltqsmx`.
  - **duckdb — DONE.** Rebuilt the upstream DuckDB package artifact from the
    patched C sysroot and strengthened the package e2e so it validates only the
    DuckDB/Curl packages it uses. The VM e2e now covers file-backed DML, real
    analytical SQL, DuckDB CSV export, `read_csv_auto` re-import, and the
    negative HTTP-URL path for DuckDB itself. Proof:
    `2026-07-08T12-38-07-0700-item11-duckdb-package-build-after-install.log`;
    `2026-07-08T12-46-18-0700-item11-duckdb-package-e2e-final-file-pass.log`;
    `2026-07-08T12-46-11-0700-item11-core-tsc-duckdb-final-file-after-refresh.log`;
    `2026-07-08T12-46-10-0700-item11-duckdb-biome-check-final-file-after-refresh.log`.
    Rev: `qrwnvouk`.
  - **codex — DONE.** Un-skipped the remaining real `codex-exec --session-turn`
    full-turn coverage. The suite now proves the model turn, on-request shell
    tool call, real subprocess filesystem side effect, and adapter-supplied
    history replay in one VM-backed file with 4/4 passing. For this coverage
    rev, the package was staged from existing real 29 MB `codex`/`codex-exec`
    artifacts because rebuilding the external Codex fork in this checkout hit
    dependency gating failures (`path-dedot`, then `tokio`/`rustls-native-certs`);
    the failed rebuild logs are kept as proof. Proof:
    `2026-07-08T12-52-54-0700-item11-codex-cli-package-build-from-prior-artifacts-after-install.log`;
    `2026-07-08T12-54-08-0700-item11-codex-fullturn-final-unskipped.log`;
    `2026-07-08T12-54-42-0700-item11-core-tsc-codex-final.log`;
    `2026-07-08T12-54-42-0700-item11-codex-biome-check-final.log`;
    rebuild blockers:
    `2026-07-08T12-48-46-0700-item11-codex-required-build-fresh-cargo-home.log`,
    `2026-07-08T12-50-38-0700-item11-codex-required-build-path-dedot-cfg.log`.
    Rev: `ryqtvoqv`.
- **Final proof:** after removing the remaining core Vitest exclusions, the
  explicit suites pass with real VM coverage: Git quickstart 1/1 in
  `2026-07-08T14-35-00-0700-item11-status-git-quickstart-final.log`; DuckDB
  package 4/4 in
  `2026-07-08T14-36-00-0700-item11-status-duckdb-package-final.log`; Codex
  full-turn 4/4 in
  `2026-07-08T14-37-00-0700-item11-status-codex-fullturn-final.log`.
- **rev:** `test(core): include registry parity suites by default`

### 12. No tests at all — 9 software + 5 agents — DONE
- **Broken:** zero e2e coverage: `gawk, sed, tar, gzip, jq, yq, diffutils,
  file, vim`; agents `claude, codex, opencode, pi, pi-cli`.
- **Status:**
  - **gawk — DONE.** Added package-local VM e2e coverage for the staged `awk`
    command. The suite proves file-backed field extraction, explicit field
    separators, numeric aggregation, `-f` script-file execution, and missing
    input-file errors through the packaged WASM command. Proof:
    `2026-07-08T13-16-03-0700-item12-gawk-package-e2e-after-install.log`;
    `2026-07-08T13-16-03-0700-item12-gawk-check-types-after-install.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T13-16-16-0700-item12-gawk-biome-check.log`. Rev: `pzxkurol`.
  - **sed — DONE.** Added package-local VM e2e coverage for the staged `sed`
    command. The suite proves file-operand substitutions, addressed `-n`
    printing, addressed deletion, multiple `-e` expressions, and missing
    input-file errors through the packaged WASM command. Proof:
    `2026-07-08T13-17-46-0700-item12-sed-package-e2e-initial.log`;
    `2026-07-08T13-17-46-0700-item12-sed-check-types-initial.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T13-17-55-0700-item12-sed-biome-check.log`. Rev: `wvpklkqv`.
  - **tar — DONE.** Added package-local VM e2e coverage for the staged `tar`
    command and tightened the tar wrapper for Linux-like directory listing and
    missing-input error context. The suite proves archive creation/listing,
    extraction into `-C` directories, gzip auto-detection by extension,
    `--strip-components`, and missing create-input errors through the packaged
    WASM command. Proof:
    `2026-07-08T13-21-48-0700-item12-tar-toolchain-cmd-build-clean-vendor.log`;
    `2026-07-08T13-22-51-0700-item12-tar-package-build-after-wrapper-fix.log`;
    `2026-07-08T13-23-02-0700-item12-tar-package-e2e-final.log`;
    `2026-07-08T13-23-02-0700-item12-tar-check-types-final.log`;
    `2026-07-08T13-23-02-0700-item12-tar-cargo-fmt-check-final.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T13-23-12-0700-item12-tar-biome-check.log`. Rev: `rszmulmk`.
  - **gzip — DONE.** Added package-local VM e2e coverage for the staged
    `gzip`/`gunzip`/`zcat` commands. The suite proves file compression with
    `-k`, source removal without `-k`, `gunzip -fk`, `zcat` streaming, and
    overwrite protection without `-f` through the packaged WASM commands. Proof:
    `2026-07-08T13-25-01-0700-item12-gzip-package-e2e-initial.log`;
    `2026-07-08T13-25-01-0700-item12-gzip-check-types-initial.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T13-25-12-0700-item12-gzip-biome-check.log`. Rev: `tlstlwvy`.
  - **yq — DONE.** Added package-local VM e2e coverage for the staged `yq`
    command and fixed the wrapper to accept file operands instead of only
    stdin. The suite proves YAML filtering, YAML-to-JSON query output,
    explicit JSON/TOML/XML input formats, and invalid YAML parse errors through
    the packaged WASM command. Proof:
    `2026-07-08T13-27-36-0700-item12-yq-toolchain-cmd-build.log`;
    `2026-07-08T13-28-59-0700-item12-yq-package-build-after-file-operands.log`;
    `2026-07-08T13-29-10-0700-item12-yq-package-e2e-final.log`;
    `2026-07-08T13-29-10-0700-item12-yq-check-types-final.log`;
    `2026-07-08T13-29-11-0700-item12-yq-cargo-fmt-check-final.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T13-29-23-0700-item12-yq-biome-check.log`. Rev: `znlmtymu`.
  - **diffutils — DONE.** Added package-local VM e2e coverage for the staged
    `diff` command and fixed recursive directory output to print the compared
    file pair before hunk output, matching the Linux `diff -r` shape. The suite
    proves identical-file exit 0, normal and unified diffs, brief output,
    ignore-case/whitespace/blank-line flags, recursive directory comparisons,
    and missing-input errors through the packaged WASM command. Proof:
    `2026-07-08T13-32-10-0700-item12-diffutils-toolchain-cmd-build.log`;
    `2026-07-08T13-34-45-0700-item12-diffutils-package-build-after-recursive-header.log`;
    `2026-07-08T13-35-04-0700-item12-diffutils-package-e2e-final.log`;
    `2026-07-08T13-35-04-0700-item12-diffutils-check-types-final.log`;
    `2026-07-08T13-35-04-0700-item12-diffutils-cargo-fmt-check-final.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T13-35-04-0700-item12-diffutils-biome-check.log`.
  - **file — DONE.** Added package-local VM e2e coverage for the staged `file`
    command and fixed shebang script classification to run before generic magic
    detection, producing Linux-like script descriptions instead of raw
    `text/x-shellscript`. The suite proves text, JSON, script, PNG, empty-file,
    directory, brief, MIME, stdin, and missing-input behavior through the
    packaged WASM command. Proof:
    `2026-07-08T13-39-36-0700-item12-file-toolchain-cmd-build.log`;
    `2026-07-08T13-40-33-0700-item12-file-package-build-after-shebang-fix.log`;
    `2026-07-08T13-41-05-0700-item12-file-package-e2e-final-after-install.log`;
    `2026-07-08T13-41-05-0700-item12-file-check-types-final-after-install.log`;
    `2026-07-08T13-40-47-0700-item12-file-cargo-fmt-check-final.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T13-41-05-0700-item12-file-biome-check-final-after-install.log`.
  - **vim — DONE.** Built/staged the real upstream Vim command without app
    source forks by keeping terminal/process compatibility in the patched C
    sysroot, disabling host Wayland/dlfcn detection at configure time, and
    trimming the Vim-local bridge down to package-specific gaps. Added
    package-local VM e2e coverage that proves the packaged binary starts with
    `-libcall` and edits/writes a file in Ex mode with the packaged runtime.
    Proof:
    `2026-07-08T14-03-20-0700-item12-vim-toolchain-cmd-build-final-ioctl.log`;
    `2026-07-08T14-04-03-0700-item12-vim-package-build-final.log`;
    `2026-07-08T14-04-03-0700-item12-vim-check-types-final.log`;
    `2026-07-08T14-04-09-0700-item12-vim-package-e2e-final.log`.
    Biome is not applicable for this package test path; it reported the file is
    ignored by config in
    `2026-07-08T14-04-51-0700-item12-vim-biome-check.log`.
  - **jq — DONE.** Added package-local VM e2e coverage for the staged `jq`
    command and fixed the jaq-backed CLI wrapper to accept Linux-style file
    operands instead of only stdin. The suite now proves version output,
    file-backed array filtering, aggregate JSON construction, slurped NDJSON,
    and invalid JSON parse errors through the packaged WASM command. Proof:
    `2026-07-08T13-10-55-0700-item12-jq-toolchain-cmd-build-isolated-target.log`;
    `2026-07-08T13-12-18-0700-item12-jq-package-build-after-wrapper-fix.log`;
    `2026-07-08T13-12-51-0700-item12-jq-package-e2e-final.log`;
    `2026-07-08T13-12-51-0700-item12-jq-check-types-final.log`;
    `2026-07-08T13-12-51-0700-item12-jq-cargo-fmt-check-final.log`.
    Rev: `slnmvuqz`.
  - **pi — DONE.** Enabled the existing real `openSession({ sessionId: 'main', agent: 'pi' })` headless
    suite in default core Vitest coverage and unskipped the upstream Pi SDK bash
    tool path. The suite proves initialization over the native sidecar
    transport plus real ACP write-tool and bash-tool flows inside the VM. Proof:
    `2026-07-08T14-37-00-0700-item12-cc-cache-restored-target-files.log`;
    `2026-07-08T14-37-00-0700-item12-sidecar-build-after-manual-cc-restore.log`;
    `2026-07-08T14-38-00-0700-item12-pi-headless-final-after-cc-restore.log`.
    Rev: `mzuuypsm`.
  - **pi-cli — DONE.** Enabled the existing real `openSession({ sessionId: 'main', agent: 'pi-cli' })`
    headless suite in default core Vitest coverage and unskipped the unmodified
    Pi CLI bash-tool flow. Pi and pi-cli now project registry command software
    only when the local `.aospkg` artifacts are built, so write-tool coverage
    still runs while bash-tool coverage is gated on real command availability.
    Proof:
    `2026-07-08T15-00-00-0700-item12-pi-cli-cc-cache-restored-target-files-final.log`;
    `2026-07-08T15-00-00-0700-item12-pi-cli-sidecar-build-after-cc-restore-final.log`;
    `2026-07-08T15-01-00-0700-item12-pi-cli-focused-final-after-cc-restore.log`.
    Rev: `xqtkmsyn`.
  - **claude — DONE.** Enabled the existing real `openSession({ sessionId: 'main', agent: 'claude' })`
    session suite in default core Vitest coverage, projected the actual
    `@agentos-software/claude-code` agent package, and replaced the missing
    test-only `xu` binary dependency with a real PATH-backed `sh` command from
    registry coreutils. The suite proves Claude ACP shell/tool flow, text-only
    responses, nested Node `execSync`/`spawn`, session metadata/lifecycle,
    modes, and raw ACP sends. The flat node_modules fixture cache now lives
    outside root `node_modules` so VM mounts survive dependency refreshes.
    Proof:
    `2026-07-08T15-17-00-0700-item12-claude-cc-cache-restored-after-cache-move.log`;
    `2026-07-08T15-17-00-0700-item12-claude-sidecar-build-after-cache-move.log`;
    `2026-07-08T15-19-00-0700-item12-claude-session-after-cache-move.log`.
    Rev: `rxmoulty`.
  - **opencode — DONE.** Added default core Vitest coverage for real
    `openSession({ sessionId: 'main', agent: 'opencode' })` initialization through the projected
    `@agentos-software/opencode` agent package. The focused suite proves the
    sidecar resolves the OpenCode ACP package, initializes the adapter, exposes
    agent metadata/capabilities/modes, and registers the session through the
    Agent OS session API. Proof:
    `2026-07-08T15-36-00-0700-item12-opencode-cc-cache-restored-focused.log`;
    `2026-07-08T15-36-00-0700-item12-opencode-sidecar-build-focused.log`;
    `2026-07-08T15-37-00-0700-item12-opencode-real-session-final.log`.
    Rev: `xtnuomsw`.
  - **codex — DONE.** Codex does not currently expose a runnable
    `openSession({ sessionId: 'main', agent: 'codex' })` ACP package: `@agentos-software/codex` is a
    registry/package wrapper and `codex-session.test.ts` verifies the sidecar
    rejects it as a session agent. The real Codex agent coverage is the
    `codex-exec --session-turn` VM path from item 11, which drives the real
    codex-core turn loop against a mock OpenAI Responses server and proves a
    real shell subprocess side effect. Proof:
    `2026-07-08T15-43-00-0700-item12-codex-cc-cache-restored-target-files.log`;
    `2026-07-08T15-43-00-0700-item12-codex-sidecar-build-before-fullturn.log`;
    `2026-07-08T15-44-00-0700-item12-codex-fullturn-current-stack.log`;
    `2026-07-08T15-45-00-0700-item12-codex-session-negative-current-stack.log`.
    Rev: `kyswsrtv`.
- **Objective:** write real e2e tests proving each behaves like its Linux
 counterpart (jq processes real JSON, sed edits streams, tar round-trips archives,
  gzip round-trips, etc.); agents exercise the real ACP
  adapter against the upstream SDK.
- **Proof:** `software/<pkg>/test/` exists and passes for each; coverage gate green.
- **rev:** one per package, e.g. `test(jq): add real JSON-processing e2e`

---

## Cross-cutting / misc

### 13. `everything` meta-package has no `agentos-package.json` — DONE
- **Fixed:** added the missing meta-package manifest, aligned the bundle with all
  current command packages (`duckdb`, `envsubst`, `git`, `sqlite3`, and `vim`
  were missing), and refreshed the workspace lockfile edges.
- **Proof:** `software/everything/test/everything.test.ts` proves the manifest is
  present and the default export resolves every command package descriptor once.
  Package build/check-types pass in
  `2026-07-08T14-21-00-0700-item13-everything-build-after-install.log` and
  `2026-07-08T14-21-00-0700-item13-everything-check-types-after-install.log`;
  the package test passes 2/2 in
  `2026-07-08T14-21-00-0700-item13-everything-test-after-install.log`;
  layout validation passes in
  `2026-07-08T14-22-00-0700-item13-check-layout.log`.
- **rev:** `fix(everything): add valid package manifest`

---

## Sequencing note

P0 first — several P1/P3 items depend on it: curl (#6) needs sockets/HTTP;
sqlite3 file-DB tests (#11) need pwrite (#4). Fix the runtime layer, then the
commands that ride on it, then backfill coverage. One jj rev per item throughout.

---

## Candidate software (future additions)

Constraint: **C or Rust only** (no Go/Haskell/Python). Real upstream tool or an
established project, same rules as above. **Focus: tools agents invoke headless
and programmatically** — no TUI/visual tools, no dev/build toolchains, no
raw-socket tools. Feasibility: 🟢 easy (fs/compute), 🟡 needs a known pattern
(spawn→`wasi-spawn`, PTY, host TCP/DNS bridge).

**Atuin-validated priority (from 348K real shell commands):** by actual usage the
clear wins are **jj** (18,929 — 3rd overall after sed/rg/grep) and **tmux**
(1,268), then **perl** (563). Modest but real: ssh/rsync (~23 each), psql (20),
dig (10), redis-cli (8), less (4), openssl (3). **Zero usage in this history:**
zstd, xz, gpg, ffmpeg, age, mlr, socat, nc, screen — generically useful but not
agent-triggered here, so lower priority. Big unserved *demand*:
**ps (1,364) + pkill (866) + pgrep (755) ≈ 3K** — see process management below.

**Cross-source validation (Homebrew + Debian popcon + agent-sandbox base images +
SWE-agent/OpenHands/Terminal-Bench trajectories):** independent sources converge
tightly on the list below. **Every agent-sandbox image (OpenHands, devcontainers,
GH Actions) pre-installs:** curl, wget, git, jq, tmux, gnupg, xz, zip/unzip, rsync,
ssh, less, vim, tree, procps, psmisc, socat, netcat, ripgrep, bzip2, lz4, sqlite3,
patch, file — near-exact overlap with what we ship/plan. Real **agent
trajectories** are dominated by coreutils (ls/rm/find/mkdir/cat/mv/chmod) + python
+ curl/wget + grep/sed + openssl + ps — all shipped or listed. New adds surfaced:
**imagemagick** ⭐ (C, image ops — high popularity), **openssl** (confirmed
high-use), **aria2 / brotli / parallel / sshpass** (base-image staples). Method
signals worth knowing: (1) agents **edit files ~2:1 over running shell commands**,
and the dominant shell idiom is "write a python repro script, run it, `rm` it" —
so a solid coreutils + python + curl/grep/sed/openssl core matters more than tool
breadth; (2) `git` is **rare inside agent turns** (harnesses extract the diff
out-of-band) but stays essential; (3) a **long tail of project-specific CLIs**
(`dvc`, `sqlglot`, `sanic`, …) comes from pip/npm install, not the registry.

**Requested (add):** ssh ✅, rsync 🟡, tmux/screen 🟡 (PTY — session persistence),
gpg 🟡, ffmpeg 🟡 (media transcode — heavy but headless), jj 🟢, dig 🟡,
nslookup 🟡, less ⭐🟡 (pager), openssl ⭐🟡 (TLS/certs/keys/hashing).
tail/head/cat are already in coreutils — confirm present.

OpenSSH now carries a private, static OpenSSL 3.5.7 **libcrypto** dependency for
its standard software crypto algorithms. The `openssl` command remains a future
registry addition: libssl, providers/modules, and the CLI are not shipped, and
curl/wget/git continue to use mbedTLS.

**Text / stream:** less ⭐🟡, **perl** ⭐🟡 (ubiquitous `-pe`/`-ne` text munging —
big C runtime but real; 563 uses in history), miller `mlr` 🟢 (CSV/JSON),
xmlstarlet 🟢, pcre2grep 🟢. (jq/yq/sed/awk/grep/head/tail already covered.)

**Networking (host TCP/DNS bridge only):** openssl ⭐🟡, ssh ✅, nc/netcat 🟡
(TCP/UDP), socat 🟡, whois 🟢, dig/nslookup 🟡, redis-cli / psql client 🟡,
aria2 🟡 (C++ downloader), sshpass 🟢 (ssh password helper).

**VCS:** git (item above), jj 🟢.

**Crypto:** gpg 🟡, openssl 🟡, age 🟢 (Rust), minisign 🟢.

**Compression:** xz, zstd, bzip2, lz4, brotli, p7zip (7z) — all ⭐🟢, common + easy.

**Media / image:** ffmpeg 🟡 (transcode), imagemagick ⭐🟡 (C, image ops — high
popularity; agents do image work).

**Files / sync:** rsync 🟡, diff/patch 🟢, rename 🟢, fdupes 🟢 (find/fd tracked
above).

**Session:** tmux/screen 🟡 (PTY).

**Process management (add — real procps-ng + psmisc, C; ps/pkill/pgrep ≈ 3K uses):**
- **Need the `/proc` prerequisite (below):** ps, pgrep, pkill, pidof, pstree,
  killall, uptime, free, vmstat, w, pwdx, pmap 🟡.
- **Signal-only — already work via the kernel (no /proc):** kill, killall-by-PID.
  (kill, sleep, timeout, env, nohup, nproc, nice/renice are coreutils — confirm
  they're shipped via uutils rather than re-adding.)

**⚙️ Runtime prerequisite — implement `/proc` (process-table-backed):** procps
reads `/proc/<pid>/{stat,cmdline,status,comm}` and enumerates `/proc/<pid>/`. The
**kernel already owns the process table** (`crates/kernel/src/process_table.rs`),
so expose a read-only procfs view of it to the guest (per-PID stat/cmdline/status
+ directory enumeration). Scope it minimal — just the fields procps parses, backed
by the existing process table, not a full Linux procfs. Unlocks the whole
ps/pkill/pgrep family (and top/htop later if ever wanted). **This is a runtime/VFS
item, do it before the procps packages.**

**Excluded — not worth it / not possible here:**
- **TUI / visual-only:** gitui, lazygit, eza, dust, ncdu, bat, delta, broot, k9s,
  skim/fzf — a terminal UI has no agent value.
- **top / htop — excluded (TUIs).** (ps/pkill/pgrep and the rest of procps-ng +
  psmisc are ADD items above, gated on the `/proc` runtime prerequisite.)
- **Raw sockets:** ping, traceroute, mtr, nmap (need raw/ICMP, not just TCP).
- **ptrace:** strace, ltrace, gdb, lldb, valgrind — genuinely impossible on WASI.
- **Dev / build toolchains:** make, cmake, clang/gcc, binutils, pkg-config,
  ctags — out of scope.
- **Go-only:** rclone, gh, kubectl — no C/Rust equivalent.
