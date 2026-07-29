# OpenCode compatibility audit

This document tracks Gigacode against the OpenCode 1.17.20 TUI and its
`@opencode-ai/sdk/v2` client. The compatibility target is the attached OpenCode
TUI backed by AgentOS sessions. It is not a promise to reproduce OpenCode's
unrelated local server facilities such as PTY hosting, provider OAuth, LSP
management, or worktree synchronization.

The status column means:

- **Supported**: exercised end to end through the real TUI or SDK.
- **Implemented**: added after the audit and covered by a regression test.
- **Client-local**: OpenCode implements the behavior without a server route.
- **Honest empty**: the schema is valid and an empty result accurately means
  AgentOS has no objects of that type to expose.
- **Unsupported**: outside the compatibility target and not advertised as
  working.

## User workflows

| Workflow | Baseline | Final status | Evidence or remaining boundary |
| --- | --- | --- | --- |
| Start `gigacode` without a daemon | Supported | Supported | CLI autospawn E2E |
| Reuse one daemon from multiple cwd values | Supported | Supported | Multi-workspace E2E proves one workspace actor per canonical cwd and multiple sessions per actor |
| List harnesses and models | Supported | Supported | Cold/warm provider and real `/models` TUI E2E |
| Create, list, select, rename, and delete sessions | Partial | Implemented | SDK create/list/update/delete against the SQLite coordinator, cwd isolation, and real TUI-created sessions |
| Resume a conversation after daemon restart | Partial | Implemented | Retains the stable AgentOS session ID and delegates durable resume to AgentOS on the next turn |
| Prompt synchronously and asynchronously | Partial | Implemented | SDK sync prompts and exact 204 async response |
| Stream text and reasoning | Missing | Implemented | ACP text/reasoning updates map to OpenCode deltas; text is exercised with tool streaming |
| Display agent tool calls and results | Missing | Implemented | Real Claude tool lifecycle projected into native-shaped OpenCode `write`, `edit`, and `bash` parts while retaining ACP provenance |
| Queue prompts entered during an active turn | Missing | Implemented | FIFO SDK and real TUI queued-turn regression tests |
| Cancel a running turn and continue the conversation | Broken | Implemented | SDK/TUI abort waits for prompt termination before idle; if AgentOS cannot cancel cleanly, Gigacode closes the ACP session for containment and returns an error without replacement |
| Change ACP harness within a session | Previously implemented by transcript handoff | Explicit boundary | The first prompt fixes the harness; later requests for another provider return a clear error and require a new logical session |
| Run `!` shell commands | Supported | Supported | SDK and real TUI success, failure, cancellation, and reuse E2E |
| Show and answer permission requests | Partial | Implemented | Permission asked/replied lifecycle, both reply routes, and cleanup E2E |
| Use `@file` completion and file prompt parts | Missing | Implemented | Bounded v2 `/api/fs/find`; file/agent/subtask parts become ACP prompt context |
| Open the Rivet debugger from OpenCode | Missing | Implemented | `/command` discovery and real TUI `/gigacode-debugger` execution |
| `/new`, `/sessions`, `/models`, `/agents` | Partial | Supported | TUI owns dialogs; attached-server session/model/agent data is schema-compatible |
| `/help`, `/debug`, `/themes`, `/timestamps`, `/thinking` | Client-local | Client-local | OpenCode owns these dialogs/toggles |
| `/move`, `/editor`, `/copy`, `/export`, `/exit` | Client-local | Client-local | OpenCode owns these local terminal/filesystem actions |
| `/mcps`, `/skills` with no configured entries | Honest empty | Honest empty | Gigacode returns schema-correct empty collections |
| `/diff`, `/timeline`, todos, children | Honest empty | Honest empty | AgentOS does not currently project OpenCode snapshots/diffs/todos/subsessions |
| `/share` and `/unshare` | False advertisement | Unsupported | `/config` declares sharing disabled; Gigacode has no share host |
| `/compact`, `/init` | False success | Explicit boundary | Compact returns unsupported; init runs an actual AgentOS instruction-analysis prompt |
| `/fork`, `/undo`, `/redo` | Missing | Unsupported | Requires conversation/filesystem snapshot semantics AgentOS does not expose |
| Questions | Missing | Unsupported | AgentOS exposes permission callbacks but no question callback |

## TUI bootstrap and events

| Surface | Baseline | Final status | Notes |
| --- | --- | --- | --- |
| `GET /global/health` | Supported | Supported | Includes Gigacode/Rivet health extensions |
| `GET /global/event`, `GET /event` | Partial | Implemented | Monotonic SSE IDs, replay, and per-directory filtering E2E |
| `GET /config/providers`, `GET /provider` | Partial | Implemented | v2 provider/model schema plus legacy aliases; cold discovery and warm global cache E2E |
| `GET /config`, `PATCH /config` | Partial | Partial | Read is compatible and disables sharing; patches are accepted for the request but not persisted |
| `GET /agent` | Partial | Supported | One schema-compatible build agent; harnesses remain provider/model groups |
| `GET /path` | Supported | Supported | Correct machine and active-directory paths |
| `GET /project`, `/project/current` | Partial | Implemented | Local project, current project, and project directories route |
| `GET /command` | Honest empty | Implemented | Advertises the Gigacode debugger command |
| `GET /mcp`, `/lsp`, `/formatter`, `/skill` | Honest empty | Honest empty | No corresponding AgentOS projection exists yet |
| `GET /vcs`, `/vcs/diff`, `/vcs/status` | Partial | Honest empty | Branch remains a display fallback; no OpenCode-owned VCS snapshot source exists |
| `GET /provider/auth` | Honest empty | Honest empty | Host credentials are mounted; Gigacode does not broker provider OAuth |
| `/experimental/capabilities`, `/experimental/console` | Unsupported | Unsupported | OpenCode explicitly tolerates these 404 responses |

## Session API

| SDK method | HTTP route | Baseline | Final status |
| --- | --- | --- | --- |
| `session.list` | `GET /session` | Partial: leaked all cwd values and was unsorted | Implemented: SQLite-coordinator-backed, cwd-filtered, and stable-sorted |
| `session.create` | `POST /session` | Supported basic actor creation | Implemented: creates a coordinator record and reuses the cwd workspace actor |
| `session.get/update/delete` | `/session/{id}` | Supported basic paths | Implemented coordinator lifecycle and ACP-session cleanup coverage |
| `session.status` | `GET /session/status` | Partial and race-prone | Implemented FIFO prompt/shell lifecycle; only busy sessions are returned, matching native OpenCode's empty idle response |
| `session.messages/message` | `GET /session/{id}/message*` | Memory-only | Implemented bounded atomic persistence across daemon restart |
| `session.prompt` | `POST /session/{id}/message` | Completed text only | Implemented rich prompt parts, deltas, tools, permissions, and typed errors |
| `session.promptAsync` | `POST /session/{id}/prompt_async` | Wrong 200 JSON response and unsafe concurrency | Implemented exact 204 plus bounded FIFO |
| `session.abort` | `POST /session/{id}/abort` | Quiescent | Awaits ACP cancellation before reporting idle; a failed/non-quiescent cancellation is contained by closing the ACP session and returned as an error without retry or replacement |
| `session.shell` | `POST /session/{id}/shell` | Supported command/output | Implemented success/error/cancellation and consistent lifecycle |
| `session.command` | `POST /session/{id}/command` | Missing | Implemented debugger command |
| `session.children/todo/diff` | GET subroutes | Honest empty | Honest empty |
| `session.summarize/init` | POST subroutes | False success | Summarize is explicit 501; init runs a real prompt |
| `session.fork/share/unshare/revert/unrevert` | POST/DELETE subroutes | Missing | Unsupported until AgentOS exposes required semantics |
| Message/part mutation | DELETE/PATCH message/part routes | Missing | Unsupported; attached TUI does not need it for normal turns |

## Permission API

The OpenCode 1.17.20 TUI uses `GET /permission`, the `permission.asked` event,
and `POST /permission/{requestID}/reply`. The legacy SDK also exposes
`POST /session/{id}/permissions/{permissionID}`. Gigacode now supports both
reply paths, emits asked/replied events, rejects abandoned waits, and cleans up
requests after completion, cancellation, or deletion. The E2E suite performs a
real Claude tool request, grants it once, and verifies the resulting tool part.
Public `per_*` IDs are globally unique; the adapter's ACP permission ID is kept
separately so concurrent sessions cannot overwrite one another.

## Harness selection

The first prompt binds an OpenCode session to one AgentOS harness. Later model
changes may select another model or variant exposed by that harness, but a model
whose provider selects another harness is rejected. Start a new OpenCode session
to use Claude, Pi, Codex, or OpenCode through a different ACP adapter.

## Deliberate non-goals

These SDK groups belong to a full OpenCode server rather than the AgentOS-backed
TUI layer. They remain unsupported unless AgentOS gains a corresponding source
of truth:

- PTY create/list/update/connect routes (Gigacode has its own `shell` command).
- Provider OAuth and credential mutation; host-native Claude/Codex/Pi auth and
  OpenCode XDG config/data are mounted into each VM instead.
- MCP add/connect/disconnect/auth management.
- LSP, formatter, and tool-registry management.
- OpenCode worktree and cross-device sync APIs.
- Share hosting.
- OpenCode-owned VCS apply/diff snapshots, fork, revert, and unrevert semantics.
- TUI remote-control queue routes; an attached local TUI handles its own input.

## Regression standard

A feature is not marked implemented until it has the narrowest applicable SDK
test and, for visible terminal behavior, a tmux snapshot test using the real
OpenCode binary. The acceptance suite must cover at least:

1. legacy and v2 SDK bootstrap,
2. session directory isolation and TUI switching,
3. transcript persistence across daemon restart,
4. streaming text/tool events,
5. FIFO queued turns,
6. quiescent in-place ACP cancellation followed by another successful logical turn,
7. slash command discovery and the Gigacode debugger command,
8. shell mode and normal multi-turn ordering.

The file-edit permission probe must start the prompt without awaiting it, poll
and answer the permission concurrently, and only then await the prompt result.
Awaiting the prompt before permission polling manufactures the permission
timeout it is meant to detect. LLMock covers this flow deterministically. Real
OpenCode 1.17.20/Sonnet file and shell turns provide the reference tool-part
shapes; full model-output parity is broader than the mock-backed suite.
