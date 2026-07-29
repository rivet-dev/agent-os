<p align="center">
  <img src=".github/media/banner.png" alt="agentOS" />
</p>

<p align="center">
  <b>Give agents an operating system as a library.</b><br/>
  Each agent gets a lightweight OS with filesystem, execution, and orchestration.<br/>
  Runs in your existing backend – no sandboxes, VMs, or SaaS.
</p>

<p align="center">
  <i>92× faster cold starts, 47× less memory, 254× cheaper (<a href="#benchmarks">source</a>)</i>
</p>

<p align="center">
  <a href="https://agentos-sdk.dev/docs">Documentation</a> —
  <a href="https://agentos-sdk.dev/docs/quickstart">Quickstart</a> —
  <a href="https://agentos-sdk.dev/registry">Registry</a> —
  <a href="https://rivet.dev/discord">Discord</a>
</p>

## Quick start

**1. Install agentOS and the agents you want**

```bash
npm install @rivet-dev/agentos

# Install the agent you want to run in agentOS
npm install @agentos-software/pi           # Pi
npm install @agentos-software/claude-code  # Claude Code (beta)
npm install @agentos-software/codex        # Codex (beta)
npm install @agentos-software/opencode     # OpenCode
```

See more Linux software in the [registry](https://agentos-sdk.dev/registry). Also supports [Flue](https://agentos-sdk.dev/docs/frameworks/flue), [Eve](https://agentos-sdk.dev/docs/frameworks/vercel-eve), and [custom agents](https://agentos-sdk.dev/docs/agents/custom).

**2. Set up the server**

```ts
// server.ts
import { agentOS, setup } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

const vm = agentOS({
  software: [pi],
});

export const registry = setup({ use: { vm } });
registry.start();
```

**3. Connect to agentOS**

```ts
// client.ts
import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>("http://localhost:6420");
const vm = client.vm.getOrCreate("my-agent");

// Subscribe to streaming events.
const conn = vm.connect();
conn.on("sessionEvent", (event) => {
  console.log(event);
});

// Open a durable session and send a prompt.
await vm.sessions.open({
  agent: "pi",
  env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await vm.sessions.prompt({
  content: [
    { type: "text", text: "Write a hello world script to /workspace/hello.js" },
  ],
});

// Read the file the agent created.
const content = await vm.filesystem.readFile("/workspace/hello.js");
console.log(new TextDecoder().decode(content));
```

```bash
npx tsx server.ts   # terminal 1
npx tsx client.ts   # terminal 2
```

Deploy it wherever your backend already runs, on [Rivet Cloud](https://dashboard.rivet.dev), or
self-hosted on Kubernetes, VMs, or bare metal. See [Deploy](https://agentos-sdk.dev/docs/deployment).

**Alternative: direct VM API**

Instead of a client-server architecture, install `@rivet-dev/agentos-core` and boot a VM
directly inline:

```ts
import { AgentOs } from "@rivet-dev/agentos-core";
import pi from "@agentos-software/pi";

const vm = await AgentOs.create({ software: [pi] });

const result = await vm.process.exec("echo hello");
console.log(result.stdout); // "hello\n"
```

## The operating system

A user-space kernel with its own filesystem, networking, and processes. No nested virtualization
like microVMs, no elevated privileges like gVisor.

### Execution

[Bash](https://agentos-sdk.dev/docs/bash) ·
[Node.js](https://agentos-sdk.dev/docs/javascript) ·
[Python](https://agentos-sdk.dev/docs/python)

Run Bash, Node.js, and Python with real processes, shells, and servers.

```ts
// Bash
const ls = await vm.process.exec("ls -la /workspace", {
  output: { capture: "all" },
});
console.log(ls.stdout);

// JavaScript
const sum = await vm.javascript.evaluate<number>("1 + 2");
console.log(sum.value); // 3

// Python
const answer = await vm.python.evaluate<number>("21 * 2");
console.log(answer.value); // 42
```

### Filesystem

[Filesystem](https://agentos-sdk.dev/docs/filesystem) ·
[Software](https://agentos-sdk.dev/docs/software) ·
[Persistence & sleep](https://agentos-sdk.dev/docs/persistence)

Every VM gets a full POSIX-compliant filesystem. Mount S3, Google Drive, or a host directory for
persistence.

```ts
// Mount S3 at a normal path.
const vm = agentOS({
  software: [pi],
  mounts: [
    {
      path: "/workspace",
      plugin: { id: "s3", config: { bucket: "my-bucket", region: "us-east-1" } },
    },
  ],
});

// Read and write it like any other file.
await vm.filesystem.writeFile("/workspace/config.json", JSON.stringify({ key: "value" }));
const content = await vm.filesystem.readFile("/workspace/config.json");
console.log(new TextDecoder().decode(content));
```

### Orchestration

[Workflows & graphs](https://agentos-sdk.dev/docs/workflows) ·
[Multiplayer](https://agentos-sdk.dev/docs/multiplayer) ·
[Agent-to-agent](https://agentos-sdk.dev/docs/agent-to-agent) ·
[Crons & loops](https://agentos-sdk.dev/docs/cron) ·
[Approvals](https://agentos-sdk.dev/docs/approvals) ·
[Apps](https://agentos-sdk.dev/docs/apps)

VMs are durable and can be orchestrated to create complex multi-agent patterns.

```ts
import { actor } from "rivetkit";
import { workflow } from "rivetkit/workflow";

// Each created actor is one durable workflow run. Steps checkpoint and resume.
const bugFixer = actor({
  run: workflow(async (ctx) => {
    await ctx.step("clone-repo", () =>
      vm.process.exec("git clone https://github.com/acme/api /home/agentos/repo"),
    );

    await ctx.step("fix-bug", () =>
      vm.sessions.prompt({
        content: [{ type: "text", text: "Fix the failing test in /home/agentos/repo" }],
      }),
    );

    // A second VM reviews the work, isolated from the one that wrote it.
    await ctx.step("review", () =>
      reviewer.sessions.prompt({
        content: [{ type: "text", text: "Review the diff in /home/agentos/repo" }],
      }),
    );
  }),
});
```

## Apps (preview)

Deploy AI-generated applications for your users. Supports use cases like HTTP servers, websites,
SQLite, workflows, and multiplayer.

Optionally works with [Rivet Actors](https://rivet.dev/docs/actors).

```ts
import { serve } from "@hono/node-server";
import { appsRouter, deployApp } from "@rivet-dev/agentos-apps";
import { Hono } from "hono";

// An agent, an upload endpoint, or anything else can deploy the files it generated.
await deployApp({
  appId: "hello-world",
  files: {
    "package.json": JSON.stringify({ name: "hello-world-app", type: "module", main: "src/index.ts" }),
    "src/index.ts": `
      import { Hono } from "hono";
      const app = new Hono();
      app.get("/", (c) => c.html("<h1>Hello from agentOS Apps</h1>"));
      export default app;
    `,
  },
});

// Mount every deployed application at /apps/:appId.
const server = new Hono();
server.route("/apps", appsRouter);
serve({ fetch: server.fetch, port: 3000 });
```

[Documentation](https://agentos-sdk.dev/docs/apps)

## Architecture

<p align="center">
  <img src=".github/media/architecture.svg" alt="A VM split into a kernel and an executor. The kernel owns the virtual filesystem, process table, socket table, pipes, PTYs, DNS, and permission policy. The executor runs guest JavaScript, WASM, and native binaries, and reaches the kernel through syscalls." width="680" />
</p>

- **[Overview](https://agentos-sdk.dev/docs/architecture)**: the full tour of how the pieces fit
  together.
- **[Kernel](https://agentos-sdk.dev/docs/architecture/posix-syscalls)**: the trusted core. It owns
  the virtual filesystem, process table, socket table, pipes, PTYs, and DNS, and every guest
  operation goes through it.
- **[Executor](https://agentos-sdk.dev/docs/architecture/javascript-executor)**: untrusted. Guest
  JavaScript runs on native V8, compiled tools run as WebAssembly, and neither holds a real
  capability of its own.
- **[Processes](https://agentos-sdk.dev/docs/architecture/processes)**: real `fork`/`exec`, signals,
  subprocesses, and a shell, so programs written for Linux run unmodified.
- **[Filesystem](https://agentos-sdk.dev/docs/architecture/filesystem)**: a snapshot root plus a
  write overlay, with mounts grafted onto guest paths.
- **[Networking](https://agentos-sdk.dev/docs/architecture/networking)**: a virtual socket table and
  DNS, with egress denied by default.
- **[Permissions](https://agentos-sdk.dev/docs/permissions)**: enforced on every syscall, with
  [approvals](https://agentos-sdk.dev/docs/approvals) to pause a turn for a human.
- **[Sessions](https://agentos-sdk.dev/docs/architecture/agent-sessions)**: an agent is just another
  guest process; a session keeps it alive across prompts and streams its output as events.
- **[Actors](https://agentos-sdk.dev/docs/persistence)**: each VM is a Rivet Actor, which is where
  durable state, sleep/wake, cron, and workflows come from.
- **[Security model](https://agentos-sdk.dev/docs/security-model)**: the trust boundary, what is in
  scope, and what is not.

## Benchmarks

Measured against the fastest and cheapest mainstream sandbox providers as of March 30, 2026.

| | agentOS | Sandbox | |
|---|---|---|---|
| Cold start (p50) | 4.8 ms | 440 ms (E2B) | **92× faster** |
| Memory per instance | ~22 MB | ~1,024 MB (Daytona) | **47× smaller** |
| Cost per execution-second | $0.000000073/s | $0.000018/s (Daytona) | **254× cheaper** |

<sub>agentOS cold start measured on Intel i7-12700KF. Memory and cost use the shell workload; a full
coding agent (Pi + MCP + filesystem) is ~131 MB. Cost is self-hosted on AWS ARM at 70% utilization
against Daytona's 1 vCPU + 1 GiB minimum.</sub>

Methodology and reproduction: [Performance](https://agentos-sdk.dev/docs/performance)

## agentOS vs Sandboxes

| | agentOS | Sandbox |
|---|---|---|
| **Runs** | Inside your backend process | Vendor account plus API keys |
| **Startup** | Single-digit ms | Seconds |
| **Cost** | Whatever your process already costs | Per second of uptime |
| **Backend integration** | Direct, via [bindings](https://agentos-sdk.dev/docs/bindings) | Network calls back to your backend |
| **Credentials** | Stay on the host | Injected into the sandbox |
| **Permissions** | Granular, deny by default | Container-level |
| **Best for** | Coding, scripting, API calls, orchestration | x86-specific software, resource-intensive applications |

The two compose: [sandbox mounting](https://agentos-sdk.dev/docs/sandbox) spins up a sandbox on
demand and mounts its filesystem into the VM.

[agentOS vs Sandbox](https://agentos-sdk.dev/docs/versus-sandbox) ·
[Limitations](https://agentos-sdk.dev/docs/limitations)

## Documentation

**Getting Started**:
[Quick Start](https://agentos-sdk.dev/docs/quickstart) ·
[Crash Course](https://agentos-sdk.dev/docs/crash-course)

**Agents**:
[Pi](https://agentos-sdk.dev/docs/agents/pi) ·
[Claude Code](https://agentos-sdk.dev/docs/agents/claude) ·
[Codex](https://agentos-sdk.dev/docs/agents/codex) ·
[OpenCode](https://agentos-sdk.dev/docs/agents/opencode) ·
[Flue](https://agentos-sdk.dev/docs/frameworks/flue) ·
[Eve](https://agentos-sdk.dev/docs/frameworks/vercel-eve)

**Execution**:
[Bash](https://agentos-sdk.dev/docs/bash) ·
[Node.js](https://agentos-sdk.dev/docs/javascript) ·
[Python](https://agentos-sdk.dev/docs/python)

**Orchestration**:
[Apps](https://agentos-sdk.dev/docs/apps) ·
[Multiplayer](https://agentos-sdk.dev/docs/multiplayer) ·
[Workflows](https://agentos-sdk.dev/docs/workflows) ·
[Crons](https://agentos-sdk.dev/docs/cron) ·
[Agent-to-Agent](https://agentos-sdk.dev/docs/agent-to-agent)

**Operating System**:
[Software](https://agentos-sdk.dev/docs/software) ·
[Filesystem](https://agentos-sdk.dev/docs/filesystem) ·
[Networking](https://agentos-sdk.dev/docs/networking) ·
[Permissions](https://agentos-sdk.dev/docs/permissions) ·
[Resource Limits](https://agentos-sdk.dev/docs/resource-limits)

**Extension**:
[Custom Bindings](https://agentos-sdk.dev/docs/bindings) ·
[Browser Automation](https://agentos-sdk.dev/docs/browser) ·
[External Sandboxes](https://agentos-sdk.dev/docs/sandboxes)

**Reference**:
[Deploy](https://agentos-sdk.dev/docs/deployment) ·
[Custom Software](https://agentos-sdk.dev/docs/custom-software/definition)

**Architecture**:
[Overview](https://agentos-sdk.dev/docs/architecture) ·
[Security Model](https://agentos-sdk.dev/docs/security-model) ·
[Limitations](https://agentos-sdk.dev/docs/limitations)

**More**:
[Sessions & Transcripts](https://agentos-sdk.dev/docs/sessions) ·
[Approvals](https://agentos-sdk.dev/docs/approvals) ·
[Models & Credentials](https://agentos-sdk.dev/docs/models-and-credentials) ·
[Authentication](https://agentos-sdk.dev/docs/authentication) ·
[Persistence & Sleep](https://agentos-sdk.dev/docs/persistence) ·
[Direct VM API](https://agentos-sdk.dev/docs/core) ·
[Debugging](https://agentos-sdk.dev/docs/debugging)

## License

Apache-2.0
