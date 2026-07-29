---
title: "Bash"
description: "Run shell commands, pipes, and file manipulation inside the VM with vm.process.exec()."
category: "Quickstart"
order: 2
---

Run real shell commands inside a VM. Reach for this when you need to shell out — invoke CLIs, chain pipes, or read and write files — without leaving the sandbox.

## How it works

Create a VM with `AgentOs.create()`, then call `vm.process.exec()` with any shell string. Each call runs the command in the VM and resolves to a result carrying an `outcome`, `stdout`, `stderr`, and `exitCode`. Because it's a real shell, pipes (`|`), redirects (`>`, `>>`), and tools like `wc` and `tr` work as written, and files persist across calls within the same VM. `vm.process.execFile()` skips the shell entirely so arguments can never be reinterpreted, `vm.process.spawn()` starts a background process you drive by pid, and `timeoutMs` bounds any call. Call `vm.dispose()` when you're done to release it.

## Run it

```sh
npm install
npm run start -- bash
```

You'll see the workspace listing, the piped uppercase output, `$(whoami)` printed literally by `execFile`, the background shell's output replayed by pid, a line count of `3`, and a final `timed_out` outcome from the deliberately short deadline.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/quickstart/bash
