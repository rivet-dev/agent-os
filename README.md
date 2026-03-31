# agentOS

**[Documentation](https://rivet.dev/docs/agent-os)** | **[Quickstart](https://rivet.dev/docs/agent-os/quickstart)** | **[agentOS vs Sandbox](https://rivet.dev/docs/agent-os/versus-sandbox)**

A portable open-source operating system for AI agents. Near-zero cold starts (~6 ms), up to 32x cheaper than sandboxes. Powered by WebAssembly and V8 isolates.

## Why agentOS

- **Runs inside your process**: No VMs to boot, no containers to pull. Agents start in milliseconds with minimal memory overhead.
- **Embeds in your backend**: Agents call your functions directly via [host tools](https://rivet.dev/docs/agent-os/tools). No network hops, no complex auth between services.
- **Granular security**: Deny-by-default permissions for filesystem, network, and process access. The same isolation technology trusted by browsers worldwide.
- **Deploy anywhere**: Just an npm package. Works on your laptop, Rivet Cloud, Railway, Vercel, Kubernetes, or any container platform.
- **Open source**: Apache 2.0 licensed. Self-host or use [Rivet Cloud](https://rivet.dev/docs/agent-os/deployment) for managed infrastructure.

## Benchmarks

All benchmarks compare agentOS against the fastest/cheapest mainstream sandbox providers as of March 2026.

### Cold start

| Percentile | agentOS | Fastest Sandbox (E2B) | Speedup |
|---|---|---|---|
| p50 | 4.8 ms | 440 ms | **92x faster** |
| p95 | 5.6 ms | 950 ms | **170x faster** |
| p99 | 6.1 ms | 3,150 ms | **516x faster** |

<sub>agentOS: median of 10,000 runs on Intel i7-12700KF. Sandbox: E2B.</sub>

### Memory per instance

| Workload | agentOS | Cheapest Sandbox (Daytona) | Reduction |
|---|---|---|---|
| Full coding agent (Pi + MCP + filesystem) | ~131 MB | ~1,024 MB | **8x smaller** |
| Simple shell command | ~22 MB | ~1,024 MB | **47x smaller** |

<sub>Sandbox baseline: Daytona minimum (1 vCPU + 1 GiB RAM).</sub>

### Cost per execution (self-hosted)

| Hardware | Cost/sec (agent workload) | vs Sandbox | 
|---|---|---|
| AWS ARM | ~$0.0000032/s | **6x cheaper** |
| AWS x86 | ~$0.0000053/s | **3x cheaper** |
| Hetzner ARM | ~$0.0000011/s | **17x cheaper** |
| Hetzner x86 | ~$0.0000013/s | **14x cheaper** |

<sub>Sandbox baseline: Daytona at $0.0504/vCPU-h + $0.0162/GiB-h. Self-hosted assumes 70% utilization.</sub>

## Features

### Agents
- **Multi-agent support**: Run Claude Code, Codex, OpenCode, Amp, Pi, and more with a unified API
- **[Sessions via ACP](https://rivet.dev/docs/agent-os/sessions)**: Create, manage, and resume agent sessions over the [Agent Communication Protocol](https://agentclientprotocol.com)
- **Universal transcript format**: One transcript format across all agents for debugging, auditing, and comparison
- **[Automatic persistence](https://rivet.dev/docs/agent-os/persistence)**: Every conversation is saved and replayable without extra code

### Infrastructure
- **[Mount anything as a filesystem](https://rivet.dev/docs/agent-os/filesystem)**: S3, Google Drive, SQLite, host directories, or custom backends
- **[Host tools](https://rivet.dev/docs/agent-os/tools)**: Define JavaScript functions that agents call as CLI commands inside the VM
- **[Cron](https://rivet.dev/docs/agent-os/cron), [webhooks](https://rivet.dev/docs/agent-os/webhooks), and [queues](https://rivet.dev/docs/agent-os/queues)**: Schedule tasks, receive external events, and serialize work with built-in primitives
- **[Sandbox extension](https://rivet.dev/docs/agent-os/sandbox)**: Pair with full sandboxes (E2B, Daytona, etc.) for heavy workloads like browsers or native compilation

### Orchestration
- **[Multiplayer](https://rivet.dev/docs/agent-os/multiplayer)**: Multiple clients observe and collaborate with the same agent in real time
- **[Agent-to-agent](https://rivet.dev/docs/agent-os/agent-to-agent)**: Agents delegate work to other agents through host-defined tools
- **[Workflows](https://rivet.dev/docs/agent-os/workflows)**: Chain agent tasks into durable workflows with retries, branching, and resumable execution
- **[Authentication](https://rivet.dev/docs/agent-os/authentication)**: Integrate with your existing auth model (API keys, OAuth, JWTs)

### Security
- **[Deny-by-default permissions](https://rivet.dev/docs/agent-os/security)**: Granular control over filesystem, network, process, and environment access
- **[Programmatic network control](https://rivet.dev/docs/agent-os/networking)**: Allow, deny, or proxy any outbound connection
- **[Resource limits](https://rivet.dev/docs/agent-os/security)**: Set precise CPU and memory limits per agent
- **[V8 + WebAssembly isolation](https://rivet.dev/docs/agent-os/architecture)**: Each agent runs in its own isolate with no shared state

## Architecture

agentOS is built on an in-process operating system kernel written in JavaScript. Three runtimes mount into the kernel:

- **WebAssembly**: POSIX utilities (coreutils, grep, sed, etc.) compiled to WASM
- **V8 isolates**: JavaScript/TypeScript agent code runs in sandboxed V8 contexts
- **Pyodide**: Python workloads via CPython compiled to WASM

The kernel manages a virtual filesystem, process table, pipes, PTYs, and a virtual network stack. Everything runs inside the kernel -- nothing executes on the host.

See the [Architecture docs](https://rivet.dev/docs/agent-os/architecture) for details.

## Quick start

```bash
npm install rivetkit @rivet-dev/agent-os-common @rivet-dev/agent-os-pi
```

**server.ts**

```ts
import { agentOs } from "rivetkit/agent-os";
import { setup } from "rivetkit";
import common from "@rivet-dev/agent-os-common";
import pi from "@rivet-dev/agent-os-pi";

const vm = agentOs({
  options: { software: [common, pi] },
});

export const registry = setup({ use: { vm } });
registry.start();
```

**client.ts**

```ts
import { createClient } from "rivetkit/client";
import type { registry } from "./server";

const client = createClient<typeof registry>("http://localhost:6420");
const agent = client.vm.getOrCreate(["my-agent"]);

// Subscribe to streaming events
agent.on("sessionEvent", (data) => {
  console.log(data.event);
});

// Create a session and send a prompt
const session = await agent.createSession("pi", {
  env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await agent.sendPrompt(
  session.sessionId,
  "Write a hello world script to /home/user/hello.js",
);

// Read the file the agent created
const content = await agent.readFile("/home/user/hello.js");
console.log(new TextDecoder().decode(content));
```

See the [Quickstart guide](https://rivet.dev/docs/agent-os/quickstart) for the full walkthrough.

## Registry

Browse pre-built agents, tools, filesystems, and software packages at the [agentOS Registry](https://rivet.dev/agent-os/registry).

<!-- BEGIN PACKAGE TABLE -->
### WASM Command Packages

| Package | apt Equivalent | Description | Source | Combined Size | Gzipped |
|---------|---------------|-------------|--------|---------------|---------|
| `@rivet-dev/agent-os-codex` | codex | OpenAI Codex integration (codex, codex-exec) | rust | - | - |
| `@rivet-dev/agent-os-coreutils` | coreutils | GNU coreutils: sh, cat, ls, cp, sort, and 80+ commands | rust | - | - |
| `@rivet-dev/agent-os-curl` | curl | curl HTTP client | c | - | - |
| `@rivet-dev/agent-os-diffutils` | diffutils | GNU diffutils (diff) | rust | - | - |
| `@rivet-dev/agent-os-fd` | fd-find | fd fast file finder | rust | - | - |
| `@rivet-dev/agent-os-file` | file | file type detection | rust | - | - |
| `@rivet-dev/agent-os-findutils` | findutils | GNU findutils (find, xargs) | rust | - | - |
| `@rivet-dev/agent-os-gawk` | gawk | GNU awk text processing | rust | - | - |
| `@rivet-dev/agent-os-git` | git | git version control *(planned)* | rust | - | - |
| `@rivet-dev/agent-os-grep` | grep | GNU grep pattern matching (grep, egrep, fgrep) | rust | - | - |
| `@rivet-dev/agent-os-gzip` | gzip | GNU gzip compression (gzip, gunzip, zcat) | rust | - | - |
| `@rivet-dev/agent-os-jq` | jq | jq JSON processor | rust | - | - |
| `@rivet-dev/agent-os-make` | make | GNU make build tool *(planned)* | rust | - | - |
| `@rivet-dev/agent-os-ripgrep` | ripgrep | ripgrep fast recursive search | rust | - | - |
| `@rivet-dev/agent-os-sed` | sed | GNU sed stream editor | rust | - | - |
| `@rivet-dev/agent-os-sqlite3` | sqlite3 | SQLite3 command-line interface | c | - | - |
| `@rivet-dev/agent-os-tar` | tar | GNU tar archiver | rust | - | - |
| `@rivet-dev/agent-os-tree` | tree | tree directory listing | rust | - | - |
| `@rivet-dev/agent-os-unzip` | unzip | unzip archive extraction | c | - | - |
| `@rivet-dev/agent-os-wget` | wget | GNU wget HTTP client | c | - | - |
| `@rivet-dev/agent-os-yq` | yq | yq YAML/JSON processor | rust | - | - |
| `@rivet-dev/agent-os-zip` | zip | zip archive creation | c | - | - |

### Meta-Packages

| Package | Description | Includes |
|---------|-------------|----------|
| `@rivet-dev/agent-os-build-essential` | Build-essential WASM command set (standard + make + git + curl) | standard, make, git, curl |
| `@rivet-dev/agent-os-common` | Common WASM command set (coreutils + sed + grep + gawk + findutils + diffutils + tar + gzip) | coreutils, sed, grep, gawk, findutils, diffutils, tar, gzip |
<!-- END PACKAGE TABLE -->

## License

Apache-2.0
