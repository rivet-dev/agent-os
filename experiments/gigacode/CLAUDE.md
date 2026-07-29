# Gigacode architecture rules

Gigacode is intentionally a relatively thin OpenCode HTTP/SSE forwarder and
data-shape converter over AgentOS and ACP. Keep OpenCode-specific routing,
queuing, event envelopes, permission aliases, and message/tool/model shape
translation here.

Do not add retries, actor reloads, adapter or ACP-session replacement, transcript
reconstruction, or complex recovery state to compensate for failures below this
layer. Surface the original error to the OpenCode client. Fix lifecycle,
cancellation, resume, process, transport, discovery, and structured-error defects
in AgentOS instead, then consume the corrected AgentOS API here.

One-shot awaited cleanup or process containment is allowed when it is needed to
stop work safely. It must still surface the failure and must not recreate a
session, replay a request, or hide the lower-level error.

Until AgentOS exposes cancellation completion with an explicit reusable-session
guarantee, unload the live ACP adapter after every cancelled prompt and resume
its durable AgentOS session on the next turn. A cancellation acknowledgement or
prompt-listener return alone does not prove that an adapter has stopped emitting
tool calls.

One logical OpenCode session owns one ACP harness. Once selected, a different
harness request must fail and tell the caller to create another session.

Automatic model discovery is allowed only when the daemon has no valid model
cache, and that first-start discovery must finish before the OpenCode API/TUI is
available. A cached startup must not probe. Afterward, the only discovery path is
the explicit `gigacode models refresh` command.

When a defect belongs to one of the maintained ACP adapter forks, modify and test
the fork directly, push that fix, and update AgentOS's exact pinned source and
checksum. Do not add a Gigacode workaround for adapter behavior we control.

## Registered follow-ups

Fix these after the Codex model/reasoning update is complete:

- Recover the actor `sessionEvent` subscription after an engine, actor, or
  WebSocket reconnect. A connection must not remain apparently usable after its
  remote event subscription has disappeared; otherwise active ACP tool and text
  events never reach the OpenCode client.
- Do not apply the 60-second Rivet health/route-dispatch guard to long-running
  prompt actions. Prompt completion must either return normally or fail
  explicitly, and GigaCode must reconcile committed AgentOS history and leave
  `busy` after every terminal outcome.

Reproduction: `ses_ad68f48651684cfcab8b593a2fd34a30`. AgentOS completed the
turn, reported the ACP session idle, and committed 49 events through sequence
49, while GigaCode received no live events and remained busy waiting for the
prompt action response.
