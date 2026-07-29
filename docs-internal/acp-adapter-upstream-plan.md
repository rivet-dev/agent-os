# Upstream ACP adapter integration plan

Status: implementation and validation in progress  
Last verified against npm dist-tags: 2026-07-14

## Goal

AgentOS should run each harness's maintained ACP implementation whenever one
exists. AgentOS packages may select an entrypoint, supply configuration, mount
native credentials, and compile upstream source for the Node runtime. They must
not independently recreate provider discovery, models, sessions, commands,
permissions, streaming, or other harness semantics.

The desired boundary is:

```text
ACP client
    -> upstream ACP implementation
        -> upstream harness SDK, service, or RPC interface
            -> AgentOS Node and operating-system runtime
```

## Rules

1. Prefer a harness's native ACP implementation. Otherwise prefer the adapter
   maintained by the Agent Client Protocol project and built on the harness's
   official SDK or machine protocol.
2. Consume released packages or pinned upstream source. Do not copy an
   adapter's implementation into AgentOS.
3. Do not patch harness semantics. In particular, do not replace model lists,
   auth, provider discovery, session behavior, tool events, slash commands, or
   ACP lifecycle behavior.
4. Run the published Node entrypoint unchanged whenever possible. A thin
   AgentOS launcher may set documented environment variables or select an
   executable, but it must not translate ACP.
5. A maintained ACP adapter and its underlying harness are separate patch
   boundaries. AgentOS may temporarily retain minimal SDK/CLI compatibility
   patches beneath a maintained adapter, but it must not fork the adapter's ACP
   implementation. Inventory, isolate, test, and remove those patches
   independently.
6. If valid Node code fails inside an AgentOS VM, reduce it to a focused runtime
   test and fix AgentOS. Do not rewrite the upstream package to avoid the
   missing or incompatible Node behavior.
7. OpenCode's target is **zero source patches**. Build configuration and
   packaging are allowed; modifying extracted OpenCode source is not.
8. Bun may be used as the upstream build tool when upstream uses it to emit a
   Node bundle. The packaged ACP process must have no Bun runtime dependency.
9. Pin source/package versions and record their provenance. Upgrades must run
   the upstream-parity suite before changing the pin.
10. Contribute adapter bugs upstream. Carrying a temporary downstream patch
   requires an explicit decision, an upstream issue/PR, and a removal test.

## Current upstream landscape

| Harness | Maintained ACP path | Upstream harness boundary | Current conclusion |
| --- | --- | --- | --- |
| OpenCode | Native `opencode acp` in the OpenCode repository | OpenCode server and SDK | Compile native ACP from current upstream source for Node; do not maintain a separate adapter. |
| Claude | [`@agentclientprotocol/claude-agent-acp`](https://github.com/agentclientprotocol/claude-agent-acp) | Official Claude Agent SDK | Replace our custom Claude SDK-to-ACP implementation with the published adapter. |
| Pi | [`pi-acp`](https://github.com/svkozak/pi-acp), a community-maintained Node adapter | Native Pi CLI through its official `pi --mode rpc` interface | Package the published adapter unchanged with the real Pi CLI. Remove the embedded Pi SDK, snapshot adapter, and custom Rust/WASI adapter. |
| Codex | [`@agentclientprotocol/codex-acp`](https://github.com/agentclientprotocol/codex-acp) | Official `codex app-server` | Package the maintained adapter unchanged and point it at the packaged Codex app-server command. |

“Maintained ACP path” does not always mean the harness vendor owns the adapter.
OpenCode owns its native implementation. The Claude adapter originated at Zed
and now lives under the Agent Client Protocol organization; it uses Anthropic's
official SDK, but it is not native ACP shipped by Anthropic. The Codex adapter
also lives under the Agent Client Protocol organization and translates the
official OpenAI Codex App Server. Pi's `pi-acp` package is community-maintained
and translates the public Pi RPC protocol.

Versions observed during this research:

- OpenCode source: `1.17.20` at `4473fc3c9055046183990a965d68df3db7ea6f62`.
- Claude Agent ACP: `0.59.0`, using Claude Agent SDK `0.3.207`, Node 22+.
- Reference Node Pi ACP: `0.0.31`. The repository currently targets
  `@earendil-works/pi-coding-agent`; `@mariozechner/pi-coding-agent` is
  deprecated in npm in favor of that package. The current Pi CLI compatibility
  target is `@earendil-works/pi-coding-agent` `0.80.6`; legacy `0.73.1` and
  `0.60.0` remain regression targets.
- Codex ACP: current published package `1.1.2` at npm git head
  `8aff492d4b033ff2c02ad3b9d591994d57617463`, resolving `@openai/codex`
  `0.144.x`.

These pins were rechecked against their npm `latest` dist-tags on 2026-07-14.
Recheck them again before a later upgrade because all four projects release
frequently.

## Workstream 1: OpenCode native ACP on Node

### Finding

Current OpenCode is much closer to Node compatibility than our pinned 1.3.13
copy:

- The ACP entrypoint itself has no Bun API calls.
- The HTTP server uses `node:http` and `@effect/platform-node`.
- OpenCode has an upstream `script/build-node.ts` and `src/node.ts` build.
- Core runtime selection uses Node conditions for SQLite, PTY, and filesystem
  implementations.
- The Node database implementation uses `node:sqlite` rather than
  `bun:sqlite`.

Bun remains in upstream's build script as the bundler. That is a build-time
dependency, not a Bun dependency in the resulting Node process.

### Current AgentOS problem

The AgentOS package downloads OpenCode 1.3.13, applies a five-file patch, then
performs thousands of lines of string-based source transformations. Those
transformations replace provider discovery, model catalogs, storage, plugins,
sessions, prompts, and parts of ACP startup. This is a hidden fork and is the
reason AgentOS OpenCode does not behave like upstream OpenCode.

### Migration

1. Pin a current OpenCode release and source checksum.
2. Install the exact upstream lockfile.
3. Run upstream generation needed by the Node build, including embedded model
   data and database migrations.
4. Build the upstream ACP entrypoint for `target: "node"` using the same Node
   conditions and defines as OpenCode's own Node build.
5. Package that output and its runtime dependencies without modifying the
   extracted source tree.
6. Invoke the exported upstream `AcpCommand` or its generated CLI entrypoint
   directly from the AgentOS command.
7. Delete `loadProviderCatalog`, the `sql.js` substitution, source-string
   rewrites, and the downstream OpenCode patch file.
8. Mount OpenCode's native configuration and credential paths so the VM sees
   the same provider state as an ordinary OpenCode process.

The AgentOS build driver may call Bun because that is OpenCode's upstream
build tool. It may not inject application behavior. Add a source-integrity gate
that fails if the prepared source differs from the downloaded source.

### Runtime compatibility gates

Run the unmodified Node bundle first and turn every failure into a focused
AgentOS runtime test. Expected areas include:

- `node:http`, WebSockets, loopback sockets, and server shutdown;
- `node:sqlite` (`DatabaseSync`, prepared statements, bigint and array modes,
  WAL, transactions, and close semantics);
- conditional package exports and the `node` condition;
- child processes, signals, stdio, and process lifecycle;
- filesystem watches, symlinks, real paths, and atomic file operations;
- `AsyncLocalStorage`, streams, fetch, and abort behavior;
- optional native packages such as PTY, file watching, and image processing.

AgentOS already has a `node:sqlite` surface and tests. The OpenCode migration
must verify the exact methods OpenCode exercises rather than replacing SQLite
inside OpenCode again.

### Acceptance

- The prepared OpenCode source is byte-identical to the pinned upstream source.
- The packaged process boots without a Bun executable or Bun runtime globals.
- Under identical config and auth, host `opencode acp` and AgentOS OpenCode
  report equivalent providers, models, config options, modes, agents, and
  commands.
- Model selection, effort selection, multi-turn prompting, cancel, session
  list/load/resume, tools, and slash commands pass end-to-end.

## Workstream 2: Claude Agent ACP package

### Finding

The maintained adapter is now
[`@agentclientprotocol/claude-agent-acp`](https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp).
It is a Node 22+ TypeScript package, exports a CLI and library API, and uses the
official Claude Agent SDK. It already implements session listing/loading,
resuming, model and effort config options, permissions, tools, terminals,
custom commands, and current ACP lifecycle behavior.

Our current `registry/agent/claude` package independently implements the same
translation and also carries substantial compatibility work for Anthropic's SDK
and minified CLI. These are different concerns: the custom ACP translation
should move to the maintained adapter, while the SDK/CLI patches must be
inventoried and migrated deliberately rather than deleted wholesale.

### Migration

1. Make the published Claude Agent ACP package own the `acpEntrypoint` and all
   ACP translation.
2. Prefer its `claude-agent-acp` binary directly. Use its exported `runAcp()`
   only if AgentOS needs an in-process launcher; do not wrap individual ACP
   methods.
3. Use the adapter's supported `CLAUDE_CODE_EXECUTABLE` environment variable to
   point it at the AgentOS-compatible Claude CLI artifact. The maintained
   adapter already forwards this path to the Claude Agent SDK, so no adapter
   source change is required.
4. Initially package it with the Claude CLI artifact that currently works in
   AgentOS, preserving the existing compatibility patches below the adapter
   boundary. Let the adapter import its normal Claude Agent SDK package; if that
   exposes a genuine AgentOS runtime gap, fix the runtime instead of aliasing a
   custom SDK indefinitely.
   The official adapter currently selects Claude Agent SDK `0.3.207`, while the
   compatibility artifact was developed against `0.2.87`; revalidate each patch
   against the selected SDK/CLI version instead of copying the old patch set.
5. Inventory every local Claude modification and classify it as ACP semantics,
   AgentOS Node/POSIX compatibility, an upstream Claude defect, or AgentOS
   packaging/observability.
6. Delete custom ACP semantics in favor of the maintained adapter. Move valid
   Node/POSIX fixes into AgentOS core, upstream real Claude defects, and retain
   only the smallest isolated SDK/CLI patches still required.
7. Keep the native `~/.claude` mount and supported Anthropic/AWS environment
   forwarding. Authentication remains owned by Claude.
8. Remove the current custom adapter only after every existing Claude behavior
   has a maintained-adapter parity test or an explicit documented disposition.

The current patch builder contains 40 named CLI rewrites plus one conditional
SDK guard. Most of the CLI rewrites are tracing probes, while others change
stdio, shell execution, startup, hooks, or lifecycle behavior. Do not migrate
this list as an opaque block. Give each retained patch a category, focused
test, reason it cannot yet be removed, and removal condition. Trace-only
rewrites should move to supported logging/observability or be deleted.

### Runtime policy

Run the published adapter unchanged. Start by trying its current SDK/CLI
unchanged, but keep the known-good compatibility artifact available during the
migration so that replacing ACP does not also rewrite the harness runtime in
one step. Previous downstream patches point to likely runtime gaps around
child-process stdio, stream destruction, shell argument handling, signals,
`process.exitCode`, `realpath`, `/dev/null`, and native helper lookup. Re-test
current releases before assuming those gaps still exist. For each reproducible
Node mismatch:

1. reproduce it without Claude in a small AgentOS runtime test;
2. fix the Node/POSIX behavior in AgentOS core;
3. verify both the reduced test and the unmodified Claude adapter;
4. do not add another minified-source patch.

If the behavior also fails under real Node, treat it as an upstream adapter or
Claude SDK issue rather than an AgentOS runtime issue.

### Acceptance

- The installed ACP adapter source is the published package without
  modifications; any remaining local changes are isolated below it in the
  Claude SDK/CLI compatibility layer.
- Auth uses the same host Claude credentials and can persist token refreshes.
- Models and effort options match the adapter running directly on the host.
- New/list/load/resume sessions, multiple turns, cancel, queued input,
  permissions, tool calls, terminals, subagents, and slash commands pass.
- Adapter disconnect and end-of-turn behavior match the host process.

## Workstream 3: Pi ACP package migration

### Decision

Use the published `pi-acp@0.0.31` Node package unchanged. It speaks ACP over
stdio, launches Pi through `pi --mode rpc`, and supports
`PI_ACP_PI_COMMAND` as the executable override. AgentOS owns only packaging,
runtime compatibility, configuration, and tests; it does not maintain a second
ACP translation.

Pin `@earendil-works/pi-coding-agent@0.80.6` beside the adapter so both commands
are projected into `/opt/agentos/bin`. Remove the former embedded SDK/snapshot
adapter and the custom Rust/WASI source build completely.

### AgentOS migration

1. Pin `pi-acp@0.0.31` and the current compatible Pi CLI package.
2. Keep `registry/agent/pi` thin: package metadata, the published adapter, and
   the native Pi CLI package.
3. Make `pi-acp` the package's `acpEntrypoint`, set only documented launch
   configuration such as the Pi executable path, and preserve native Pi config
   and credential mounts.
4. Delete the SDK adapter, private `dist/core/*` imports, snapshot entry/build,
   and the duplicate `pi-cli` agent after compatibility aliases are handled.
5. Validate the published adapter against the packaged AgentOS entrypoint, then
   run the shared Gigacode suite.

### Acceptance

- The packed package contains the pinned published `pi-acp` adapter and Pi CLI.
- Model/provider discovery and auth come from the launched Pi CLI's native
  config and storage; no credential conversion or embedded SDK remains.
- The required feature matrix and native/AgentOS parity tests pass, including
  multi-turn, resume, cancel, queueing, models, tools, commands, permissions,
  and shutdown.
- AgentOS and host runs use the same pinned Node adapter and behave equivalently
  after normalizing IDs, times, and paths.

## Workstream 4: Codex ACP package

### Finding

The current maintained adapter is
[`@agentclientprotocol/codex-acp`](https://www.npmjs.com/package/@agentclientprotocol/codex-acp).
It is a Node package that launches the official `codex app-server`, translates
ACP requests to App Server operations, and maps Codex events back to ACP. It
includes a compatible `@openai/codex` dependency and supports overriding the
executable through `CODEX_PATH`.

The current AgentOS `registry/agent/codex` package explicitly does not register
an ACP entrypoint.

### Migration

1. Package the published Codex ACP adapter unchanged.
2. Package a compatible official Codex command and set `CODEX_PATH` only when
   the normal dependency resolution cannot select it.
3. Register `codex-acp` as the Codex `acpEntrypoint`.
4. Mount `~/.codex` and forward supported OpenAI environment variables.
5. Delete any obsolete placeholder behavior once the real adapter is wired.

The adapter itself is JavaScript, but the official npm Codex launcher selects a
platform-native Codex executable. AgentOS must provide a supported way to run
the official App Server command, such as an AgentOS-compatible upstream Codex
build. Do not modify the ACP adapter to emulate App Server. Treat executable
support as a core runtime/package-format workstream.

### Acceptance

- ACP translation is entirely the published adapter.
- The child process is the official Codex App Server implementation.
- Host ChatGPT/Codex auth works and refreshes through native Codex storage.
- Providers/models, reasoning effort, approval and runtime modes, sessions,
  multiple turns, cancel, tools, plans, terminals, MCP, skills, review commands,
  and slash commands pass.

## Shared parity test suite

Each migration must run the same black-box suite against two commands:

```text
reference: upstream adapter on host Node
candidate: the same maintained adapter packaged in an AgentOS VM
```

For Pi, compare the released Rust native command, its AgentOS WASM build, and
the pinned Node reference with the same fake Pi RPC transcript. Do not compare
the removed embedded adapter as a candidate; retain it only as a historical
feature-baseline column until every row has a disposition.

Normalize nondeterministic IDs, timestamps, paths, token counts, and provider
availability, then compare:

- initialization capabilities and auth methods;
- provider/model/config-option IDs, names, categories, and current values;
- modes, commands, agents, and metadata;
- new, list, load, resume, fork, and close lifecycle where supported;
- at least three ordered turns and resume followed by another turn;
- cancellation during streaming and a prompt after cancellation;
- queued or steering messages where supported;
- text, reasoning, plans, tool calls, tool updates, and terminal events;
- permission request/response behavior;
- model, effort/thinking, mode, and agent switching;
- slash commands and client-provided MCP servers;
- process exit, stdin EOF, and unexpected child-process failure.

Use deterministic mock providers for the required suite. Keep an opt-in live
credential smoke test for each harness to catch native auth and real catalog
drift. The parity test must invoke the packaged entrypoint, not an AgentOS mock
adapter or internal class.

## Delivery order

1. **OpenCode:** prove the current upstream Node ACP bundle with zero source
   patches and fix runtime gaps.
2. **Claude:** replace the custom adapter with Claude Agent ACP and fix runtime
   gaps exposed by the official SDK.
3. **Pi:** package `pi-acp@0.0.31` unchanged with the native Pi CLI, prove
   host/AgentOS parity, then remove the embedded SDK and custom Rust/WASI path.
4. **Codex:** package Codex ACP once the official App Server executable has a
   supported AgentOS execution path.
5. Run the full Gigacode end-to-end suite against all available agents and
   remove compatibility code that is no longer reachable.

## Decision log

- 2026-07-13: Keep harness behavior upstream; AgentOS owns runtime
  compatibility, packaging, mounts, and execution.
- 2026-07-13: Target zero OpenCode source patches. Bun is permitted only as an
  upstream build tool for a Node artifact.
- 2026-07-13: Use the Agent Client Protocol project's Claude adapter rather
  than the custom AgentOS ACP translation. Preserve necessary Claude SDK/CLI
  compatibility below that boundary and minimize it separately.
- 2026-07-14: Supersede the custom Rust/WASI Pi adapter decision. Package the
  standard `pi-acp@0.0.31` Node adapter unchanged and point it at the packaged
  Pi CLI through `PI_ACP_PI_COMMAND`.
- 2026-07-13: Use the Agent Client Protocol project's Codex adapter and the
  official Codex App Server rather than implementing Codex ACP in AgentOS.

## Open decisions

- Whether build-time Bun is acceptable long term. It is not a runtime
  dependency; replacing upstream's builder is unnecessary for Node
  compatibility.
- Which Pi RPC limitations require an explicit partial/unsupported ACP result;
  these must remain visible in the standalone feature table rather than being
  hidden by AgentOS-specific behavior.
- Which AgentOS-supported execution form will run the official Codex App Server
  without allowing arbitrary native executables inside the VM.
