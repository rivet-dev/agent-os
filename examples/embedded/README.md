---
title: "Embedded VMs"
description: "Embedded agentOS API: exec, configuration, lifecycle events, and mounts."
category: "Reference"
order: 1
---

The embedded `@rivet-dev/agentos-core` API surface in one place: boot a VM with
`AgentOs.create()` and control it from your application for exec, filesystem,
processes, agent sessions, networking, and cron. There is no actor runtime or
client/server split. Reach for this when you already own the application
lifecycle and want a reference for what an `AgentOs` instance can do and how it
is configured.

## How it works

`AgentOs.create({ ... })` boots a VM through a shared sidecar process with its
mounts, software, and network settings, then returns an `AgentOs` instance to
the application. Everything runs through that instance: `exec`/`spawn` for processes, `readFile`/`writeFiles`/`readdirEntries`
for the filesystem, `openSession`/`prompt` for agents, `httpRequest` for in-VM
servers, and `scheduleCron` for jobs. Portable process, shell, session, adapter,
and cron events use explicit subscription methods. Interactive permission
requests and responses are variants of the generic `onSessionEvent` union.

- `vm.ts`: boot a VM and every instance capability (exec, filesystem,
  processes, sessions, networking, cron).
- `quickstart.ts`: run a coding agent directly from a Node.js application.
- `advanced.ts`: pin VMs to a dedicated sidecar process.
- `agent-to-agent.ts`: connect two embedded agents through a binding.
- `bindings.ts`: expose a host function as a command inside the VM.
- `config-reference.ts`: common `AgentOs.create()` configuration.
- `hooks.ts`: generic durable session-event observation.
- `limits.ts`: configure resource limits and receive local warnings.
- `models-and-credentials.ts`: pass provider credentials to a session.
- `mounts.ts`: host-directory and S3 mount descriptors.
- `persistence.ts`: restore filesystem state from a SQLite database.
- `permissions.ts`: apply a kernel permission policy at VM creation.
- `software.ts`: install a software package and run its command.

## Run it

```sh
npm install
npx tsx vm.ts
```

## Source

View the source on GitHub: https://github.com/rivet-dev/agentos/tree/main/examples/embedded
