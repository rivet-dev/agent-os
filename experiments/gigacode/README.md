# Gigacode (AgentOS experiment)

This is the next-generation Gigacode experiment: the vanilla OpenCode TUI and
GUI backed by AgentOS actors over RivetKit.

```text
OpenCode attach
    -> Gigacode OpenCode-compatible API (:2468)
        -> RivetKit client
            -> one SQLite coordinator actor (:2469)
            -> one AgentOS workspace actor per canonical cwd
                -> many logical sessions using Claude Code, Codex, Pi, or OpenCode
```

The implementation intentionally lives in one TypeScript entrypoint,
[`gigacode.ts`](./gigacode.ts). Running it with no subcommand is the client;
`daemon` is the server. The client health-checks and autospawns the daemon before
running `opencode attach`. `gigacode run ...` uses OpenCode's headless
`run --attach` mode against the same autospawned daemon.

The daemon is machine-wide rather than workspace-bound. On the first startup it
finishes model discovery before exposing the OpenCode API, so the TUI cannot race
the ACP probes. Later startups load the persisted catalog and expose the API
without probing any harness. The coordinator actor owns the durable session index in embedded SQLite. A
canonical host cwd maps to one durable AgentOS actor whose creation input mounts
that directory at `/workspace`; every OpenCode session has independent durable
metadata and transcript state inside the shared workspace actor. Each logical
session owns its ACP process; prompting another session does not close or reload
its neighbors. The first prompt fixes the ACP harness for that logical session;
a request for another harness returns an error and must be sent through a new
OpenCode session. Starting Gigacode from another directory reuses the same daemon
and does not restart Rivet.

Gigacode is intentionally a thin OpenCode HTTP/SSE forwarder and data-shape
converter over AgentOS/ACP. It does not retry failed Rivet startup, model probes,
ACP session creation, or prompts, and it does not replace failed sessions or
adapters. Those failures are surfaced so the underlying AgentOS or adapter defect
can be fixed at its source.

Agent model catalogs are discovered from each harness's ACP session config and
stored in one machine-wide cache at `$GIGACODE_STATE_DIR/models.json`. A missing
or invalid cache is the only automatic discovery path, and it is a first-start
barrier before the TUI. Subsequent daemon starts only load that cache. Run
`gigacode models refresh` to explicitly discover and persist changed catalogs;
there is no timer, idle hook, prompt hook, or other automatic runtime refresh.
A foreground-priority gate keeps an explicitly requested refresh from
overlapping user turns.

When the client autospawns the daemon, it mirrors plain `[gigacode]` startup
milestones—including first-start model discovery—to the terminal until the
Rivet runtime and catalog are usable, then opens the TUI. Durations are shown
beside completed phases; per-session Pino JSONL is never mixed into the
interactive startup display.

## Local workspace mode

**Gigacode intentionally has read-write access to the selected host workspace.
It is not a security boundary and must not be used to run untrusted prompts or
software.** Other host directories are not mounted, apart from the explicit
credential directories below.

Every AgentOS VM receives these mounts:

```text
Guest path                  Host path                         Access
/workspace                  <canonical active cwd>            read-write
/home/agentos/.claude       ~/.claude                         read-write
/home/agentos/.codex        ~/.codex                          read-write
/home/agentos/.pi           ~/.pi                             read-write
```

OpenCode's configuration and `auth.json` are passed through
`OPENCODE_CONFIG_CONTENT` and `OPENCODE_AUTH_CONTENT`. Its config and data
directories are deliberately not mounted because they can contain host plugins
and the live SQLite database, which cannot safely run inside or be shared with
the AgentOS process.

Gigacode adds AgentOS system instructions explaining that commands execute in
the local AgentOS VM, not Docker, and that `/workspace` is the active project.
Writes through `/workspace` affect the real project immediately using the
daemon user's Unix permissions.

## Quickstart

From the repository root:

```bash
just install-gigacode
gigacode
# `giga` is installed as a short alias.
```

The recipe builds Gigacode and its in-repo AgentOS/ACP dependencies, then runs
the bundled installer. It creates a self-contained production deployment under
`~/.local/share/gigacode`, packages optimized release builds of the native
sidecar and actor plugin, and writes launchers to `~/.local/bin` by default.
Codex requires a WASI build that supports `codex app-server`, which is the
protocol used by the packaged `@agentclientprotocol/codex-acp` adapter. The
current `rivet-dev/codex:wasi-port-codex-core` artifact supports only
`codex-exec --session-turn` and cannot be used by that adapter until app-server
support is ported or a session-turn ACP bridge is packaged.

To run directly from the checkout instead:

```bash
pnpm install
pnpm --filter @rivet-dev/agentos-experiment-gigacode... build
pnpm --dir experiments/gigacode gigacode
```

Set `GIGACODE_INSTALL_BIN_DIR` to choose a different executable directory or
`GIGACODE_INSTALL_ROOT` to choose a different runtime directory. The installer
also creates `giga` as a short alias for `gigacode`. The installed command does
not depend on the checkout's `node_modules` remaining present. The deployment
keeps a runtime archive and automatically restores its own `node_modules` if a
machine cleanup removes generated dependency trees.

Useful commands:

```bash
gigacode daemon             # foreground daemon
gigacode daemon start       # detached daemon
gigacode daemon status
gigacode daemon stop
gigacode models refresh    # the only refresh after first-start discovery
gigacode shell             # shell in the cwd's shared workspace actor
gigacode shell sh          # optionally choose the guest command and arguments
gigacode run --model claude/default "Summarize this workspace"
gigacode debugger [actorID] # open the local Rivet inspector
```

`gigacode shell` attaches a PTY to the same per-cwd workspace actor used by
OpenCode sessions. Exiting the shell closes the connection but leaves the
durable workspace actor available for later sessions.

Configuration:

- `GIGACODE_PORT` — OpenCode-compatible API port (default `2468`)
- `GIGACODE_RIVET_PORT` — local Rivet engine port (default `2469`, deliberately
  not RivetKit's conventional `6420`)
- `GIGACODE_OPENCODE_BIN` — OpenCode executable (default `opencode`, with an
  `npx --yes opencode-ai` fallback)
- `GIGACODE_INSPECTOR_URL` — local inspector base URL (default
  `http://localhost:43708/`)
- `GIGACODE_WORKSPACE` — fallback directory when an OpenCode request does not
  send its cwd; it does not bind or restart the central daemon
- `GIGACODE_SESSION_ENV_JSON` — bounded JSON string map forwarded to every ACP
  harness session (for example, provider credentials or a test endpoint)
- `GIGACODE_LOOPBACK_EXEMPT_PORTS` — comma-separated host loopback ports made
  reachable from the AgentOS VM
- `GIGACODE_NETWORK_PERMISSION` — optional AgentOS network override, `allow` or
  `deny`; unset preserves AgentOS's restricted default policy
- `GIGACODE_SHARE_HOST_CREDENTIALS=0` — disable the default host credential
  mounts
- `GIGACODE_CREDENTIALS_READ_ONLY=1` — mount host credentials read-only; this
  prevents agent writes but may prevent OAuth refresh persistence
- `GIGACODE_CLAUDE_CONFIG_DIR` / `GIGACODE_CODEX_HOME` / `GIGACODE_PI_HOME` —
  override the host credential directories (defaults: `~/.claude`, `~/.codex`,
  and `~/.pi`)
- `GIGACODE_OPENCODE_CONFIG_DIR` / `GIGACODE_OPENCODE_DATA_DIR` — override the
  host directories from which GigaCode reads bounded OpenCode config and auth
  files (defaults: `~/.config/opencode` and `~/.local/share/opencode`). These
  directories are not mounted into the VM, so unrelated files such as
  `opencode.db` are not projected into AgentOS.
- `GIGACODE_PI_API_KEY` / `GIGACODE_PI_BASE_URL` — optional Pi-only Anthropic
  provider overrides, primarily useful for local gateways and deterministic
  tests; these values are not added to other harness sessions
- `GIGACODE_MODEL_PROBE_CONCURRENCY` — simultaneous model probes (default `3`;
  higher values can be slower because cold actors share one local runner)
- `GIGACODE_LOG_LEVEL` — Pino level for per-session structured logs (default
  `info`)
- `GIGACODE_MAX_PROMPT_QUEUE_PER_SESSION` — maximum queued turns for one
  session (default `64`)
- `GIGACODE_MAX_MESSAGES_PER_SESSION` — maximum projected OpenCode messages in
  one session (default `10000`)
- `GIGACODE_MAX_MESSAGE_STORE_BYTES` — maximum durable message-store size
  (default `67108864`)
- `GIGACODE_MAX_FS_FIND_SCAN` / `GIGACODE_MAX_FS_FIND_RESULTS` — bounds for
  OpenCode's recursive file completion (defaults `50000` and `200`)

Each OpenCode session receives an asynchronous Pino JSONL log at
`$GIGACODE_STATE_DIR/session-logs/<session-id>.jsonl` (by default under
`~/.local/state/gigacode`). Phase events include `durationMs` fields for actor
setup, ACP session creation, prompting, idle delivery, and connection disposal.

## Host harness authentication

By default, Gigacode mounts the host's existing `~/.claude`, `~/.codex`, and
`~/.pi` directories, plus OpenCode's `~/.config/opencode` and
`~/.local/share/opencode` directories, at their corresponding
`/home/agentos` paths in every AgentOS VM. The mounts are writable so each
harness can refresh its own OAuth tokens and persist them back to the host. This
makes native logins available without copying credentials into Gigacode state.

The Claude AgentOS adapter reads OAuth credentials by default. Set
`CLAUDE_CODE_BARE=1` or `CLAUDE_CODE_SIMPLE=1` on the daemon only when
intentionally opting into Claude's API-key-only minimal modes.

Pi uses its own native host authentication. Install and log in to Pi once:

```bash
npm install -g @earendil-works/pi-coding-agent
pi
# Enter /login and select Anthropic Claude Pro/Max, OpenAI Codex, or another provider.
```

Pi stores OAuth credentials in `~/.pi/agent/auth.json`. Gigacode mounts the
entire `~/.pi` directory into every VM, so Pi reads that file directly and
persists refreshes back to the host. If Pi's `models.json` contains an Anthropic
API key, Gigacode also forwards that existing key in Pi's session environment;
Pi does not otherwise recognize the custom-model entry as built-in-provider
authentication during ACP `session/new`. The value is not forwarded to other
harnesses. Gigacode does not translate Claude or Codex OAuth tokens into Pi's
credential format.

Gigacode also forwards only a fixed allowlist of provider variables when they
are set on the daemon: Anthropic/OpenAI keys and base URLs, Claude Bedrock or
Vertex switches, and the AWS credential-chain variables. Values in
`GIGACODE_SESSION_ENV_JSON` override this allowlist.

These are trusted host mounts exposed to an untrusted agent VM. A harness can
read and modify the mounted Claude, Codex, Pi, and OpenCode configuration, not
only the token files. Set `GIGACODE_CREDENTIALS_READ_ONLY=1` to prevent writes,
or `GIGACODE_SHARE_HOST_CREDENTIALS=0` to isolate Gigacode completely.

## End-to-end tests

The E2E suite starts LLMock, invokes the Gigacode client in headless `run` mode,
and verifies that the client autospawns a fresh daemon and local Rivet engine.
The OpenCode v2 TypeScript SDK exercises provider discovery, file search,
session CRUD and directory isolation, durable resume, three ordered turns per
harness across a daemon restart, FIFO
queueing, cancellation, permissions, streamed tool calls, shell success/error/
cancellation, prompting through every available ACP harness, harness-switch
rejection,
SSE replay, and debugger command discovery. The Codex cases require the
app-server-capable WASI artifact described above. The reusable
[`TmuxTerminal`](./tmux-terminal.ts) driver then launches the globally installed
`gigacode` command and drives the real OpenCode TUI through ordered multi-turn
prompts, model selection, cancellation, queued input, `!` shell mode, and slash
commands while retaining terminal snapshots.

Build the native AgentOS artifacts once, then run the suite:

```bash
cargo build -p agentos-sidecar
pnpm --dir experiments/gigacode test:e2e
```

No host model API keys are read. The E2E daemon receives isolated mock keys and
provider endpoints, and allows network access only for that test process so all
available harnesses can reach LLMock on its random loopback port.

## Deliberate experiment boundaries

- The OpenCode provider groups are AgentOS harnesses, not inference providers.
  Models within each group come from the harness's ACP model config option. A
  harness that does not advertise models retains a `default` model entry. An
  OpenCode session cannot change harnesses after its first prompt; create a new
  logical session instead.
- Session listing comes from a dedicated coordinator actor whose embedded
  SQLite database stores the OpenCode session-to-workspace/ACP-session mapping.
  The bounded OpenCode message projection remains beside Rivet state, while
  AgentOS persists the underlying ACP event history in each workspace actor.
  After a daemon or actor restart, Gigacode retains the AgentOS session ID and
  lets AgentOS perform its normal durable resume on the next turn. Gigacode does
  not construct a transcript handoff or create a replacement ACP session.
- One machine-wide daemon serves every cwd. Each request supplies its directory;
  a canonical cwd selects one workspace actor and its creation input mounts only
  that host directory at `/workspace`. Deleting a session closes its ACP session
  but intentionally retains the reusable workspace actor.
- Recursive file completion and file prompt parts are supported. OpenCode PTY
  hosting, LSP/formatter management, sharing, questions, worktrees, fork,
  revert, and OpenCode filesystem snapshots remain explicit non-goals rather
  than successful no-ops.
- `gigacode debugger` opens the inspector directly. OpenCode also discovers a
  `gigacode-debugger` custom command through `/command`; invoking it asks the
  daemon to open the inspector for the current AgentOS actor.
- `/diff`, todos, children, MCPs, skills, LSPs, and formatters return schema-
  correct empty collections because Gigacode has no corresponding AgentOS
  objects. Sharing is disabled in `/config`; unsupported mutation routes return
  an explicit error.

The audited route and workflow matrix, including intentional omissions, is in
[`COMPATIBILITY.md`](./COMPATIBILITY.md).
