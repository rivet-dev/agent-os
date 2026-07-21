---
title: "Dev Servers"
description: "Run a long-lived guest server and request it through AgentOS."
---

This example starts a Node.js HTTP server with `runtime.javascript.spawn()`,
waits for its readiness output, and calls it through
`runtime.vm.network.httpRequest()`. The guest
listener stays inside the VM; no host port is exposed.

The returned process handle controls lifecycle through `kill()` and `wait()`.
Keep the runtime alive while its server is needed, then stop the process and
dispose the runtime.

## Run it

```bash
pnpm --dir examples/js-dev-servers start
```
