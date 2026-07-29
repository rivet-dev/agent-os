---
title: "Processes"
description: "Execute commands and manage spawned processes inside the VM."
category: "Quickstart"
order: 5
---

Run shell commands and manage long-lived processes from inside a VM. Reach for this when you need to invoke CLI tools, build pipelines, or launch a script and stream its output as it runs.

## How it works

Create a VM with `AgentOs.create()`, then drive it two ways. Use `vm.process.exec()` for one-shot shell commands — it runs pipelines, `grep`, `sed`, and the like, returning `stdout` and an `exitCode`. For longer-running work, `vm.process.spawn()` starts a process and hands back a `pid`; subscribe with `vm.onProcessOutput(pid, handler)`. Wait for completion with `vm.process.wait(pid)`, and inspect what is running with `vm.process.list()`.

## Run it

```sh
npm install
npm run dev -- processes
```

You should see captured `exec` output, streamed `tick` lines from the spawned Node script, its exit code, and the live process list.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/quickstart/processes
