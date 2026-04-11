# Sidecar BARE Migration Plan

`agent_os_sidecar_v1.bare` is the canonical schema for the sidecar wire protocol value carried on native transports. It covers every current `request`, `response`, `event`, `sidecar_request`, and `sidecar_response` frame shape from [`crates/sidecar/src/protocol.rs`](../src/protocol.rs).

## Framing

The native sidecar transport keeps the current framing boundary during migration:

- 4-byte big-endian length prefix
- one encoded `ProtocolFrame` payload immediately after the prefix

US-083 and US-084 should replace only the payload codec first. They should not redesign the outer length prefix at the same time, because the current stdio/native-process tests and bridge code already rely on that framing contract.

## Compatibility Rules

The migration keeps the current semantic invariants unchanged across codecs:

- `ProtocolSchema.name` remains `agent-os-sidecar`
- `ProtocolSchema.version` remains `1`
- host-originated `request_id` values stay positive
- sidecar-originated `request_id` values stay negative
- ownership scope rules and response-correlation rules stay exactly the same
- `max_frame_bytes` and duplicate-response hardening stay transport-level requirements, not JSON-specific behavior

## JSON Boundary

The current protocol still has several fields modeled as `serde_json::Value` on the Rust side and `unknown`/structured JS values on the TypeScript side. BARE v1 represents those fields as `JsonUtf8`:

- UTF-8 JSON text inside the BARE field
- canonicalized by the codec before hashing/comparison in tests
- intentionally temporary until later protocol work replaces them with BARE-native typed payloads

This applies to fields such as session config blobs, ACP notifications, mount plugin configs, tool schemas/inputs, JS bridge arguments, and tool results.

## Rollout Plan

1. US-082: lock the schema and this plan in repo, with tests that fail if Rust adds a new frame payload without updating the schema.
2. US-083: add a Rust BARE codec alongside the existing JSON codec while preserving the current 4-byte big-endian frame prefix.
3. US-083: make the Rust decoder dual-stack by inspecting the first payload byte after the length prefix.
   JSON frames begin with `{` today, while BARE frames begin with a union tag byte/varint, so the decoder can distinguish the two without an extra wrapper frame.
4. US-083: once a connection's first successfully decoded frame is known, pin the connection to that codec for all later frames on that transport.
5. US-084: teach the TypeScript native sidecar client and related bridge transports to emit and decode the BARE payload form using the same schema.
6. US-084: keep JSON decode support only for the migration window; once both sides default to BARE and the targeted tests are green, delete JSON encoding and the dual-stack sniffing path.

## Normalization Notes

BARE does not have Serde's "omitted but defaults to empty list/map" behavior. The codec should therefore normalize these fields explicitly:

- omitted/defaulted collection fields in JSON become empty lists/maps in BARE
- optional fields remain explicit `optional<T>` values
- response payloads that are currently "rejected" remain valid correlated responses, just as they do in `ResponseTracker`

That normalization rule is part of schema compatibility and should be preserved in tests when the codec lands.
