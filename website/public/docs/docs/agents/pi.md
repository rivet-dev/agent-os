# Pi

Run the Pi coding agent inside a VM with extensions and custom configuration.

## Quick start

Read [Sessions](/docs/sessions) first for session options, streaming events, prompts, and lifecycle management.

## Model & credentials

Set the relevant variable on the session's `env`, sourced from your server's environment:

- `ANTHROPIC_API_KEY` — Anthropic (Claude), the default.
- Other providers — use the provider-named key (e.g. `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`).

See [Models & Credentials](/docs/models-and-credentials), and Pi's [providers docs](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/providers.md) for the full list.

## Skills

Pi discovers `SKILL.md` files from its skills directory. Write the skill into the VM before creating a session and Pi loads it automatically.

## MCP servers

Expose extra tools to the agent by passing `mcpServers` to `openSession`. Both local child-process servers and remote URLs are supported.

**Pre-install `npx`-launched servers.** A local server started with `npx -y …` writes install progress to **stdout** on its first run, which corrupts the MCP stdio handshake (you'll see `Connection closed`). Pre-install it in the VM so `npx` is silent — `await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem")` before the session — or pin the package and point `command` at the installed binary.

## Extensions

Pi supports [extensions](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent/examples/extensions) that let you register custom tools, modify the system prompt, and hook into agent lifecycle events. Write a `.js` file into the VM's extensions directory before creating a session and Pi discovers it automatically.

Pi scans two directories for `.js` extension files:

| Directory | Scope |
|-----------|-------|
| `~/.pi/agent/extensions/` | Global — applies to all Pi sessions |
| `<cwd>/.pi/extensions/` | Project — applies only when cwd matches |

See the [Pi extension documentation](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent/examples/extensions) for the full extension API.

## Customizing the agent

Pi is a built-in agent, but it's just a software package under the hood. To ship your own ACP adapter, swap the underlying agent SDK, or register a tweaked Pi build as a new agent, see [Custom Agents](/docs/agents/custom).