# Agent OS Sidecar Extension

- Author ACP behavior as an `Ext` extension over `agentos-protocol`; do not add new top-level sidecar request/response variants for agent-session RPCs.
- Keep `agentos-protocol` as the only ACP payload schema source; extension requests, responses, events, and callbacks must use the generated BARE types.
- Keep generic agentos sidecar code agent-agnostic; ACP namespace handling belongs in this wrapper extension, not in agentos transport or kernel layers.
- Extension guest work must still run through the kernel boundary via `ExtensionContext`; never spawn host-native agent adapters or touch host files directly from extension logic.
- Emit live session notifications as generated `AcpEvent` payloads in `EventPayload::Ext`; do not add event cursor replay to snapshot state.
