---
title: "Codex Agent"
description: "Run the Codex agent in a session using an OpenAI API key."
category: "Agents"
order: 3
---

# Codex Agent

Run OpenAI's Codex agent inside a VM session and prompt it with natural language. Reach for this when you want a coding agent that can read and act on the VM's filesystem, backed by your own OpenAI API key.

## How it works

Register the `codex` software with `agentOS({ software: [codex] })` so the VM knows how to launch the agent. The client calls `agent.sessions.open({ agent: "codex", ... })`, which infers the default `main` session, passes `OPENAI_API_KEY` through `env`, then drives the agent with `agent.sessions.prompt({ content })`. Two optional extensions build on the same flow: drop a `SKILL.md` into `/home/agentos/.codex/skills/` before opening the session and the agent auto-discovers it, or configure MCP servers in Codex's native config file.

## Run it

```bash
npm install
export OPENAI_API_KEY=sk-...
npx tsx server.ts   # starts the registry on http://localhost:6420
npx tsx client.ts   # creates a Codex session and prints the agent's reply
```

You should see the agent describe the files in its working directory.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/codex
