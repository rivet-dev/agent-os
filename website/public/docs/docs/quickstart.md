# Quickstart

Set up an agentOS actor, create a session, and run your first coding agent.

<span>Use this pre-built prompt to get started faster.</span>
<button type="button" onclick="var b=this;navigator.clipboard.writeText(b.getAttribute('data-prompt')||'').then(function(){b.textContent='Copied!';setTimeout(function(){b.textContent='Copy prompt';},1500);});" data-prompt={AGENT_PROMPT} style="appearance:none;border:1px solid rgba(27,25,22,0.18);background:#1b1916;color:#f4f1e7;font-family:var(--sl-font);font-size:0.8rem;font-weight:600;display:inline-flex;align-items:center;justify-content:center;height:2rem;padding:0 0.85rem;border-radius:6px;cursor:pointer;white-space:nowrap;margin-top:0;flex:none;box-sizing:border-box;">Copy prompt</button>

<span>Prefer to read code? Clone the example repository.</span>
<a href="https://github.com/rivet-dev/agentos/tree/main/examples/quickstart-app" style="appearance:none;border:1px solid rgba(27,25,22,0.18);background:transparent;color:#1b1916;font-family:var(--sl-font);font-size:0.8rem;font-weight:600;display:inline-flex;align-items:center;justify-content:center;height:2rem;padding:0 0.85rem;border-radius:6px;cursor:pointer;white-space:nowrap;text-decoration:none;flex:none;gap:0.45rem;box-sizing:border-box;"><svg viewBox="0 0 496 512" width="14" height="14" fill="currentColor" aria-hidden="true"><path d="M165.9 397.4c0 2-2.3 3.6-5.2 3.6-3.3.3-5.6-1.3-5.6-3.6 0-2 2.3-3.6 5.2-3.6 3-.3 5.6 1.3 5.6 3.6zm-31.1-4.5c-.7 2 1.3 4.3 4.3 4.9 2.6 1 5.6 0 6.2-2s-1.3-4.3-4.3-5.2c-2.6-.7-5.5.3-6.2 2.3zm44.2-1.7c-2.9.7-4.9 2.6-4.6 4.9.3 2 2.9 3.3 5.9 2.6 2.9-.7 4.9-2.6 4.6-4.6-.3-1.9-3-3.2-5.9-2.9zM244.8 8C106.1 8 0 113.3 0 252c0 110.9 69.8 205.8 169.5 239.2 12.8 2.3 17.3-5.6 17.3-12.1 0-6.2-.3-40.4-.3-61.4 0 0-70 15-84.7-29.8 0 0-11.4-29.1-27.8-36.6 0 0-22.9-15.7 1.6-15.4 0 0 24.9 2 38.6 25.8 21.9 38.6 58.6 27.5 72.9 20.9 2.3-16 8.8-27.1 16-33.7-55.9-6.2-112.3-14.3-112.3-110.5 0-27.5 7.6-41.3 23.6-58.9-2.6-6.5-11.1-33.3 2.6-67.9 20.9-6.5 69 27 69 27 20-5.6 41.5-8.5 62.8-8.5s42.8 2.9 62.8 8.5c0 0 48.1-33.6 69-27 13.7 34.7 5.2 61.4 2.6 67.9 16 17.7 25.8 31.5 25.8 58.9 0 96.5-58.9 104.2-114.8 110.5 9.2 7.9 17 22.9 17 46.4 0 33.7-.3 75.4-.3 83.6 0 6.5 4.6 14.4 17.3 12.1C428.2 457.8 496 362.9 496 252 496 113.3 383.5 8 244.8 8zM97.2 352.9c-1.3 1-1 3.3.7 5.2 1.6 1.6 3.9 2.3 5.2 1 1.3-1 1-3.3-.7-5.2-1.6-1.6-3.9-2.3-5.2-1zm-10.8-8.1c-.7 1.3.3 2.9 2.3 3.9 1.6 1 3.6.7 4.3-.7.7-1.3-.3-2.9-2.3-3.9-2-.6-3.6-.3-4.3.7zm32.4 35.6c-1.6 1.3-1 4.3 1.3 6.2 2.3 2.3 5.2 2.6 6.5 1 1.3-1.3.7-4.3-1.3-6.2-2.2-2.3-5.2-2.6-6.5-1zm-11.4-14.7c-1.6 1-1.6 3.6 0 5.9 1.6 2.3 4.3 3.3 5.6 2.3 1.6-1.3 1.6-3.9 0-6.2-1.4-2.3-4-3.3-5.6-2z"/></svg>View on GitHub</a>

      <text x="50" y="50" text-anchor="middle" dominant-baseline="central" font-family="var(--sl-font)" font-weight="700" font-size="38" fill="#1b1916">OS</text>
  <text x="82" y="92" text-anchor="middle" font-family="var(--sl-font)" font-size="15" font-weight="600" fill="#1b1916">Client</text>
  <text x="82" y="112" text-anchor="middle" font-family="var(--sl-font)" font-size="10.5" fill="#56524a">JS · Browser · Backend</text>
  <text x="224" y="62" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Server</text>
    <text x="153.5" y="178" text-anchor="middle" dominant-baseline="central" font-family="var(--sl-font)" font-weight="700" font-size="7" fill="#56524a">OS</text>
    <text x="170" y="178" dominant-baseline="central" font-family="var(--sl-font)" font-size="12" fill="#56524a">= agentOS VM</text>

1. **Install**

   - **@rivet-dev/agentos** — Actor framework with built-in persistence and orchestration
   - **@agentos-software/pi** — [Pi](https://github.com/mariozechner/pi-coding-agent) coding agent. [Claude Code](/docs/agents/claude), [Codex](/docs/agents/codex), and [OpenCode](/docs/agents/opencode) install the same way.

   ```bash
   npm install @rivet-dev/agentos @agentos-software/pi
   ```

2. **Create the server**

3. **Create the client**

   The client can be any public frontend or another backend. The same `vm` actor is reachable from a plain Node script, a browser/React app, or a separate server.

4. **Run it**

   Start the server, then run the client in a second terminal:

   ```bash
   # Terminal 1: start the server
   npx tsx server.ts

   # Terminal 2: run the client
   npx tsx client.ts
   ```

   With the server running, open http://localhost:6420/ui to watch the VM in the [inspector](/docs/inspector): the live transcript, the filesystem, and its processes.

5. **Customize**

   Now that you have a working agent, customize it to fit your needs:

   - **[Software](/docs/software)** — Install software packages inside the VM
   - **[Filesystem](/docs/filesystem)** — Read, write, and manage files inside the VM
   - **[Permissions & Resource Limits](/docs/permissions)** — Gate what the agent can do and cap its resource usage
   - **[Bindings](/docs/bindings)** — Expose your JavaScript functions to agents as CLI commands

5. **Deploy**

   By default, agentOS runs locally with `npx rivetkit dev` — no infrastructure needed. To run in production, deploy to any of these targets:

   See [Deployment](/docs/deployment) for managed, self-hosted, and direct VM options.

agentOS is in preview and the API is subject to change. If you run into issues, please [report them on GitHub](https://github.com/rivet-dev/rivet/issues) or [join our Discord](https://rivet.dev/discord).

## Direct VM API

The same `@rivet-dev/agentos` package also exports `AgentOs.create()` for direct
VM control without the actor lifecycle, multiplayer, or orchestration layer.

See the [direct VM API documentation](/docs/core) for reference.