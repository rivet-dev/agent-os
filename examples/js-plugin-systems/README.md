---
title: "Plugin Systems"
description: "Evaluate untrusted JavaScript plugins with a narrow AgentOS policy."
---

This example evaluates plugin source inside an isolated VM and returns a
structured value. The plugin can use only the filesystem, network, processes,
packages, and bindings allowed by the runtime configuration.

Mount only the dependency tree a plugin needs, deny network unless it is part
of the contract, and expose privileged operations through validated bindings.
TypeScript plugins can be checked with `runtime.typescript.check()` before they
run in the same VM.

## Run it

```bash
pnpm --dir examples/js-plugin-systems start
```
