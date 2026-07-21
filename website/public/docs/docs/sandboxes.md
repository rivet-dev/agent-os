# External Sandboxes

Extend agentOS with full sandboxes for heavy workloads like browsers, desktop automation, and compilation.

Pair agentOS with a full sandbox on demand for heavy workloads. The sandbox
filesystem mounts into the VM as a native directory, and its process management
is exposed as [bindings](/docs/bindings) — provider-agnostic through
[Sandbox Agent](https://sandboxagent.dev).

- agentOS covers most use cases; a sandbox adds a full Linux environment for
  special software.
- Lazily start a sandbox only when a task needs it, then tear it down.
- One agent session can mix lightweight coding with heavy system work.
- See [agentOS vs Sandbox](/docs/versus-sandbox) for the full comparison.

## When to use a sandbox

- **Native binaries** not yet supported in the agentOS runtime.
- **Browsers / desktop automation** — Playwright, Puppeteer, Selenium, anything
  needing a display server.
- **Heavy compilation** — large builds or native toolchains.
- **GUI apps** — desktop apps, VNC, graphical workloads.
- **npm packages with native extensions** — `sharp`, `bcrypt`,
  `better-sqlite3`.

Default to the agentOS VM; spin up a sandbox only when required. Sandboxes bill
per second of uptime.

## Getting started

Ships as `@rivet-dev/agentos-sandbox`, working through two mechanisms:

- **Filesystem mount** — projects the sandbox into the VM as a native
  directory. Read/write files through the mount.
- **Bindings** — exposes sandbox process management as
  [bindings](/docs/bindings). Run commands on the sandbox from the VM.

Both powered by [Sandbox Agent](https://sandboxagent.dev); swap providers
without changing agent code.

```bash
npm install @rivet-dev/agentos-sandbox sandbox-agent
```

- `createSandboxFs`, `createSandboxBindings` — from `@rivet-dev/agentos-sandbox`.
- `SandboxAgent` + provider helpers (e.g. `docker`) — from `sandbox-agent`.
- Pass a provider as `sandbox: { provider: docker() }`. agentOS starts the
  client, mounts it at `/mnt/sandbox`, registers process bindings, and disposes
  it with the VM.
- In RivetKit actors, pass the provider to `agentOS(...)` — a fresh client per
  actor VM.

## Calling the mounted bindings

Write code through the filesystem, run it inside the sandbox. Bindings are a CLI
command, called through the same `exec`/`spawn` surface as any command.

## Bindings reference

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

Works with any [Sandbox Agent](https://sandboxagent.dev) provider. See the
[Sandbox Agent docs](https://sandboxagent.dev) for available providers and
setup.