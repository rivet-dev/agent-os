---
title: "Flue"
description: "Run a Flue agent with a durable agentOS sandbox."
category: "Integrations"
order: 2
---

# Flue

Run a Flue agent with agentOS as its sandbox. Flue owns the agent runtime;
agentOS supplies the isolated VM and durable `/workspace` filesystem.

## Run it

```sh
pnpm install
npx flue run assistant --id local \
  --input '{"message":"Write hello from Flue to /workspace/hello.txt, run wc -c /workspace/hello.txt, then read the file back."}'
```

Set `ANTHROPIC_API_KEY` in `.env` before running the agent.

## Configuration

- Change the actor name passed to `agentOSSandbox()` when your registry uses a name other than `vm`.
- Configure software, permissions, and resource limits on `agentOS()` in `actors.ts`.
- Keep files that must persist under `/workspace`.

See the [Flue integration guide](https://agentos-sdk.dev/docs/frameworks/flue)
for the complete setup.

## Source

View the source on GitHub: https://github.com/rivet-dev/agentos/tree/main/examples/flue
