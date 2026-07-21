---
title: "Code Mode"
description: "Let generated code orchestrate narrow host bindings in one VM process."
---

Code Mode gives an LLM one execution tool instead of exposing every host tool
directly. This example registers a Zod-validated weather binding, lets a
generated expression invoke it more than once, and returns one structured
result.

Binding handlers run in the trusted host. Only validated input and JSON output
cross the AgentOS boundary, so credentials and direct host resources stay out
of generated code.

## Run it

```bash
pnpm --dir examples/js-code-mode start
```
