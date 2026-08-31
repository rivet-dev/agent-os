---
title: "Hello World"
description: "Minimal example: create a VM, write a file, and read it back."
category: "Quickstart"
order: 1
---

The smallest end-to-end agentOS program: spin up a VM, write a file into it, and
read the bytes back out. Reach for this when you want to confirm your install
works and learn the shape of the client API before building anything real.

## How it works

`AgentOs.create()` provisions a fresh VM and hands back a client. From there the
VM exposes a filesystem you drive with `writeFile` and `readFile`. `readFile`
returns raw bytes, so decode them with a `TextDecoder` to get a string. When
you are done, `dispose()` tears the VM down and releases its resources.

## Run it

```sh
npm install @rivet-dev/agentos-core
npx tsx index.ts
```

Expected output: `Hello from agentOS!`

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/quickstart/hello-world
