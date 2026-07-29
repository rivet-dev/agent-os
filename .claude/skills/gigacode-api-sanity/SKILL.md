---
name: gigacode-api-sanity
description: Compare GigaCode's OpenCode-compatible HTTP/SSE API with native OpenCode in isolated temporary workspaces. Use when asked to sanity-check, re-test, or manually validate Claude Sonnet streaming, multi-turn sessions, tool calls, permissions, file edits, cancellation, or post-cancel reuse.
---

# GigaCode API Sanity

Run a black-box parity check. Do not modify product source or disturb an existing daemon.

## Set up

1. Run `pwd` and `jj log -r @`; record the revision and both runtime versions.
2. Create one fresh temp root containing separate native and GigaCode workspaces, logs, and raw traces.
3. Pick unused API/Rivet ports. Never reuse or stop the default ports `2468`/`2469`.
4. Start native `opencode serve` in its workspace. Start GigaCode in its workspace with unique state and port environment variables. Prefer the current checkout; disclose any installed-runtime fallback.
5. Open `/event` before creating a session and preserve the unmodified SSE stream.
6. Check `/global/health` and `/provider`. Require a working Claude Sonnet credential on both sides before judging model behavior. For GigaCode, pin Sonnet with `GIGACODE_SESSION_ENV_JSON`, for example `{"ANTHROPIC_MODEL":"claude-sonnet-4-6"}`.

Treat authentication failure as a setup limitation, not an API deviation. Do not claim full parity without a valid native Sonnet run.

## Run the same scenario on each server

Use direct HTTP requests and save status, headers, body, timing, and SSE frames for every step:

1. `POST /session`; record the complete response.
2. Send two synchronous `POST /session/{id}/message` prompts in the same session. Make turn two depend on turn one.
3. Prompt the agent to create `api-edit.txt` with exact contents using a tool. Poll `GET /permission`, reply once through `POST /permission/{id}/reply`, and verify both the tool-part lifecycle and host file contents.
4. Start a long tool call with `POST /session/{id}/prompt_async`. Use a command such as `touch <workspace>/sleep-started && sleep 20`, poll permissions concurrently, and approve the tool if required. Wait for either the marker file or the tool part to be `running` before calling `POST /session/{id}/abort`. The marker proves the command actually began even when an implementation publishes tool events late. Merely observing the session as busy can mean the model is still deciding what tool to call and does not validate tool-process cancellation.
5. Immediately send another synchronous prompt on that same session. Poll and resolve permissions concurrently while that request is in flight: an aborted tool instruction remains in model history and some models repeat it on the next turn. The follow-up must finish and return to idle.
6. Fetch final messages, parts, permissions, and `/session/status`.

Use equivalent prompts and the same Sonnet release. Allow provider/model identifiers to differ only where the implementations require it.

## Compare

Report a compact table with `surface | native | GigaCode | deviation | severity`. Check:

- HTTP status, content type, body shape, IDs, and error placement
- SSE event types, order, identifiers, deltas, tool states, and busy-to-idle transitions
- session-create fields and `session.created` payloads
- permission asked/replied behavior and file-edit completion
- abort latency, true quiescence, and post-cancel session reuse
- final message history and `/session/status`

Call out known compatibility-sensitive details explicitly: opaque versus numeric event IDs, idle status entries, session metadata fields, `session.updated`, and native-only catalog/plugin/diff events. Separate deliberate boundary differences from regressions.

## Preserve and clean up

- Keep raw request/response JSON, SSE traces, server logs, prompts, ports, PIDs, revision, versions, and timing in the temp root; print its path in the report.
- Stop only processes created by this run and verify their ports are closed.
- If AgentOS, RivetKit, or secure-exec behaves unexpectedly, update the matching `~/.agents/friction/*.md` entry without duplicating an existing issue.
- A pass requires both implementations to complete every scenario. Flag bootstrap timeouts, unresolved permissions, incomplete tool parts, abort-before-quiescence, follow-up hangs, missing files, or missing SSE as failures. Never await a synchronous prompt before starting its permission poller; doing so manufactures a timeout whenever the model requests a tool.
