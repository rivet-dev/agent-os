# Python

Execute Python source, files, modules, and package workflows in agentOS.

agentOS runs CPython 3.13 as a first-class VM execution engine. Python shares the
VM filesystem, process tree, networking, permissions, and limits with agents,
Bash, JavaScript, and installed software.

Letting an agent write code instead of chaining one tool call per step is called
[Code Mode](https://blog.cloudflare.com/code-mode/). It has a few advantages over
driving [Bash](/docs/bash):

- **Fewer tokens**: Ten chained operations cost one round trip, not ten.
- **Real data processing**: Comprehensions and `pandas` instead of `jq` and `awk`.
- **The package ecosystem**: `pip install` whatever the task needs.
- **Concurrency**: `asyncio` instead of shell job control.

## Evaluate an expression

`evaluate()` returns a JSON-serializable value.

A value the JSON encoder can't represent fails with
`evaluation_serialization_failed` rather than silently losing the value.

## Execute code

`execute()` runs source and captures its output instead of returning a value.

Executions are ephemeral, so capture stdio only when you want it — `"stderr"` for
diagnostics, `"all"` for both streams. `onStdout`/`onStderr` stream live and work
independently of capture.

## Pass data into code

`inputs` hands host values to the guest as real objects, so data never gets
interpolated into source.

## Keep state between calls

Pass a `contextId` to keep globals, functions, imports, and modules alive in one
interpreter.

Only inline `execute()`/`evaluate()` retain memory. Files, modules, installs, and
Bash may pass the same id, but they run fresh. A context runs one operation at a
time — reusing a busy `contextId` fails immediately.

## Files, modules, and async

Awaited work counts against the deadline; unawaited `asyncio` tasks are not
promised to survive between operations.

## Install packages

Installs modify the VM-wide filesystem, so a package installed once is importable
by every later execution in that VM, in any language. Only one npm/Python
mutation runs at a time per VM; a concurrent install fails with `execution_busy`.

## Background processes and web servers

`spawn*` starts a long-lived process and returns a `pid`. From there you get
stdin, output, signals, and waiting.

## A full Linux environment underneath

There is a real Linux environment behind all of this, shared by every language.
Files and installed packages are immediately visible to Bash, JavaScript, agents,
and other executions.

See [Filesystem](/docs/filesystem), [Processes & Shells](/docs/processes), and
[Networking & Previews](/docs/networking).

## Bindings

Python calls [bindings](/docs/bindings) as normal commands via `subprocess`, so
host credentials stay outside the VM.

## Permissions, limits, and timeouts

Every operation inherits the VM [permission policy](/docs/permissions) and
[resource limits](/docs/resource-limits).