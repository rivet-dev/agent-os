---
name: update-acp
description: Audit AgentOS ACP feature coverage across the stable ACP v1 specification, each underlying agent harness and control interface, its upstream ACP adapter, and AgentOS public API/types. Use for ACP upgrades, adapter parity checks, or missing-feature audits.
---

# Update ACP

1. Pin the latest `schema-v1.*` ACP tag and the exact local harness and adapter versions. Keep the schema release, SDK/package release, and wire `protocolVersion` distinct. Ignore draft/v2 features unless clearly labeled.
2. Enumerate every stable ACP method, capability, content type, update, and lifecycle behavior. Include AgentOS-required extensions separately; do not present them as ACP requirements.
3. Audit every agent registered under `registry/agent/`. Prefer current upstream source over README claims:
   - OpenCode: native `opencode acp` and its internal harness APIs.
   - Claude: Claude Agent SDK/CLI and `@agentclientprotocol/claude-agent-acp`.
   - Codex: Codex App Server/CLI and `@agentclientprotocol/codex-acp`.
   - Pi: Pi core, the pinned Pi RPC contract, and `svkozak/pi-acp`.
4. Produce one table per agent. For every feature report: ACP requirement (`must`, optional, or extension), harness support, harness control-interface support (RPC/App Server/SDK/CLI), upstream adapter support, AgentOS sidecar support, AgentOS public API/type support, confidence, and source evidence. Use `yes`, `partial`, `no`, or `n/a`. Group rows only when every status matches, and name every grouped feature. Name equivalent harness primitives such as `switch_session`; do not require them to share the ACP method name, and never infer harness support only from adapter behavior.
5. Inspect AgentOS at minimum in `crates/agentos-sidecar/src/acp_extension.rs`, `packages/core/src/agent-session-types.ts`, `packages/core/src/agent-os.ts`, registry manifests, and relevant tests. Verify both runtime forwarding and public exposure; preserved unknown JSON alone is not typed API support.
6. After the matrix, list prioritized action items grouped by owner: upstream adapter, harness/control interface, AgentOS sidecar/runtime, AgentOS API/types, or packaging/tests. State when no action is justified because the harness lacks the feature or ACP makes it optional.
7. Do not change code during the audit. End by asking: **Do you want me to fix these? If so, which priorities or agents should I include?**
