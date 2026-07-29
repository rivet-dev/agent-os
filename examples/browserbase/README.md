# Browserbase example

Read the web from an agentOS VM using the Browserbase [`browse`](https://docs.browserbase.com) CLI.

`browse cloud fetch <url>` retrieves a page through the Browserbase cloud — the page is rendered by a
real browser in Browserbase's infrastructure and returned as JSON with the page content as clean
markdown, so the VM never runs a local browser and no sandbox is required. The CLI ships as the
`@agentos-software/browserbase` package and is exposed inside the VM as the `browse` command.

`server.ts` defines the agentOS VM (with the Claude Code agent and the `browse` CLI). `client.ts` connects
to it and shows both ways to use `browse`: running the CLI yourself through the VM's process API, then
letting a Claude Code agent use it. For the agent, the server mounts the local `skills/` folder into Claude
Code's skills directory (`~/.claude/skills`). It holds the [`browse` CLI skill](https://github.com/browserbase/stagehand/tree/main/packages/cli)
that Browserbase bundles (the one `browse skills install` installs), copied verbatim, so the agent
discovers `browse` and reaches for it on its own; the prompt never mentions it.

## Setup

Get an API key and project id from the [Browserbase dashboard](https://www.browserbase.com/settings), then:

```bash
export BROWSERBASE_API_KEY=bb_...
export BROWSERBASE_PROJECT_ID=...
export ANTHROPIC_API_KEY=sk-ant-...   # for the Claude Code agent (client.ts only)
```

## Run

Start the server, then run a client against it:

```bash
pnpm start          # start the agentOS server (server.ts)
pnpm client         # run browse directly, then let a Claude Code agent use it
```

## Interactive browsing

`browse`'s interactive driver commands (`browse open`, `browse snapshot`, `browse click`, …) drive a
live browser session step by step. That mode runs a local driver daemon that the in-VM runtime does
not host — for interactive, multi-step browser automation, run `browse` inside a full sandbox via
[External Sandboxes](https://agentos-sdk.dev/docs/sandboxes). The `browse cloud` commands used here need
no daemon and run directly in the VM.
