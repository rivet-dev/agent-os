---
title: "Browser Terminal"
description: "A full xterm.js terminal in the browser for Agent OS VMs, driving PTY shells over the shipped agentOS() RivetKit actor with a VM sidebar, tabs, and reconnect."
category: "Processes & Shell"
order: 2
---

A full terminal for Agent OS VMs that runs in the browser, talking to the shipped
Agent OS actor (`agentOS()` from `@rivet-dev/agentos`) over its live
[RivetKit](https://rivetkit.org) connection — no bespoke WebSocket server.

- **Left sidebar** — a list of VMs. Each is one Agent OS VM (one RivetKit actor
  instance). The VM ids are kept in `localStorage`, so reopening the page — or
  clicking a VM again — reconnects to the same running VM.
- **Tabs** — each VM can have multiple terminal sessions (PTY shells).
- **Reconnect** — the actor keeps its VM (and shells) alive, so a browser that
  reconnects re-adopts the running shells (by the ids it saved in `localStorage`)
  and resumes their live I/O.

## How it works

```
Browser (React + xterm.js)                Node (server.ts)
  ├─ useActor({ name:"shellVm", key })      ├─ agentOS({ software:[…] })
  ├─ terminal.open/write/resize ───────────▶│    setup({ use:{ shellVm } })
  ├─ terminal.close                         │    registry.start()
  └─ conn.on("shellData"|"shellExit") ◀────┘  terminal.open ─▶ broadcast events
```

The browser opens a shell with `terminal.open`, sends keystrokes with
`terminal.write`,
and renders the ordered stdout/stderr bytes delivered by the `shellData` broadcast
**event** (routed to the right tab by `shellId`, with a small buffer for output
that arrives before a tab subscribes). `shellStderr` remains available as an
optional diagnostic tap and must not be rendered alongside `shellData`. This
mirrors the actor terminal in `packages/shell/src/actor-vm.ts`. The VM and its
shells live inside the actor's Rust plugin, so there is no server-side terminal
code here — `registry.start()` hosts the actor and the browser talks to it
directly.

## Run

From the repo root:

```bash
pnpm install
pnpm --filter @rivet-dev/agentos-example-browser-terminal dev
```

or from this directory:

```bash
pnpm dev            # RivetKit server (:6420) + Vite (:5173)
```

Open http://localhost:5173, click **+ New VM**, then **+** to open a terminal and
start typing (`ls`, `echo hi | tr a-z A-Z`, `cd /tmp`, …).

Run the pieces separately if you prefer:

```bash
pnpm server         # registry.start() on :6420
pnpm web            # Vite dev server on :5173
```

Override the web→server endpoint with `VITE_AGENTOS_ENDPOINT` (default
`http://localhost:6420`).

## Notes

- Software: `@agentos-software/common` (provides `sh` + coreutils) plus `git`,
  `curl`, `ripgrep`, `jq`, and `sqlite3`. Agent OS has no vim/editor package, so
  there is no in-VM editor.
- This client predates the actor's `listShells` and `shellSnapshot` actions: it
  re-adopts saved shell ids on reconnect and resumes **live** output only, so
  history from before the reload is not replayed. Stale ids (VM recreated) are
  dropped after a liveness probe. The dashboard Terminal tab uses the
  server-owned list and server-rendered repaint instead.
- The VM shell is line-buffered (it only echoes a line on Enter), so the client
  does **local echo + line editing** (printable chars, Backspace, Ctrl-C) and
  suppresses the shell's own echo of the submitted line to avoid double display.
