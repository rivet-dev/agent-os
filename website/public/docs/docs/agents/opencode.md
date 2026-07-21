# OpenCode

Run the OpenCode coding agent inside a VM with skills, MCP servers, and custom configuration.

## Quick start

Read [Sessions](/docs/sessions) first for session options, streaming events, prompts, and lifecycle management.

## Model & credentials

OpenCode auto-detects a provider when its key is present on the session's `env`, sourced from your server's environment. Common variables:

- `ANTHROPIC_API_KEY` — Anthropic (Claude), the default.
- `OPENAI_API_KEY` — OpenAI.
- `OPENROUTER_API_KEY` — OpenRouter.
- `GEMINI_API_KEY` — Google Gemini.
- `GROQ_API_KEY` — Groq.
- …plus Amazon Bedrock, Azure, Google Vertex, and 70+ providers via [models.dev](https://models.dev).

See [Models & Credentials](/docs/models-and-credentials), and OpenCode's [providers docs](https://opencode.ai/docs/providers/) for the full list.

## Model configuration

To pin a specific model — or point a provider at a custom endpoint — write an OpenCode config file into the VM before creating the session. OpenCode reads `<HOME>/.config/opencode/opencode.json` (the agent's `HOME` is `/home/agentos` by default).

Two settings will silently produce an **empty response** if wrong:
- The Anthropic provider **`baseURL` must end in `/v1`** (`https://api.anthropic.com/v1`). Without `/v1`, OpenCode calls `…/messages` and Anthropic returns `404 Not Found`.
- The **`model` must be a current model id.** A retired id returns a `404 not_found_error` and the turn ends with zero output.

```ts
// Write the config before creating the session
await agent.filesystem.mkdir("/home/agentos/.config/opencode", { recursive: true });
await agent.filesystem.writeFile(
  "/home/agentos/.config/opencode/opencode.json",
  JSON.stringify({
    $schema: "https://opencode.ai/config.json",
    model: "anthropic/claude-haiku-4-5-20251001", // use a current model id
    provider: {
      // The Anthropic baseURL MUST include /v1, or requests 404.
      anthropic: { options: { baseURL: "https://api.anthropic.com/v1" } },
    },
  }),
);

await agent.sessions.open({
  agent: "opencode",
  env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
```

## Skills

OpenCode discovers `SKILL.md` files from its skills directory. Write the skill into the VM before creating a session and OpenCode loads it automatically.

## MCP servers

Expose extra tools to the agent by passing `mcpServers` to `openSession`. Both local child-process servers and remote URLs are supported.

**Pre-install `npx`-launched servers.** A local server started with `npx -y …` writes install progress to **stdout** on its first run, which corrupts the MCP stdio handshake (you'll see `Connection closed`). Pre-install it in the VM so `npx` is silent — `await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem")` before the session — or pin the package and point `command` at the installed binary.

## Customizing the agent

OpenCode is a built-in agent, but it's just a software package under the hood. To ship your own ACP adapter, swap the underlying agent SDK, or register a tweaked build as a new agent, see [Custom Agents](/docs/agents/custom).