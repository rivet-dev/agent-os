# Persistence & Sleep

How agentOS persists files and manages sleep/wake cycles.

agentOS persists the `/home/agentos` filesystem, durable session catalog, and completed session history across actor sleep. A later client call wakes a fresh VM. Adapter processes, running commands, shells, live subscriptions, and in-progress ACP deltas do not survive VM shutdown.

## What persists across sleep

| Data | Storage | Persists? |
|------|---------|-----------|
| Files in `/home/agentos` | Actor SQLite over UDS | Yes |
| Preview URL tokens | Actor SQLite | Yes |
| Session catalog and configuration | Actor SQLite over UDS | Yes |
| Completed ACP session history | Actor SQLite over UDS | Yes |
| Live ACP adapter process | VM memory | No; restored lazily |
| In-progress message deltas | Live event stream | No |
| Cron job definitions | VM memory | No |
| Running processes | VM kernel | No |
| Active shells | VM kernel | No |
| In-memory mounts | VM memory | No |

The native sidecar reads and writes filesystem chunks directly through the actor's authenticated SQLite Unix socket. File contents do not pass through the TypeScript or JavaScript actor layer. VM creation supplies one SQLite descriptor, which the sidecar resolves once and shares with filesystem metadata, filesystem blocks, and core session persistence; plugins do not open additional UDS or file connections.

## Sleep and active turns

An active prompt turn uses RivetKit's keep-awake scope through the terminal SQLite commit. An idle durable session does not keep the actor awake.

```text
Actor becomes idle -> idle timeout -> actor sleeps and the VM shuts down

listSessions/readHistory -> actor wakes -> VM boots -> SQLite is read without starting an adapter

prompt -> actor wakes -> VM boots -> adapter is restored lazily -> turn runs
```

RivetKit's default idle sleep timeout is 30 seconds. agentOS sets the graceful shutdown budget to 15 minutes and the action timeout to the largest Node timer-safe delay (2,147,483,647 ms, about 24.8 days), so a normal human permission review is not cut off by the previous 15-minute bound. These can be changed through the actor's `options` configuration.

## Sleep vs destroy

| | Sleep | Destroy |
|-|-------|---------|
| Filesystem | Preserved | Deleted |
| Preview tokens | Preserved | Deleted |
| Session catalog and completed history | Preserved | Deleted |
| Adapter process, live deltas, and subscriptions | Lost; restored or recreated as needed | Lost |
| Processes and shells | Lost | Lost |

## VM lifecycle events

Use native RivetKit subscriptions, `connection.on("vmBooted", ...)` and `connection.on("vmShutdown", ...)`, to observe actor-owned VM lifecycle changes. These are hosting events and are intentionally absent from Core.

## Reading durable state after sleep

When the actor wakes, a fresh VM is created over the same actor SQLite database. Files under `/home/agentos`, the session catalog, and completed history remain available. `listSessions()`, `getSession()`, and `readHistory()` read that stored state without starting an ACP adapter. Prompting an existing session ID restores its adapter lazily, preferring native ACP `session/load` when supported and falling back to a new adapter session with bounded durable history context when necessary.

Live subscriptions resume only from new events. Ephemeral message deltas that had not completed before shutdown are not reconstructed.

## SQLite tables

`agentos_fs_metadata_heads` and `agentos_fs_metadata_chunks` store the chunked inode and directory metadata for each filesystem namespace. `agentos_fs_blocks` stores content-addressed file chunks.

`agentos_core_sessions` stores durable session metadata and cached ACP negotiation state. `agentos_core_events` stores exact native ACP payloads plus a compact scalar AgentOS envelope. `agentos_core_prompts`, `agentos_core_permission_records`, and `agentos_core_permission_outcomes` store bounded prompt, idempotency, and permission bookkeeping. The sidecar treats SQLite as the source of truth; it does not depend on adapter-owned history for listing or reading sessions.

The filesystem, core, and TypeScript actor independently own `agentos_fs_schema_version`, `agentos_core_schema_version`, and `agentos_actor_schema_version`. There is no shared schema-version table or global migration sequence. `agentos_actor_preview_tokens`, `agentos_actor_dynamic_mounts`, and `agentos_actor_linked_software` are actor-owned hosting metadata.

This per-VM database is trusted plaintext storage. Session environment values, MCP credentials, prompts, messages, and tool or permission payloads may be stored without encryption or redaction so they can survive sleep. Protect database and backup access accordingly. See [Sessions & Persistence](/docs/architecture/sessions-persistence/) for exact event storage and retention bounds.