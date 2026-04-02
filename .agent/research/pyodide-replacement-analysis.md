# Pyodide Replacement Analysis

*Date: 2026-04-01*

Research on what it would take to replace Pyodide in Agent OS, with a focus on the difference between:

1. Replacing the current Pyodide integration with a native/server-only Python runtime that binds to Agent OS kernel/POSIX APIs.
2. Reproducing everything Pyodide does ourselves.

## Executive Summary

Replacing the current Pyodide integration with a native Python runtime for server-side Agent OS is feasible. It is still real work, but it is an integration project, not a platform rewrite.

Reproducing everything Pyodide does ourselves is much larger. Pyodide is not just "CPython compiled to WebAssembly." It also includes:

- patched CPython
- a JavaScript <-> Python FFI
- JavaScript runtime/bootstrap code
- an Emscripten platform definition with ABI-sensitive build flags
- package loading and wheel compatibility machinery
- a separate cross-build toolchain in `pyodide-build`

If browser compatibility matters, a native replacement is not equivalent. Agent OS's own architecture docs are explicit that browser support requires a JavaScript host runtime.

## Current Agent OS State

Agent OS already uses a thin wrapper around Pyodide rather than depending on broad Pyodide functionality.

### What the current integration does

- `packages/python/src/driver.ts` loads Pyodide with `loadPyodide(...)`.
- It registers a `secure_exec` JS module that bridges filesystem and network calls back to the host runtime.
- It blocks imports of `js` and `pyodide_js` to prevent escape into the host JS runtime.
- It applies stdin / env / cwd overrides and serializes returned values with conservative caps.
- `packages/python/src/kernel-runtime.ts` adds a `kernel_spawn` RPC bridge and monkey-patches `os.system()` / `subprocess` to route through the Agent OS kernel.

### Scope of our wrapper code

Local line counts:

- `packages/python/src/driver.ts`: 831 lines
- `packages/python/src/kernel-runtime.ts`: 790 lines
- runtime + tests in `packages/python`: 2,439 lines total

This is small compared to Pyodide itself. The current Agent OS work is primarily policy, sandboxing, and bridge logic, not interpreter/runtime implementation.

### Important behavior choices already made

The current compatibility docs explicitly constrain the Python surface:

- Pyodide runs in a Node worker thread.
- file/network/env/cwd/stdio are bridged
- `micropip`, `loadPackage`, and `loadPackagesFromImports` are blocked
- raw subprocess spawning is blocked unless routed through the kernel shim

That means Agent OS is already using only a narrow, controlled subset of Pyodide's capabilities.

## What Pyodide Actually Is

Pyodide's own README describes the project as:

- a build of CPython with patches
- a JS/Python foreign function interface
- JavaScript code for creating and managing interpreters
- an Emscripten platform definition with ABI-sensitive flags
- a toolchain for cross-compiling, testing, and installing packages

The repo structure document breaks the runtime into:

1. CPython
2. bootstrap Python packages in `src/py`
3. mixed C + JS core in `src/core`
4. more Python bootstrap/runtime code in `src/py`
5. public JS API and loader in `src/js`
6. `packages/` containing Python packages built for Pyodide

This matches the practical shape of the codebase: Pyodide is a distribution and platform, not just a WASM module.

## Repo Inspection

Local clone used for this analysis:

- `~/misc/pyodide` at commit `7fa9f3e`
- `~/misc/pyodide/pyodide-build` at commit `89f2524`

### Top-level repo language mix

`cloc` on the main `pyodide` repo reported:

| Language | Code lines |
|---|---:|
| Python | 20,115 |
| C | 8,619 |
| TypeScript | 7,530 |
| JavaScript | 1,374 |
| JSON | 18,762 |
| Markdown | 8,718 |
| Total counted code | 73,660 |

Interpretation:

- JS + TS together are about 8,904 lines, or about 12.1% of counted source code in the main repo.
- Python is the single largest source language in the repo.
- C is substantial because much of the runtime core is implemented in C and then compiled through Emscripten.

### Runtime directory mix

Looking only at the runtime directories `src/js`, `src/core`, and `src/py`:

| Area | Main languages | Code lines |
|---|---|---:|
| `src/js` | TypeScript + JavaScript | 5,252 |
| `src/core` | C + headers + JS/TS glue | 11,599 |
| `src/py` | Python | 4,084 |
| Total | mixed runtime source | 29,353 |

Approximate runtime split:

- JS + TS: 8,287 lines, about 28.2%
- C core only: 7,972 lines, about 27.2%
- C core + headers: 8,552 lines, about 29.1%
- Python runtime/bootstrap: 4,084 lines, about 13.9%

This is the most useful answer to "how much JavaScript is involved versus how much is WebAssembly":

- there is a real JS/TS runtime surface
- there is a real C core that becomes WASM at build time
- there is real Python bootstrap/runtime code
- there is almost no handwritten `.wasm` source; the WASM is generated build output

### Build toolchain burden

`pyodide-build` adds another 13,422 lines of counted code, mostly Python:

| Language | Code lines |
|---|---:|
| Python | 11,859 |
| YAML | 862 |
| TOML | 257 |
| Markdown | 240 |
| Total counted code | 13,422 |

If the goal is "own what Pyodide owns," this submodule matters. It is part of the maintenance burden.

## Delivered Artifact Shape

Pyodide's deployment docs make the runtime split explicit.

The core distribution contains:

- `pyodide.asm.mjs`: the JavaScript half of the main binary, generated by Emscripten
- `pyodide.asm.wasm`: the WebAssembly half of the main binary
- `pyodide.mjs`: a small loader shim
- `python_stdlib.zip`: the Python standard library and Pyodide runtime libraries
- `pyodide-lock.json`: package lock data used by package loading

This matters because "how much is JavaScript versus WebAssembly" is not only a source question. The shipped runtime also has an explicit JS half and WASM half.

## Difficulty by Goal

### Option A: Keep Pyodide and continue the current narrow integration

Difficulty: low to moderate

This is the easiest path.

What it involves:

- keep using Pyodide as the interpreter
- continue bridging only the capabilities we care about
- keep package install blocked unless there is a strong reason to open it up
- extend the `kernel_spawn` path if we want better subprocess fidelity

Why this is tractable:

- the current wrapper is already small
- Agent OS is not currently depending on most of Pyodide's package ecosystem
- the main remaining work is behavior/policy, not interpreter ownership

### Option B: Replace Pyodide with a native/server-only Python runtime

Difficulty: moderate to high

This is much easier than replacing Pyodide-the-platform, but it is still a substantial engineering project.

Main work:

- embed or manage CPython safely
- map file, network, env, cwd, stdin/stdout/stderr into Agent OS kernel semantics
- decide process model: true native subprocesses versus kernel-routed subprocess behavior
- define timeouts, cancellation, memory limits, restart semantics, and warm-state behavior
- define serialization rules for values crossing the boundary
- define what package installation means, if anything

Why this is still manageable:

- native CPython has ordinary POSIX expectations
- you avoid Emscripten ABI/platform maintenance
- you avoid browser constraints
- you avoid the JS<->Python FFI complexity Pyodide needs for browser use

The main risk is compatibility drift between "real CPython on host POSIX" and "Agent OS virtual POSIX/kernel semantics." The more native behavior you expose, the more fidelity work you own.

### Option C: Rebuild everything Pyodide does ourselves

Difficulty: very high

This is a platform effort, not a feature task.

Owning this means owning:

- CPython porting/patches for WebAssembly
- Emscripten platform and ABI policy
- JS bootstrap/runtime APIs
- JS<->Python FFI and proxy lifetimes
- package loader behavior
- shared-library and extension-module compatibility policy
- package build recipes and CI
- documentation and ongoing upstream churn

This is a multi-quarter to multi-year maintenance commitment if done seriously.

## Browser Constraint

This is the key architecture fork.

Agent OS's own wasmVM docs say browser compatibility is a hard requirement for the JS host runtime and explain why the host runtime is in TypeScript rather than WASM. They also explicitly reject native WASM runtimes because they would be server-only.

So the requirement question is:

- if this Python runtime must work in browsers, a native replacement is not a substitute for Pyodide
- if this Python runtime can be server-only, a native replacement becomes much more attractive

That is the single most important decision because it changes the project from "replace an integration" to "replace a platform."

## Recommendation

If the real requirement is "Python inside Agent OS on the server, with access to our kernel/POSIX abstraction," do not try to recreate Pyodide. Build or embed a native Python runtime with a narrow, explicit bridge to Agent OS.

If the real requirement is "Python inside Agent OS in both browser and server environments with similar behavior," keep Pyodide for the browser-compatible path. Replacing it wholesale would mean taking ownership of a large platform surface.

If the real requirement is only "the current Pyodide integration is too constrained," the shortest path is to keep Pyodide and improve the bridge rather than replacing the runtime.

## Suggested Approach for a Native Runtime

If we go native/server-only, the cleanest initial scope is:

1. stdlib-first runtime, no arbitrary package install
2. file/network/env/cwd/stdio bridged through Agent OS policies
3. subprocess routed through the Agent OS kernel rather than raw host subprocesses
4. warm interpreter model, matching the current `PythonRuntime`
5. explicit timeout / cancellation / restart semantics

This gets most of the value while avoiding the hardest compatibility surface.

After that, decide whether package installation is:

- unsupported
- curated / preinstalled only
- full `pip` support

That decision has major implications for complexity.

## Open Questions

These are the requirement questions that matter most:

1. Does this Python runtime need browser support, or can it be server-only?
2. Do we need arbitrary `pip install`, or only stdlib plus curated packages?
3. Should `subprocess` mean real native subprocesses, or Agent OS kernel-spawned commands?
4. Do we need C-extension package compatibility, or is pure Python enough?
5. Do we want warm persistent interpreter state, or more process-like isolation per execution?
6. Are we optimizing for narrower, predictable agent workloads or broad Python compatibility?

## Sources Consulted

Internal:

- `packages/python/src/driver.ts`
- `packages/python/src/kernel-runtime.ts`
- `docs/python-compatibility.mdx`
- `docs/wasmvm/supported-commands.md`

External clone inspected locally:

- `~/misc/pyodide/README.md`
- `~/misc/pyodide/repository-structure.md`
- `~/misc/pyodide/docs/usage/downloading-and-deploying.md`
- `~/misc/pyodide/docs/development/abi.md`
- `~/misc/pyodide/docs/development/building-from-sources.md`
- `~/misc/pyodide/src/js/README.md`
