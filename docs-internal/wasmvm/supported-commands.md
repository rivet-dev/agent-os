# WasmVM Supported Commands

> **This is a living document.** Update it whenever a command's status changes.

## Status Key

| Symbol | Meaning |
|--------|---------|
| `done` | Fully implemented via uutils crate or established library wrapper |
| `builtin` | Custom minimal implementation in `builtins.rs` — to be replaced |
| `custom` | Custom implementation (grep.rs, find.rs) — to be replaced |
| `shim` | Subprocess shim via `wasi-ext` proc_spawn |
| `stub` | Returns error message or hardcoded value |
| `shell` | Handled in shell layer, not dispatch table |
| `missing` | Not yet implemented |
| `excluded` | Intentionally excluded from scope |

## File Operations

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| cat | yes | done | `uu_cat` (vendor+patch) | — |
| chmod | yes | done | `uu_chmod` (vendor+patch US-007) | — |
| cp | yes | done | `uu_cp` (vendor+patch US-007) | — |
| dd | — | done | `uu_dd` (vendor+patch US-010) | — |
| file | yes | done | `file.rs` custom builtin (`infer` crate, US-014) | — |
| install | — | stub | error message | `uu_install` (vendor+patch) |
| link | — | done | `uu_link` | — |
| ln | yes | done | `uu_ln` (vendor+patch US-010) | — |
| ls | yes | done | `uu_ls` (hostname stub) | — |
| mkdir | yes | done | `uu_mkdir` (Tier 1, patched uucore) | — |
| mktemp | — | done | `uu_mktemp` (vendor+patch US-010) | — |
| mv | yes | done | `uu_mv` (Tier 1, no patches) | — |
| readlink | yes | done | `uu_readlink` | — |
| realpath | — | done | `uu_realpath` | — |
| rm | yes | done | `uu_rm` (Tier 1, no patches) | — |
| rmdir | yes | done | `uu_rmdir` | — |
| shred | — | done | `uu_shred` | — |
| split | yes | done | `uu_split` (vendor+patch US-015) | — |
| stat | yes | done | `uu_stat` (vendor+patch US-007) | — |
| touch | yes | done | `uu_touch` (vendor+patch US-010) | — |
| tree | yes | done | `tree.rs` custom builtin (US-015) | — |
| truncate | — | done | `uu_truncate` | — |
| unlink | — | done | `uu_unlink` | — |

## Text Processing

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| awk | yes | done | `awk-rs` wrapper | — |
| comm | yes | done | `uu_comm` | — |
| column | yes | done | `column.rs` custom builtin (US-013) | — |
| cut | yes | done | `uu_cut` | — |
| diff | yes | done | `diff.rs` custom builtin (`similar` crate, US-018) | — |
| expand | yes | done | `uu_expand` | — |
| fmt | — | done | `uu_fmt` | — |
| fold | yes | done | `uu_fold` | — |
| grep | yes | done | `grep.rs` POSIX BRE/ERE/fixed shim (444 lines) | — |
| egrep | — | done | `grep.rs` | — |
| fgrep | — | done | `grep.rs` | — |
| head | yes | done | `uu_head` (Tier 1, no patches) | — |
| join | yes | done | `uu_join` | — |
| nl | yes | done | `uu_nl` | — |
| od | yes | done | `uu_od` | — |
| paste | yes | done | `uu_paste` | — |
| ptx | — | done | `uu_ptx` | — |
| rev | yes | done | `rev.rs` custom builtin (US-013) | — |
| rg | yes | done | `rg.rs` ripgrep-compatible (regex crate engine, US-011) | — |
| sed | yes | done | `uutils/sed` 0.1.1 (vendor+patch US-012) | — |
| sort | yes | done | `uu_sort` (Tier 1, no patches) | — |
| strings | yes | done | `strings.rs` custom builtin (US-013) | — |
| tac | yes | done | `uu_tac` (vendor+patch US-010) | — |
| tail | yes | done | `uu_tail` (vendor+patch) | — |
| tr | yes | done | `uu_tr` | — |
| tsort | — | done | `uu_tsort` (vendor+patch US-010) | — |
| unexpand | yes | done | `uu_unexpand` | — |
| uniq | yes | done | `uu_uniq` | — |
| wc | yes | done | `uu_wc` | — |

## Output / Printing

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| echo | yes | done | `uu_echo` | — |
| printf | yes | done | `uu_printf` | — |
| tee | yes | done | `uu_tee` | — |
| yes | — | done | `uu_yes` | — |

## Checksums & Encoding

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| b2sum | — | done | `uu_b2sum` | — |
| base32 | — | done | `uu_base32` | — |
| base64 | yes | done | `uu_base64` | — |
| basenc | — | done | `uu_basenc` | — |
| cksum | — | done | `uu_cksum` | — |
| md5sum | yes | done | `uu_md5sum` | — |
| sha1sum | yes | done | `uu_sha1sum` | — |
| sha224sum | — | done | `uu_sha224sum` | — |
| sha256sum | yes | done | `uu_sha256sum` | — |
| sha384sum | — | done | `uu_sha384sum` | — |
| sha512sum | — | done | `uu_sha512sum` | — |
| sum | — | done | `uu_sum` | — |

## Navigation & Path

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| basename | yes | done | `uu_basename` | — |
| cd | yes | shell | shell builtin | Rust shell |
| dirname | yes | done | `uu_dirname` | — |
| pathchk | — | done | `uu_pathchk` (vendor+patch US-010) | — |
| pwd | yes | done | `uu_pwd` | — |

## Disk & Filesystem

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| df | — | stub | error message | `uu_df` (vendor+patch) |
| du | yes | done | `du.rs` custom builtin (US-014) | — |

## System & Environment

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| arch | — | done | `uu_arch` | — |
| date | yes | done | `uu_date` | — |
| env | yes | shim | `shims::env` | — |
| envsubst | — | done | C program (`getenv`, stdin filter, US-078) | — |
| export | yes | shell | shell builtin | Rust shell |
| hostname | yes | stub | returns "wasm-host" | — (adequate) |
| hostid | — | stub | returns "00000000" | — (adequate) |
| logname | — | done | `uu_logname` (vendor+patch US-010) | — |
| nproc | — | done | `uu_nproc` | — |
| printenv | yes | done | `uu_printenv` | — |
| uname | — | done | `uu_uname` | — |
| whoami | yes | builtin | `builtins::whoami` | — |

## Process & Execution

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| expr | yes | done | `expr.rs` custom builtin (`regex` crate, US-014) | — |
| factor | — | done | `uu_factor` | — |
| make | yes | done | C program (clean-room POSIX make, `posix_spawn`, US-083) | — |
| false | yes | done | `uu_false` | — |
| nice | — | shim | `shims::nice` | — |
| nohup | — | shim | `shims::nohup` | — |
| numfmt | — | done | `uu_numfmt` | — |
| seq | yes | done | `uu_seq` | — |
| shuf | — | done | `uu_shuf` | — |
| sleep | yes | builtin | `builtins::sleep` (busy-wait) | add host callback |
| stdbuf | — | shim | `shims::stdbuf` | — |
| test / [ | — | builtin | `builtins::test_cmd` | — |
| timeout | yes | shim | `shims::timeout` | — |
| true | yes | done | `uu_true` | — |
| xargs | yes | shim | `shims::xargs` (proc_spawn, US-019) | — |

## Search

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| find | yes | custom | `find.rs` (540 lines, ~50%) | enhance custom (add -exec, -mtime, -size) |
| fd | — | done | `fd.rs` custom (walkdir+regex, fd-find compatible CLI) | — |

## Formatting & Display

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| dircolors | — | done | `uu_dircolors` | — |

## Compression

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| gzip | yes | done | `gzip.rs` custom builtin (`flate2` crate, US-016) | — |
| gunzip | yes | done | `gzip.rs` custom builtin (`flate2` crate, US-016) | — |
| zcat | yes | done | `gzip.rs` custom builtin (`flate2` crate, US-016) | — |
| tar | yes | done | `tar_cmd.rs` custom builtin (`tar` + `flate2` crates, US-017) | — |
| zip | — | done | C program (`zlib` + `minizip`, US-076) | — |
| unzip | — | done | C program (`zlib` + `minizip`, US-077) | — |

## Shell Builtins

| Command | just-bash | Status | Target |
|---------|-----------|--------|--------|
| alias | yes | shell | Rust shell |
| bash | yes | shell | Rust shell |
| cd | yes | shell | Rust shell |
| clear | yes | shell | Rust shell |
| export | yes | shell | Rust shell |
| help | yes | shell | Rust shell |
| history | yes | shell | Rust shell |
| sh | yes | shell | Rust shell |
| time | yes | shell | Rust shell |
| unalias | yes | shell | Rust shell |
| which | yes | shell | Rust shell |

## Data Processing

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| jq | yes | done | `jaq` wrapper | — |
| sqlite3 | yes | done | C program (SQLite amalgamation, WASI VFS, US-081) | — |
| yq | yes | done | `yq.rs` custom builtin (`serde_yaml` + `toml` + `quick-xml` + `jaq-core`, US-020) | — |
| xan | yes | missing | — | `xsv` fork or `csv` crate |
| python3 | yes | excluded | — | — |
| js-exec | yes | excluded | — | — |

## Network

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| curl | yes | done | `curl_cli.c` (libcurl HTTP-only, C program via `host_net`) | — |

## Version Control

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| git | yes | done | C program (clean-room Apache-2.0, SHA-1 + zlib + libcurl): init, add, commit, status, log, diff, branch, checkout, merge, tag, remote, clone, fetch, push, pull, hash-object, cat-file | — |
| git-remote-http | — | done | Symlink → git (HTTP transport for clone/fetch/push) | — |
| git-remote-https | — | done | Symlink → git (HTTPS transport for clone/fetch/push) | — |

## AI Tools

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| codex | — | done | Rust binary (`rivet-dev/codex` fork, TUI mode via ratatui/crossterm, `host_net` + `host_process`) | — |
| codex-exec | — | partial | Rust binary placeholder; provider-backed headless mode is not wired | — |

- **codex** is the TUI (interactive terminal UI) mode — requires a PTY for rendering
- **codex-exec** currently accepts prompt arguments only as a placeholder and fails fast for ACP session-turn mode
- Provider-backed Codex commands require `OPENAI_API_KEY` and network access (`host_net`) when that path is wired

## Package Management (Node Runtime)

| Command | just-bash | Status | Implementation | Target |
|---------|-----------|--------|----------------|--------|
| npm | yes | done | Routed through Node RuntimeDriver (host npm-cli.js in V8 isolate) | — |
| npx | yes | done | Routed through Node RuntimeDriver (host npx-cli.js in V8 isolate) | — |

## Deferred

| Command | just-bash | Reason | Notes |
|---------|-----------|--------|-------|
| html-to-markdown | yes | Depends on curl | `htmd` crate (MIT) for conversion |

## Stubbed (WASM-incompatible)

| Command | Status | Behavior |
|---------|--------|----------|
| chcon / runcon | stub | SELinux not supported |
| chgrp / chown | stub | ownership not supported |
| chroot | stub | no filesystem root change |
| groups / id | stub | user database not supported |
| kill | stub | signals not supported |
| mkfifo / mknod | stub | special files not supported |
| pinky / who / users / uptime | stub | utmp not available |
| stty | stub | terminal control not supported |
| sync | stub | no-op (VFS is in-memory) |
| tty | stub | returns "not a tty" |

## Summary

| Category | Done | Builtin (to replace) | Custom (to replace) | Missing | Stub | Shell | Excluded |
|----------|------|---------------------|---------------------|---------|------|-------|----------|
| File Operations | 22 | 0 | 0 | 0 | 1 | 0 | 0 |
| Text Processing | 29 | 0 | 0 | 0 | 0 | 0 | 0 |
| Output / Printing | 4 | 0 | 0 | 0 | 0 | 0 | 0 |
| Checksums & Encoding | 12 | 0 | 0 | 0 | 0 | 0 | 0 |
| Navigation & Path | 4 | 0 | 0 | 0 | 0 | 1 | 0 |
| Disk & Filesystem | 1 | 0 | 0 | 0 | 1 | 0 | 0 |
| System & Environment | 8 | 1 | 0 | 0 | 2 | 1 | 0 |
| Process & Execution | 13 | 2 | 0 | 0 | 0 | 0 | 0 |
| Search | 1 | 0 | 1 | 0 | 0 | 0 | 0 |
| Formatting | 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| Compression | 6 | 0 | 0 | 0 | 0 | 0 | 0 |
| Shell Builtins | 0 | 0 | 0 | 0 | 0 | 11 | 0 |
| Data Processing | 3 | 0 | 0 | 1 | 0 | 0 | 2 |
| Network | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| Version Control | 3 | 0 | 0 | 0 | 0 | 0 | 0 |
| AI Tools | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| Package Management | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| **Total** | **113** | **3** | **1** | **1** | **4** | **13** | **2** |

---

## Bash Compatibility

**Implementation:** [brush-shell](https://github.com/reubeno/brush) (MIT license)
**Target:** bash 5.x compatible
**Dispatch aliases:** `sh`, `bash`

### Supported Features

| Feature | Status | Notes |
|---------|--------|-------|
| Pipes (`\|`) | done | via `fd_pipe` + `proc_spawn` (validated US-006) |
| Redirects (`>`, `>>`, `<`, `2>&1`, `N>&M`) | done | |
| Variables (`$VAR`, `${VAR}`, `${VAR:-default}`) | done | |
| Parameter expansion (`${VAR%pat}`, `${VAR#pat}`, `${VAR//pat/repl}`) | done | |
| Command substitution (`$(...)`, backticks) | done | via `proc_spawn` + capture stdout (validated US-006) |
| Arithmetic expansion (`$((...))`) | done | |
| Glob expansion (`*`, `?`, `[charset]`, `**`) | done | |
| Tilde expansion (`~`, `~/path`) | done | |
| Brace expansion (`{a,b,c}`, `{1..10}`) | done | |
| Control operators (`&&`, `\|\|`, `;`) | done | |
| if/elif/else/fi | done | |
| for/while/until loops | done | |
| case/esac | done | |
| Functions | done | |
| Here-documents (`<<EOF`, `<<-EOF`, `<<'EOF'`) | done | |
| Subshells (`(...)`) | done | via `proc_spawn` (validated US-006) |
| `export`, `unset`, `readonly` | done | |
| `test` / `[` / `[[` | done | |
| `alias` / `unalias` | done | |
| `set -e`, `set -x`, `set -o` | done | |
| `exec`, `eval`, `read`, `shift`, `source`/`.` | done | |
| `$@`, `$*`, `$#`, `$0`, `$$`, `$?` | done | |
| `type`, `command`, `which` | done | |

**23 of 23 features working** (0 planned, 8 stubbed — see Disabled/Stubbed below)

### Disabled / Stubbed Features

| Feature | Status | Reason |
|---------|--------|--------|
| Signals (`trap`, `SIGINT`, `SIGTERM`, etc.) | stubbed | WASM has no signal support |
| Job control (`fg`, `bg`, `jobs`, `&`) | stubbed | No process groups in WASI |
| Terminal handling (`termios`, `stty`) | stubbed | No TTY in WASI |
| Process substitution (`<(...)`, `>(...)`) | stubbed | Requires `/dev/fd` or named pipes |
| Coprocesses (`coproc`, `\|&`) | stubbed | Requires bidirectional pipes + background |
| `select` builtin | stubbed | Requires interactive terminal |
| `suspend` builtin | stubbed | No job control |
| `ulimit` builtin | stubbed | No resource limits in WASM |

### Integration Notes

- brush-shell 0.3.0 integrated as **Tier 1 (direct dependency)** — compiles for wasm32-wasip1 with no patches
- Requires `default-features = false, features = ["minimal"]` to skip terminal/reedline deps
- Wired into dispatch as `"sh" | "bash"` via `brush_shell::entry::run()`
- **US-005:** brush-shell is now the sole shell — TypeScript shell parser/evaluator deleted, `exec()` spawns `sh -c '<command>'`
- All shell parsing, variable expansion, pipelines, redirects, control flow handled by brush-shell in WASM
- Child processes spawned via `std::process::Command` → patched stdlib → `host_process.proc_spawn` → Worker
- **US-006:** proc_spawn validated — pipes, command substitution, subshells, exit codes all work through brush-shell. Required: /bin/ stubs for PATH, std fd_dup patch, brush-core WASI command substitution patch, VFS sharing for inline children.
- Binary size impact: +1.65MB (6.35MB → 8.0MB)
- **US-008:** cat, head, tail, sort, ls replaced with uutils crates (uu_cat, uu_head, uu_tail, uu_sort, uu_ls). head/sort compiled Tier 1 (no patches). cat/tail required vendor+patch. ls required hostname stub. Binary size: 8.0MB → 8.6MB (+0.6MB)
- **US-009:** cp, mv, rm, mkdir, chmod replaced with uutils crates. mv/rm compiled Tier 1 (no patches). cp/chmod use existing vendor patches from US-007. mkdir uses patched uucore. Binary size: 8.6MB → 8.8MB (+0.2MB)
- **US-010:** stat, touch, ln, mktemp, dd, tac replaced with uutils crates. Also replaced: logname, pathchk, tsort. 8 vendor patches created (uu_tac mmap stub, uu_ln WASI symlink, uu_tsort fadvise removal, uu_touch WASI stdout path, uu_dd stdin handling, uu_pathchk PATH_MAX constants, uu_mktemp permissions gate, uu_logname env var fallback). builtins.rs reduced from 595→159 lines (only sleep, test, whoami, spawn-test remain).

---

## Architecture & Trade-offs

### Target Platforms

wasmVM runs on **any environment with WebAssembly + SharedArrayBuffer support**:

- **Browsers:** Chrome, Firefox, Safari, Edge (requires `Cross-Origin-Isolation` headers for SharedArrayBuffer)
- **Node.js:** v16+ (SharedArrayBuffer available by default)
- **Deno / Bun:** Supported via standard WebAssembly API
- **Serverless / Edge:** Cloudflare Workers, Vercel Edge Functions, etc. (where SharedArrayBuffer is available)

Browser compatibility is a hard requirement. Every architectural decision flows from this constraint.

### Why a JavaScript Host Runtime

The OS layer (WASI polyfill, VFS, process management, pipeline orchestration) is implemented in ~5,000 lines of TypeScript. This is not because JavaScript is the best language for an OS — it's because **the host runtime must be JavaScript to run in browsers**.

WASM modules cannot directly access browser APIs. Creating Workers, allocating SharedArrayBuffers, using Atomics, calling `Date.now()`, generating random bytes via `crypto.getRandomValues()`, instantiating WASM modules — all of these require JavaScript. A WASM-based OS layer would still need to call back into JS for every I/O operation, adding FFI overhead without eliminating the JS dependency.

The expensive computation (shell parsing, command execution, text processing, compression) is already in WASM. The JS layer is thin glue that routes WASI syscalls to browser APIs. Moving it to WASM would add complexity without meaningful performance gains.

See `notes/research/js-vs-wasm-os-layer.md` for the full analysis.

### Technologies We Chose Not to Use

#### Native WASM Runtimes (Wasmtime, etc.)

Native WASM runtimes provide a complete WASI implementation, the Component Model, epoch-based CPU interruption, memory limiters, and pre-compiled native code caching.

**Why we don't use them:** They are native binaries. They don't run in browsers. Our requirement is that the same WASM binary + JS host works identically in Chrome and Node.js. Using a native runtime would make wasmVM server-only.

We evaluated JavaScript-based WASI polyfills and found them incomplete — missing syscalls, broken fd management, no process spawning. It was faster to write a correct polyfill than to patch broken third-party ones.

#### WASM Component Model / WIT Interfaces

The Component Model adds typed interfaces (WIT), module composition, and structured host↔guest communication. Server-side projects use this to define clean contracts between host and guest.

**Why we don't use it:** No browser supports the Component Model natively. The `jco` tool (Bytecode Alliance) can transpile WASM components into core WASM + JS glue for browser use, but this adds a build step, increases bundle size, and the generated JS glue is another layer of indirection. We target `wasm32-wasip1` which produces core WASM modules that all environments understand natively.

#### WASI Shadowing

Some native-runtime-based projects use the linker to "shadow" the default WASI filesystem implementation with a custom hybrid VFS. The guest calls standard WASI functions; the linker intercepts and routes to virtual or real storage by path prefix.

**Why we don't use it:** Shadowing is a native-runtime linker feature. We don't have a base WASI implementation to shadow — we ARE the WASI implementation. Our `wasi-polyfill.ts` intercepts every syscall and routes it to our VFS directly. Same end result, different mechanism.

#### Shell-Level Spawn Handler Pattern

An alternative architecture intercepts brush-shell's command execution at the shell level via a "spawn handler" callback. When brush-shell wants to run `cat`, the handler looks it up in a registry and runs it in a separate WASM instance. brush-shell never calls `std::process::Command`, `pipe()`, or `dup()`.

**Why we use a different approach:** We let brush-shell call `std::process::Command` normally. Our patched Rust stdlib routes this to `host_process.proc_spawn`, which spawns a new Worker with the same WASM binary. This means brush-shell does real fd management, real pipe setup, and real process waiting — same as a Unix shell. The trade-off: we hit more WASI edge cases (fd_dup, spawn_blocking) that need patching, but we get true concurrent pipelines via ring buffers and don't need to intercept brush-shell's internals.

| | Spawn handler pattern | wasmVM (patched stdlib) |
|--|---|---|
| Pipeline concurrency | Batch I/O, sequential | True parallel via ring buffers |
| WASI patches needed | Almost none | fd_dup, spawn_blocking, PATH resolution |
| Shell modification | Spawn handler callback | No shell modification (stdlib level) |
| Fidelity | Host fakes process execution | Real process spawning |

### What's on the Horizon

These WASM proposals could change the architecture in the future, but none are ready for production use:

| Proposal | Status (March 2026) | What It Would Enable |
|----------|---------------------|---------------------|
| **Shared-Everything Threads** | Phase 1–2, no implementations | WASM-native IPC between modules — could replace SharedArrayBuffer ring buffers with WASM-level shared memory + atomics. Most impactful proposal for us. ETA: 2028+. |
| **Stack Switching** | Phase 3, no implementations | WASM coroutines/green threads — would let brush-shell's async runtime work natively without `spawn_blocking` hacks. ETA: 2027+. |
| **JSPI (JS Promise Integration)** | **Shipped** (Chrome 137, Firefox 139, Safari via Interop 2026) | Lets synchronous WASM call async JS functions. Could simplify our Atomics.wait blocking pattern for stdin reads and process waiting. Available now but doesn't fundamentally change the architecture. |
| **Component Model** | Phase 2–3, no browser support | Typed host↔guest interfaces (WIT). Would clean up our import/export definitions but not eliminate JS dependency. Browser-native support has no timeline. |
| **Multi-Memory** | **Shipped** (Chrome, Firefox) | Multiple memory blocks per WASM module. Minimal impact — our architecture uses separate instances per Worker. |

**Bottom line:** Nothing shipping in the next 1–2 years changes our approach. JSPI is available now as a potential optimization. Shared-Everything Threads is the one to watch — if it ships, it could enable a pure-WASM IPC layer. But that's years away.

See `notes/research/js-vs-wasm-os-layer.md` for detailed analysis.
