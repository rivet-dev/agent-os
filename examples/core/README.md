---
title: "Core"
description: "Core AgentOs API: exec, config reference, lifecycle events, and mounts."
category: "Reference"
order: 1
---

The core `@rivet-dev/agentos` API surface in one place: boot a VM with
`AgentOs.create()` and drive it directly for exec, filesystem, processes, agent
sessions, networking, and cron — no actor runtime and no client/server split.
Reach for this when you want a reference of what an `AgentOs` instance can do and
how it is configured.

## How it works

`AgentOs.create({ ... })` boots a VM in-process with its mounts, software, and
network settings, and returns an `AgentOs` instance. Everything runs through that
instance: `exec`/`spawn` for processes, `readFile`/`writeFiles`/`readdirEntries`
for the filesystem, `openSession`/`prompt` for agents, `httpRequest` for in-VM
servers, and `scheduleCron` for jobs. Portable process, shell, session, adapter,
and cron events use explicit subscription methods. Interactive permission
requests and responses are variants of the generic `onSessionEvent` union.

- `vm.ts` — boot a VM and every instance capability (exec, filesystem,
  processes, sessions, networking, cron).
- `advanced.ts` — pin VMs to a dedicated sidecar process.
- `config-reference.ts` — the full `AgentOs.create()` config surface.
- `hooks.ts` — generic durable session-event observation.
- `mounts.ts` — host-directory and S3 mount descriptors.

## Run it

```sh
npm install
npx tsx vm.ts
```

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/core
