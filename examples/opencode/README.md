---
title: "OpenCode Agent"
description: "Run the OpenCode agent in a session using an Anthropic API key."
category: "Agents"
order: 4
---

Spin up the OpenCode coding agent inside a VM session and prompt it with natural language. Reach for this when you want an autonomous coding agent that can read files, run commands, and follow project conventions — backed by your own Anthropic API key.

## How it works

Register the `opencode` software with `agentOS({ software: [opencode] })` so the runtime knows the agent type. The client calls `agent.sessions.open({ agent: "opencode", ... })`, which infers the default `main` session, passes `ANTHROPIC_API_KEY` through `env`, and then drives the agent with `agent.sessions.prompt`. The example also shows two extension points: drop a `SKILL.md` into `~/.config/opencode/skills/` before opening the session and the agent auto-discovers it, and wire in extra tools through native ACP `mcpServers`. Pre-install any `npx` MCP server first so install output does not corrupt the stdio handshake.

## Run it

```bash
npm install
export ANTHROPIC_API_KEY=sk-ant-...
npx tsx server.ts   # starts the registry on http://localhost:6420
npx tsx client.ts   # creates a session and prints the agent's reply
```

The agent answers your prompt — e.g. listing the files in the working directory.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/opencode
