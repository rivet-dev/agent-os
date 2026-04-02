# wasm-git Research Analysis

Research on https://github.com/petersalomonsen/wasm-git for informing our clean-room Rust Git implementation targeting wasm32-wasip1.

## What is wasm-git?

wasm-git compiles **libgit2** (v1.7.1, a C library) to WebAssembly using **Emscripten**. It targets browser and Node.js environments, NOT wasm32-wasip1/WASI. The output is an Emscripten WASM module (`lg2.wasm` + `lg2.js` glue) that exposes libgit2's example programs as a CLI-like interface via `callMain()`.

Key distinction: wasm-git is Emscripten-based (wasm32-unknown-emscripten target), not WASI-based. This means it relies heavily on Emscripten's JavaScript glue code for I/O, filesystem, and networking. Our approach (wasm32-wasip1 targeting a WASI runtime in secure-exec) is fundamentally different.

## Architecture

The project wraps libgit2's `examples/` directory programs into a single binary. The main entry point (`lg2.c`) dispatches subcommands to individual example implementations. It is NOT the full git CLI -- it is a subset of git operations implemented via libgit2's C API.

### Build variants

Three build modes exist, all using Emscripten:

1. **Sync** (default) -- Runs in Web Workers using synchronous XHR for HTTP transport. Smallest binary.
2. **Async** (Asyncify) -- Uses Emscripten Asyncify to allow async JS calls from synchronous C code. Doubles binary size due to stack unwinding/rewinding instrumentation. Can run on main thread.
3. **OPFS** -- Uses Emscripten's WASMFS with Origin Private File System backend. Runs in Web Worker with pthreads. No Asyncify needed.

### Build flags of interest

```
-DREGEX_BACKEND=regcomp    # Uses POSIX regcomp, not PCRE
-DUSE_HTTPS=OFF            # No HTTPS -- HTTP only in transport
-DUSE_SSH=OFF              # No SSH transport
-DTHREADSAFE=OFF           # Single-threaded
-DBUILD_SHARED_LIBS=OFF    # Static linking
-s ALLOW_MEMORY_GROWTH=1   # Dynamic memory
-s STACK_SIZE=131072        # 128KB stack
```

## Supported Git Operations

From `lg2.c` command table (these are the libgit2 example programs, some patched):

| Command | Patched? | Notes |
|---------|----------|-------|
| add | Yes | Supports -v, -n, -u flags |
| blame | No | Stock libgit2 example |
| cat-file | No | Stock |
| checkout | Yes | Branch switching, path checkout, -b flag, remote tracking setup |
| clone | No | Stock, but HTTP-only (no SSH, no HTTPS) |
| commit | Yes | Supports -m flag, merge commits, signature from .gitconfig |
| config | No | Stock |
| describe | No | Stock |
| diff | No | Stock |
| fetch | No | Stock |
| for-each-ref | No | Stock |
| general | No | Stock |
| index-pack | No | Stock |
| init | No | Stock |
| log | No | Stock |
| ls-files | No | Stock |
| ls-remote | No | Stock |
| merge | No | Stock |
| push | Yes | Custom implementation, pushes HEAD to origin |
| remote | No | Stock |
| reset | Yes | Supports --hard, --soft |
| revert | Yes | Custom implementation |
| rev-list | No | Stock |
| rev-parse | No | Stock |
| show-index | No | Stock |
| stash | Yes | push/pop/apply/list/drop |
| status | Yes | Long/short/porcelain format, ahead/behind, conflict display |
| tag | No | Stock |

**Notable absences**: No `branch` command (branching done via checkout -b), no `rm`, no `rebase`, no `cherry-pick`, no `bisect`, no `submodule` management, no `worktree`.

## Special Adaptations

### 1. HTTP Transport (the biggest adaptation)

libgit2's standard HTTP transport (`http.c`) is **completely replaced** with a custom Emscripten HTTP transport. The original `http.c` is deleted in `setup.sh` and replaced with `emscriptenhttp.c`.

The transport implements libgit2's `git_smart_subtransport` interface and delegates actual HTTP to JavaScript:

- **Sync version**: Uses `EM_ASM_INT` to call into `Module.emscriptenhttpconnect/read/write` JS functions. In browser, these use synchronous `XMLHttpRequest`. In Node.js, they use `worker_threads` with `SharedArrayBuffer` + `Atomics` for synchronous cross-thread HTTP.
- **Async version**: Uses `EM_JS` with `Asyncify.handleAsync()` to wrap async fetch calls. Browser uses async XHR; Node.js uses same SharedArrayBuffer approach.

**Key insight for our implementation**: Git's HTTP smart protocol uses 4 endpoints:
- `/info/refs?service=git-upload-pack` (fetch discovery)
- `/git-upload-pack` (fetch data)
- `/info/refs?service=git-receive-pack` (push discovery)
- `/git-receive-pack` (push data)

These are POST requests with specific content types (`application/x-git-upload-pack-request`, etc.). Any WASI implementation needs to be able to make these HTTP requests through the host.

### 2. Filesystem Layer

wasm-git does NOT implement its own filesystem. It uses Emscripten's built-in FS backends:

- **MEMFS**: In-memory, not persisted. Default.
- **IDBFS**: IndexedDB-backed. Requires explicit `FS.syncfs()` calls to persist.
- **NODEFS**: Pass-through to host Node.js filesystem.
- **WASMFS + OPFS**: Emscripten's newer filesystem with Origin Private File System backend.

**Key insight for our implementation**: For wasm32-wasip1, the filesystem is provided by the WASI runtime (our secure-exec kernel VFS). We don't need to implement any of this -- WASI fd_read/fd_write/path_open etc. will be handled by the kernel. This is a major simplification compared to wasm-git's approach.

### 3. File Permission Patches

Two patches in `setup.sh` change file modes:
```c
// pack.h: GIT_PACK_FILE_MODE 0444 -> 0644
// odb.h: GIT_OBJECT_FILE_MODE 0444 -> 0644
```

libgit2 creates pack files and object files as read-only (0444). Emscripten's FS doesn't handle this well -- once a file is created read-only, it can't be modified or deleted. Changing to 0644 allows normal read-write access.

**Key insight for our implementation**: Our VFS supports chmod properly, so we may not need this workaround. However, if our VFS has any issues with read-only files being subsequently opened for write (e.g., during repacking), we'd hit the same issue.

### 4. Integer Overflow Intrinsics

`integer.h` is patched to add an Emscripten-specific case for `size_t` overflow detection:
```c
#if defined(__EMSCRIPTEN__)
  // Emscripten/WebAssembly: size_t is unsigned long
  #define git__add_sizet_overflow(out, one, two) __builtin_uaddl_overflow(one, two, out)
  #define git__multiply_sizet_overflow(out, one, two) __builtin_umull_overflow(one, two, out)
```

In Emscripten's WASM target, `size_t` is `unsigned long` (32-bit), but the existing code only matched `UINT_MAX` or `ULONG_MAX` cases that didn't apply to Emscripten's type configuration.

**Key insight for our implementation**: In wasm32-wasip1, `size_t` is 32-bit. If using libgit2's C code (we're not -- we're doing Rust), this would matter. In Rust, integer overflow is handled by the language.

### 5. C Standard Compatibility

```bash
echo 'set(CMAKE_C90_STANDARD_COMPILE_OPTION "-std=gnu90")' >> CMakeLists.txt
```

Forces GNU C90 standard across all libgit2 compilation units for Emscripten compatibility.

### 6. WASMFS getcwd() Bug Workaround

The OPFS variant has a significant workaround for a WASMFS bug where `getcwd()` returns wrong paths for directories backed by a different backend than the root. The workaround creates symlinks at the root so broken paths still resolve:

```javascript
// getcwd() returns '//repo' instead of '/opfs/repo'
FS.symlink(workingDir + '/' + repoName, '/' + repoName);
```

Additionally, CWD must be re-set before each `callMain()` call because WASMFS may reset it.

### 7. chmod Workaround (now removed)

The async build previously had a workaround for libgit2 calling chmod with only `S_IFREG` set (permissions 0000). This was later found to be unnecessary and removed.

## Emscripten Fixes Required

Four PRs to Emscripten itself were needed (all merged by 2020-03-29):

1. **emscripten#10095**: Unknown (PR not detailed in README)
2. **emscripten#10526**: Unknown
3. **emscripten#10782**: Unknown
4. **emscripten#10669**: Needed for NODEFS support

These were all early-stage fixes for edge cases in Emscripten's WASM compilation and filesystem layer.

## Gotchas and Lessons Learned

### For any WASM git implementation:

1. **No SSH transport** -- Only HTTP smart protocol is supported. SSH would require a TCP socket + SSH client implementation, which is extremely complex in WASM.

2. **No HTTPS** -- wasm-git builds with `USE_HTTPS=OFF`. In browser, the browser handles TLS for XHR/fetch. In our case, the secure-exec network adapter handles TLS on the host side, so this isn't an issue.

3. **No native credential handling** -- Authentication is done by the HTTP layer (basic auth in URLs or custom headers). There's no SSH key or credential helper support.

4. **File permissions matter** -- libgit2 creates pack/object files as read-only. If the VFS doesn't support chmod properly, or if re-opening read-only files for write fails, git operations will break.

5. **Single-threaded** -- libgit2 is compiled with `THREADSAFE=OFF`. This is fine for WASM.

6. **Memory growth** -- Git operations (especially clone) can use significant memory. `ALLOW_MEMORY_GROWTH=1` is essential.

7. **Stack size** -- 128KB stack is used. Git operations with deep directory trees or large diffs could potentially overflow this.

8. **The "examples" are the interface** -- wasm-git doesn't wrap the full libgit2 API. It uses the example programs which are simplified implementations. Some operations (like push) have very limited option support.

### For our Rust + wasm32-wasip1 approach:

1. **We have a fundamental advantage**: WASI provides a proper POSIX-like filesystem interface. We don't need to implement FS backends or deal with Emscripten's FS quirks. The kernel VFS handles everything.

2. **Networking is the hard part**: The git smart HTTP protocol needs HTTP client support. In WASI, we'd need to either:
   - Use WASI socket APIs (if available in our runtime)
   - Implement a custom host function for HTTP requests
   - Use the secure-exec kernel's network adapter

3. **Consider using gitoxide (gix)**: Instead of compiling libgit2 to WASM, a pure Rust git implementation like [gitoxide](https://github.com/Byron/gitoxide) could potentially compile to wasm32-wasip1 more cleanly, since it's already Rust and doesn't have C dependencies (except for optional features).

4. **Alternatively, git2-rs**: The Rust bindings for libgit2 could work, but would require cross-compiling libgit2's C code to wasm32-wasip1 (not Emscripten), which is a different challenge. Issue #77 on wasm-git discusses this but no one has done it.

5. **Consider scope carefully**: wasm-git supports ~27 commands but many are stock libgit2 examples with limited options. For coding agents, the critical operations are: clone, status, add, commit, diff, log, checkout, branch, push, pull (fetch+merge), stash, reset. This is a tractable subset.
