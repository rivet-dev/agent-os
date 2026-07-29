# Sandbox Mounting

Extend agentOS with full sandboxes for heavy workloads like browsers, desktop automation, and compilation.

For heavy workloads like browsers, desktop automation, and compilation, pair agentOS with a full sandbox on demand. Its filesystem mounts into the VM as a native directory, and its process management is exposed as [bindings](/docs/bindings), all provider-agnostic through [Sandbox Agent](https://sandboxagent.dev).

## Why use agentOS with a sandbox?

agentOS is an alternative to sandboxes that covers most use cases, but some workloads need a full sandbox for special kinds of software (browsers, desktop automation, heavy compilation). Sandbox mounting lets you lazily start a sandbox on demand, only when it is needed, and project it into the VM. The hybrid model means one agent session can handle both lightweight coding tasks and heavy system operations, using the right tool for each.

See [agentOS vs Sandbox](/docs/versus-sandbox) for a detailed comparison.

## When to use a sandbox

- **Native binaries** not yet supported in the agentOS runtime.
- **Browsers and desktop automation**: Playwright, Puppeteer, Selenium, or anything that needs a display server.
- **Heavy compilation**: Large builds or native toolchains that require a full Linux environment.
- **GUI applications**: Desktop apps, VNC sessions, or any workload that needs a graphical environment.
- **Node.js packages with native extensions** (e.g. `sharp`, `bcrypt`, `better-sqlite3`) that require a full build toolchain.

Start with the default agentOS VM for all workloads, and only spin up a sandbox when a task genuinely requires one. Sandboxes are billed per second of uptime, so start them on demand and tear them down when the task is done.

## Getting started

The sandbox integration ships as the `@rivet-dev/agentos-sandbox` package. It works through two mechanisms:

- **Filesystem mount**: Projects the sandbox into the VM as a native directory, like mounting a hard drive on your own machine. Read and write files through the mount directly.
- **Bindings**: Exposes sandbox process management as [bindings](/docs/bindings). Execute commands on the sandbox from within the VM.

Both are powered by [Sandbox Agent](https://sandboxagent.dev), and you can swap providers without changing agent code:

```bash
npm install @rivet-dev/agentos-sandbox
```

Pass a provider as `sandbox: { provider: docker() }`. AgentOS starts a fresh sandbox for each VM, mounts it at `/mnt/sandbox`, registers the process bindings, and destroys it when the VM is disposed.

## Configuration

The default VM mount is `/mnt/sandbox`. Change the VM path with `mountPath`, scope it to a directory inside the external sandbox with `sandboxRoot`, or make the mount read-only:

```ts
const vm = agentOS({
	sandbox: {
		provider: docker(),
		mountPath: "/workspace",
		sandboxRoot: "/app",
		readOnly: true,
		timeoutMs: 30_000,
	},
});
```

## Calling the mounted bindings

Once the sandbox is mounted, write code through the filesystem and run it inside the sandbox. The sandbox bindings are exposed inside the VM as a CLI command, so you call it through the same `exec`/`spawn` surface as any other command.

## Bindings reference

The bindings expose these commands inside the VM:

```bash
# Run a command synchronously
agentos-sandbox run-command --command "npm install" --cwd "/app"

# Start a background process
agentos-sandbox create-process --command "npm" --args "run" --args "dev"

# List running processes
agentos-sandbox list-processes

# Get process output
agentos-sandbox get-process-logs --id "proc_abc123"

# Stop or kill a process
agentos-sandbox stop-process --id "proc_abc123"
agentos-sandbox kill-process --id "proc_abc123"

# Send input to an interactive process
agentos-sandbox send-input --id "proc_abc123" --data "yes"
```

## Sandbox providers

The examples use Docker. Every Sandbox Agent provider is available through an isolated entrypoint, so you only install the SDK for the provider you use:

```ts
// Docker is also available from "@rivet-dev/agentos-sandbox/docker".
import { docker } from "@rivet-dev/agentos-sandbox";
import { local } from "@rivet-dev/agentos-sandbox/local";
import { e2b } from "@rivet-dev/agentos-sandbox/e2b";
import { daytona } from "@rivet-dev/agentos-sandbox/daytona";
import { vercel } from "@rivet-dev/agentos-sandbox/vercel";
import { cloudflare } from "@rivet-dev/agentos-sandbox/cloudflare";
import { modal } from "@rivet-dev/agentos-sandbox/modal";
import { computesdk } from "@rivet-dev/agentos-sandbox/computesdk";
import { sprites } from "@rivet-dev/agentos-sandbox/sprites";

const vm = agentOS({ sandbox: { provider: docker() } });
```

Install the matching provider SDK for E2B (`@e2b/code-interpreter`), Daytona (`@daytonaio/sdk`), Vercel (`@vercel/sandbox`), Cloudflare (`@cloudflare/sandbox`), Modal (`modal`), ComputeSDK (`computesdk`), or Sprites (`@fly/sprites`). Docker and local execution do not require an additional provider SDK. You can also adapt a custom [Sandbox Agent](https://sandboxagent.dev) backend with `sandboxAgentProvider(backend)` from the package root.

## Advanced: existing clients

Standalone `AgentOs.create()` can mount an already-connected client. The caller owns it by default; provide a disposal callback to transfer ownership to the VM:

Install `sandbox-agent` directly when using this manual path.

```ts
import { AgentOs } from "@rivet-dev/agentos-core";
import { SandboxAgent } from "sandbox-agent";
import { docker } from "sandbox-agent/docker";

const sandbox = await SandboxAgent.start({ sandbox: docker() });
const vm = await AgentOs.create({
	sandbox: {
		client: sandbox,
		dispose: () => sandbox.destroySandbox(),
	},
});
```

RivetKit `agentOS()` intentionally rejects `{ client }` because one client cannot be shared safely across actor VMs. Use `{ provider }` there so every actor VM owns a fresh sandbox.