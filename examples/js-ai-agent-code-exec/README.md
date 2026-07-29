---
title: "AI Agent Code Exec"
description: "Execute agent-generated code inside an isolated AgentOS VM."
---

Treat every generated program as hostile input. This example creates one
`AgentOs`, evaluates a generated expression with a timeout, validates
the structured result, and disposes the VM.

Use `evaluate()` for a JSON-serializable value, `execute()` for a complete
program whose output is the result, and `spawn()` for a long-running tool or
server. Configure permissions and VM resource limits before accepting untrusted
source.

## Run it

```bash
pnpm --dir examples/js-ai-agent-code-exec start
```
