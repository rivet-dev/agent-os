---
title: "Node.js"
description: "Run a Node.js script inside the VM that performs filesystem operations."
category: "Quickstart"
order: 3
---

Run real Node.js code inside a VM and capture its output. Reach for this when you want an agent (or your own pipeline) to execute a script against an isolated filesystem and read back the results.

## How it works

Create a VM with `AgentOs.create()`, then stage a script onto its filesystem with `filesystem.writeFile`. The script uses the standard `fs` module to make directories, write files, read them back, and stat them. `vm.process.exec("node /tmp/demo.mjs")` runs the script in the VM and returns its `stdout` and `exitCode`, which you print before tearing the VM down with `dispose()`.

## Run it

```bash
npm install @rivet-dev/agentos
node index.ts
```

You'll see the files the script created listed, the contents of `index.js`, and the size of `README.md`, followed by `Exit code: 0`.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/quickstart/nodejs
