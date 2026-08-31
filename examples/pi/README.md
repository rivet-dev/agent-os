---
title: "Pi Agent"
description: "Run the Pi coding agent in a session, including quickstart and session management."
category: "Agents"
order: 1
---

Spin up the Pi coding agent inside a VM, open a session, and send it prompts. Reach for this when you want an end-to-end agent loop from quickstart through the session knobs for skills and MCP servers.

## How it works

The server registers a VM with the `pi` software package and starts the registry. The client grabs a VM with `getOrCreate`, then calls `openSession({ agent: "pi", env: … })`, which infers the default `main` session and passes `ANTHROPIC_API_KEY` through `env`. From there `prompt({ content })` runs a turn and returns the agent's `text`. Drop a `SKILL.md` into the agent's skills directory before opening the session and it is auto-discovered; Pi reads MCP servers from its native `.mcp.json` config. Pre-install any `npx`-launched MCP server so install output does not corrupt the stdio handshake.

## Run it

```sh
npm install
ANTHROPIC_API_KEY=sk-... npx tsx server.ts   # then run the client in another shell
```

The agent answers the prompt and prints its response to the console.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/pi
