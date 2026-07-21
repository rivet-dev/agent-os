# Overview

A high-level tour of how agentOS works: the client / server / VM picture, the anatomy of a Linux VM (kernel + executor), agents and sessions, and the Rivet Actor orchestration underneath.

agentOS runs AI agents and untrusted code safely inside fully virtualized Linux VMs. Nothing the guest does touches your host directly: there is no real host filesystem, no real host network socket, and no real host process. Every guest operation is serviced by a kernel that agentOS owns.

This page is a high-level tour. It walks through the overall shape, the parts that make up a VM, how agent sessions work, and the orchestration layer underneath. Each section links out to a detailed page when you want to go deeper.

## The big picture

A running agentOS system has three roles: your **app** (the client), your **server** (which runs the sidecar that hosts the VMs), and the **VM** where guest code actually runs. Your app never runs guest code itself, it asks the server to.

      <text x="50" y="50" text-anchor="middle" dominant-baseline="central" font-family="var(--sl-font)" font-weight="700" font-size="38" fill="#1b1916">OS</text>
  <text x="82" y="92" text-anchor="middle" font-family="var(--sl-font)" font-size="15" font-weight="600" fill="#1b1916">Client</text>
  <text x="82" y="112" text-anchor="middle" font-family="var(--sl-font)" font-size="10.5" fill="#56524a">JS · Browser · Backend</text>
  <text x="224" y="62" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Server</text>
    <text x="157.5" y="178" text-anchor="middle" dominant-baseline="central" font-family="var(--sl-font)" font-weight="700" font-size="7" fill="#56524a">OS</text>
    <text x="174" y="178" dominant-baseline="central" font-family="var(--sl-font)" font-size="12" fill="#56524a">= an isolated VM</text>

The client speaks to the agentOS server over the wire. The server runs the **sidecar**, the trusted core that hosts every VM: it owns each VM's kernel and brokers every guest syscall the agent makes (filesystem, processes, network, permissions) before carrying it out. Each VM is a fully isolated world, so agents are isolated from one another and from your host.

### Your app (the client)

- **Trusted caller.** Your app drives agentOS. It creates VMs, opens sessions, sends prompts, and reads results back.
- **Never runs guest code.** The agent and any code it generates run in the VM, not in your app's process.
- **Available everywhere.** There is a TypeScript client and a Rust client, and the same VM is reachable from a Node script, a browser/React app, or a separate backend.
- **Owns the configuration.** Everything you send (VM setup, permission policy, resource limits, mounts) is trusted input. See the [Security Model](/docs/security-model) for why your configuration is not an attack surface.

### Your server (the sidecar)

- **The trusted core.** The sidecar is the part of the system that owns everything: the kernel, the virtual filesystem, the process and socket tables, pipes, PTYs, the permission policy, and DNS.
- **The enforcement point.** Every request the VM makes is serviced here. The sidecar decides what is allowed before carrying it out.
- **Hosts every VM.** A single sidecar manages many VMs side by side, each with its own kernel, filesystem, and process table, so every agent runs in its own isolated world. A crash or runaway in one VM never affects another.

### The VM

- **A fully virtualized Linux environment.** Each VM has its own filesystem, process table, and network policy. Two VMs share nothing.
- **The unit of isolation.** Put one tenant or one task per VM to control the blast radius. A crash or runaway in one VM never affects another.
- **Where guest code lives.** The agent, the shell, npm packages, and any generated code all run inside the VM, behind the kernel's boundary.

## Anatomy of a Linux VM

Inside every VM there are two halves. The **kernel** is the trusted core that owns all the resources and rules. The **executor** is where untrusted guest code actually runs. Guest code can only *ask* the kernel for things, it never holds a real capability of its own.

  <text x="32" y="40" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">The VM</text>

  <text x="52" y="82" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Kernel</text>
  <text x="52" y="100" font-family="var(--sl-font)" font-size="10.5" fill="#56524a">trusted core, every operation goes through here</text>
    <rect x="52" y="116" width="118" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="111" y="135" text-anchor="middle">virtual filesystem</text>
    <rect x="182" y="116" width="118" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="241" y="135" text-anchor="middle">process table</text>
    <rect x="312" y="116" width="118" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="371" y="135" text-anchor="middle">socket table</text>
    <rect x="442" y="116" width="92" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="488" y="135" text-anchor="middle">pipes / PTYs</text>
    <rect x="546" y="116" width="100" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="596" y="135" text-anchor="middle">DNS</text>
    <rect x="52" y="156" width="594" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="349" y="175" text-anchor="middle">permission policy · network allowlist · resource limits</text>

  <text x="430" y="228" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">syscalls / replies</text>

  <text x="52" y="274" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Executor</text>
  <text x="52" y="292" font-family="var(--sl-font)" font-size="10.5" fill="#56524a">untrusted, runs guest code, holds no capabilities</text>
    <rect x="382" y="262" width="170" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="467" y="281" text-anchor="middle">guest JavaScript (native V8)</text>
    <rect x="562" y="262" width="84" height="30" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="604" y="281" text-anchor="middle">WASM</text>
    <rect x="382" y="298" width="264" height="22" rx="6" fill="#faf8f3" stroke="#1b1916" stroke-width="1" /><text x="514" y="313" text-anchor="middle" font-size="10.5">shell · coreutils · npm packages · native binaries</text>

### Kernel: the trusted core

  <text x="60" y="79" text-anchor="middle" font-family="var(--sl-font)" font-size="11" fill="#56524a">guest request</text>
  <text x="200" y="79" text-anchor="middle" font-family="var(--sl-font)" font-size="12" font-weight="600" fill="#1b1916">Kernel</text>
    <rect x="290" y="8" width="176" height="26" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="378" y="25" text-anchor="middle">filesystem</text>
    <rect x="290" y="46" width="176" height="26" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="378" y="63" text-anchor="middle">processes</text>
    <rect x="290" y="80" width="176" height="26" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="378" y="97" text-anchor="middle">network &amp; DNS</text>
    <rect x="290" y="116" width="176" height="26" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="378" y="133" text-anchor="middle">policy &amp; limits</text>

The kernel is the single chokepoint. Each kind of guest operation is serviced by a kernel-owned subsystem, never by a real host capability.

- **Virtual filesystem.** A per-VM filesystem. Guest reads and writes hit the VFS, not your host disk.
- **Process table.** A virtual process table. Child processes are kernel-managed and visible only inside their VM. No real host process is ever spawned for guest work.
- **Socket table and DNS.** A virtual network stack. Outbound traffic is gated by the network allowlist.
- **Pipes and PTYs.** Kernel-owned IPC and terminal devices, so shells and pipelines behave like real Linux.
- **Policy and limits.** The kernel checks the applied permission policy, network allowlist, and resource limits on every request.

### Executor: where guest code runs

  <text x="30" y="36" font-family="var(--sl-font)" font-size="12" font-weight="600" fill="#1b1916">Executor</text>
  <text x="30" y="52" font-family="var(--sl-font)" font-size="9.5" fill="#56524a">untrusted · no capabilities</text>
    <rect x="30" y="62" width="80" height="44" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="70" y="88" text-anchor="middle">JS (V8)</text>
    <rect x="120" y="62" width="80" height="44" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="160" y="88" text-anchor="middle">WASM</text>
    <rect x="210" y="62" width="92" height="44" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="256" y="82" text-anchor="middle">native</text><text x="256" y="96" text-anchor="middle">binaries</text>
  <text x="336" y="48" text-anchor="middle" font-family="var(--sl-font)" font-size="9" fill="#56524a">syscall</text>
  <text x="336" y="92" text-anchor="middle" font-family="var(--sl-font)" font-size="9" fill="#56524a">reply</text>
  <text x="414" y="70" text-anchor="middle" font-family="var(--sl-font)" font-size="12" font-weight="600" fill="#1b1916">Kernel</text>

The executor is the untrusted half of the VM. It runs the guest code and reaches the kernel for everything else.

- **JavaScript Acceleration.** Guest JavaScript runs on a native V8 runtime (the same engine in Chrome and Node.js, with the full JIT compiler) inside an isolate. This is what we call **JavaScript Acceleration**: the guest's JavaScript executes at native speed, not through an interpreter or a translation shim. It is genuinely fast, and it presents normal Node.js semantics. See [JavaScript](/docs/javascript).
- **WASM alongside it.** The shell (`sh`) and the coreutils behind process execution ship as WebAssembly modules, and you can run your own WASM too. See [POSIX Syscalls](/docs/architecture/posix-syscalls) and the [Compiler Toolchain](/docs/architecture/compiler-toolchain).
- **Native binaries.** Tools mounted into the VM run inside the same boundary as everything else.
- **No host fallthrough.** The executor holds no capability of its own. For every file read, process spawn, or socket open, it issues a syscall and blocks for the kernel's reply.

### Processes & shell

    <rect x="14" y="14" width="92" height="26" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="60" y="31" text-anchor="middle">exec() / run()</text>
    <rect x="14" y="78" width="92" height="26" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="60" y="95" text-anchor="middle">spawn / shell</text>
  <text x="245" y="56" text-anchor="middle" font-family="var(--sl-font)" font-size="11.5" font-weight="600" fill="#1b1916">process table</text>
  <text x="245" y="71" text-anchor="middle" font-family="var(--sl-font)" font-size="9" fill="#56524a">virtual · per-VM</text>
  <text x="415" y="64" text-anchor="middle" font-family="var(--sl-font)" font-size="10.5" fill="#1b1916">pipes &amp; PTYs</text>

- **A real process model.** `exec()` and `run()` start fresh guest processes; you can also `spawn` long-running ones and open interactive shells.
- **Kernel-managed.** Every process lives in the virtual process table, with stdio bridged through kernel-owned pipes and PTYs.
- **Fresh each run.** Each `exec()` / `run()` starts a brand new guest process, so in-memory state never leaks from one run into the next.
- See [Processes](/docs/architecture/processes) for the internals.

### Virtual filesystem

    <rect x="60" y="14" width="360" height="30" rx="8" fill="#ffffff" stroke="#1b1916" stroke-width="1.2" /><text x="240" y="33" text-anchor="middle">overlay (guest writes)</text>
    <rect x="60" y="52" width="360" height="30" rx="8" fill="#faf8f3" stroke="#1b1916" stroke-width="1.2" /><text x="240" y="71" text-anchor="middle">root layer (snapshot)</text>
    <rect x="60" y="104" width="108" height="32" rx="7" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="114" y="124" text-anchor="middle">host dir mount</text>
    <rect x="186" y="104" width="108" height="32" rx="7" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="240" y="124" text-anchor="middle">S3 mount</text>
    <rect x="312" y="104" width="108" height="32" rx="7" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="366" y="124" text-anchor="middle">cloud store</text>
  <text x="240" y="98" text-anchor="middle" font-family="var(--sl-font)" font-size="9" fill="#56524a">mount points grafted onto guest paths</text>

- **Layered engines.** The VFS is a tree of engines: a root layer bootstrapped from a snapshot, an overlay for writes, and mount points that graft other backends onto guest paths.
- **Host-backed mounts.** A guest path can be backed by a host directory, S3, or a cloud store. The kernel confines all guest I/O to the mount root, even against symlink and `..` tricks.
- **Persisted.** The `/home/agentos` filesystem survives sleep/wake.
- See [Filesystem](/docs/architecture/filesystem) for the internals.

### Networking

    <rect x="14" y="10" width="92" height="24" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="60" y="26" text-anchor="middle">fetch()</text>
    <rect x="14" y="42" width="92" height="24" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="60" y="58" text-anchor="middle">node:http</text>
    <rect x="14" y="74" width="92" height="24" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="60" y="90" text-anchor="middle">node:net</text>
    <rect x="14" y="106" width="92" height="24" rx="6" fill="#ffffff" stroke="#1b1916" stroke-width="1" /><text x="60" y="122" text-anchor="middle">WASM sockets</text>
  <text x="248" y="68" text-anchor="middle" font-family="var(--sl-font)" font-size="11" font-weight="600" fill="#1b1916">socket table</text>
  <text x="248" y="83" text-anchor="middle" font-family="var(--sl-font)" font-size="9" fill="#56524a">kernel-owned</text>
  <text x="410" y="74" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#1b1916">egress allowlist</text>

- **One authoritative transport.** Guest `fetch()`, `node:http`, `node:net`, and WASM sockets all target the same kernel socket table. No part of guest networking opens a real host socket on its own.
- **Egress policy.** Outbound traffic is gated by the network allowlist; loopback traffic stays confined to the VM.
- **Preview URLs.** Servers a guest starts can be exposed through signed preview URLs.
- See [Networking](/docs/architecture/networking) for the internals.

<Note>The security boundary that matters is between the trusted sidecar and the untrusted executor. Everything the guest tries to do crosses into the kernel, where the policy is checked before the operation runs. See the [Security Model](/docs/security-model) for the full threat model.</Note>

## Agents & sessions

An agent (such as [Pi](https://github.com/mariozechner/pi-coding-agent)) is just another guest process running inside a VM, behind the same boundary as any other code. A **session** keeps that agent alive across many prompts and streams its output back to your app as events.

  <text x="87" y="98" text-anchor="middle" font-family="var(--sl-font)" font-size="14" font-weight="600" fill="#1b1916">Client</text>
  <text x="87" y="118" text-anchor="middle" font-family="var(--sl-font)" font-size="10.5" fill="#56524a">your app</text>

  <text x="214" y="84" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">prompt</text>
  <text x="214" y="136" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">events</text>

  <text x="290" y="56" font-family="var(--sl-font)" font-size="12" font-weight="600" fill="#1b1916">The VM</text>
  <text x="410" y="98" text-anchor="middle" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Agent session</text>
  <text x="410" y="116" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">long-lived agent process</text>

  <text x="619" y="100" text-anchor="middle" font-family="var(--sl-font)" font-size="12" font-weight="600" fill="#1b1916">Filesystem</text>
  <text x="619" y="118" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">SQLite over UDS</text>

### Sessions & durable files

- **Long-lived.** Where a bare `exec()` runs once and exits, a session keeps an agent alive across many prompts.
- **Streamed.** The agent's output flows back to your app in real time as `sessionEvent`s.
- **Durable semantic events.** Completed ACP updates and interactive permission request/response records are sequenced in SQLite; streaming message deltas remain live-only.
- **Durable files.** Files under `/home/agentos` survive sleep through the sidecar's direct SQLite-over-UDS connection.
- **Context injected.** agentOS adds a system prompt describing the VM environment and available commands and bindings, layered on top of the agent's own instructions. See [System Prompt](/docs/system-prompt).
- See [Agent Sessions](/docs/architecture/agent-sessions) for the internals.

### Permissions & approvals

- **Two layers, different jobs.** The lower-level [permission policy](/docs/permissions) is enforced by the kernel on every guest syscall (nothing is allowed until you opt in). On top of that, [approvals](/docs/approvals) are about an agent asking before it uses a tool.
- **Human-in-the-loop or automatic.** Use the default `allow_all`, explicit `reject_all`, or subscribe to `permission_request` variants on the ordinary session-event stream with `permissionPolicy: "ask"`.
- **Blocks until answered.** An `ask` request has no expiry and keeps the active turn awake until a response or explicit lifecycle transition wins the race.

## Orchestration (Rivet Actors)

The `agentOS()` actor (from `@rivet-dev/agentos`) wraps the raw VM in a [Rivet Actor](/docs/core), which adds durable state, scheduling, and orchestration. This is what gives you persistence, cron, and workflows out of the box.

  <text x="64" y="46" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Rivet Actor</text>
  <text x="64" y="64" font-family="var(--sl-font)" font-size="10.5" fill="#56524a">durable, addressable server object</text>

  <text x="154" y="116" text-anchor="middle" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">agentOS VM</text>
  <text x="154" y="136" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">the virtual Linux VM</text>

    <rect x="272" y="80" width="120" height="34" rx="7" fill="#ffffff" stroke="#1b1916" stroke-width="1.1" /><text x="332" y="102" text-anchor="middle">Cron</text>
    <rect x="412" y="80" width="120" height="34" rx="7" fill="#ffffff" stroke="#1b1916" stroke-width="1.1" /><text x="472" y="102" text-anchor="middle">Workflows</text>
    <rect x="272" y="126" width="260" height="34" rx="7" fill="#ffffff" stroke="#1b1916" stroke-width="1.1" /><text x="402" y="148" text-anchor="middle">Persistence · sleep / wake</text>
    <rect x="552" y="80" width="92" height="80" rx="7" fill="#ffffff" stroke="#1b1916" stroke-width="1.1" /><text x="598" y="116" text-anchor="middle">Durable</text><text x="598" y="132" text-anchor="middle">state</text>

### What are actors?

- **Durable server objects.** A Rivet Actor is a long-lived, addressable object with its own state. You reach a specific VM by name (`vm.getOrCreate("my-agent")`).
- **Stateful by default.** The actor persists its filesystem, actor state, durable session metadata, and completed ACP history. Active adapter processes remain runtime state.
- **The portable runtime.** Actors give you a consistent way to run `agentOS()` on any infrastructure, with persistence, networking, and orchestration built in.

### Cron

- **Recurring work.** Schedule a shell command or an agent session on a cron expression.
- **Overlap control.** Choose what happens when a run is still going when the next is due (`allow`, `skip`, or `queue`).
- **Observable.** Stream `cronEvent`s to watch executions. See [Crons & Loops](/docs/cron).

### Workflows

- **Durable multi-step tasks.** A workflow is the actor's `run` handler wrapped in `workflow()`, where each `ctx.step()` is recorded, retried, and resumed independently.
- **Crash-proof.** If the process dies mid-run, replay skips completed steps and continues where it left off.
- **Composable.** The output of one step feeds the next: clone a repo, let an agent fix a bug, run the tests. See [Workflows & Graphs](/docs/workflows).

### Persistence & sleep/wake

- **Sleeps when idle.** After a grace period (15 minutes by default) with no activity, the VM sleeps to free resources.
- **Wakes on demand.** It wakes automatically when a client connects or a cron job fires.
- **What survives.** The `/home/agentos` filesystem, actor state, preview tokens, durable session metadata, and completed ACP history persist. On wake, `openSession` restores the adapter when possible and otherwise starts it with AgentOS history as context. In-flight deltas, running processes, and open shells do not survive. See [Persistence & Sleep](/docs/persistence).

## Going deeper

This page is the map. Each subsystem has its own detailed page in the Advanced architecture section:

- **[Agent Sessions](/docs/architecture/agent-sessions)**: how a session is bound to a VM, and how prompts and events flow end to end.
- **[Processes](/docs/architecture/processes)**: the virtual process table, `exec()` / `run()`, child processes, and PTYs.
- **[Filesystem](/docs/architecture/filesystem)**: the per-VM virtual filesystem, overlays, and host-backed mounts.
- **[Networking](/docs/architecture/networking)**: the virtual socket table, DNS, the allowlist, and guest `fetch()`.
- **[JavaScript Executor & Socket Reactor](/docs/architecture/javascript-executor)**: how the shared Tokio runtime, V8 executor threads, coalesced readiness, bounded channels, and Node stream backpressure fit together.
- **[POSIX Syscalls](/docs/architecture/posix-syscalls)**: how WebAssembly guests behave like normal POSIX programs on top of the kernel.
- **[Compiler Toolchain](/docs/architecture/compiler-toolchain)**: how the shell and coreutils are compiled to WebAssembly and mounted into the VM.
- **[System Prompt](/docs/system-prompt)**: the context agentOS injects into every agent session.
- **[Persistence & Sleep](/docs/persistence)**: what survives sleep/wake, and how VMs sleep and wake.

For the trust model and what counts as a sandbox escape, see the [Security Model](/docs/security-model).