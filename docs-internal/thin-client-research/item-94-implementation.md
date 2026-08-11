# Item 94 implementation map: sidecar-owned host-tool schema validation

Researched on **2026-07-16** against working revision `wurwpovw`. This is an
implementation brief only. It does not change production code, tests, or the
tracker status.

## Decision

**Priority: P1. Fix confidence: high.**

Move the Rust compatibility validator to `agentos-native-sidecar-core`, use it
at the native sidecar's last trusted boundary before a host-tool callback is
dispatched, and delete both existing copies:

- the complete 404-line validator in the Rust client; and
- the separate shallow validator in the native sidecar.

The protocol already carries each caller-authored schema in
`InitializeVmRequest.host_callbacks`, and the native sidecar already retains
that schema with its VM-owned toolkit registry. No protocol field, default,
client timer, or new sidecar state is needed.

The Rust client must continue to parse the callback's JSON wire string into a
`serde_json::Value`, find the exact VM-owned host closure, execute it, and
serialize its result. Those are necessary transport/host-resource duties. It
must not independently interpret the schema.

TypeScript is the intentional exception. It must continue to:

1. author tools with Zod;
2. project a structural JSON Schema for sidecar registration; and
3. call the complete Zod schema's `safeParseAsync` exactly once before invoking
   the host closure.

The sidecar validator is structural and never runs Zod effects. TypeScript's
single Zod parse remains authoritative for transforms, async refinements,
defaults, stripping, and the callback's typed input.

## Exact current issue

### Rust client owns a parallel schema interpreter

In `crates/client/src/agent_os.rs`:

- `AgentOs::create()` serializes `HostTool.input_schema` into each
  `RegisteredHostCallbackDefinition.input_schema`. This forwarding is correct.
- The same method stores the complete `HostTool`, including its schema,
  description, and timeout, in `VmHostToolRegistry.tool_map` after those fields
  have already been forwarded.
- `run_host_callback()` parses `HostCallbackRequest.input`, resolves the VM and
  callback key, then calls `validate_tool_input(&tool.input_schema, &input)`
  before executing the closure.
- `ToolInputSchemaViolation` through `compact_json` are a 404-line private JSON
  Schema interpreter. It implements null/empty schemas, `anyOf`, compatibility
  `oneOf`, `enum`, `const`, string or array-valued `type`, implicit object
  schemas, string lengths, numeric bounds, array bounds/items, object
  required/properties, and boolean or schema-valued `additionalProperties`.
  Unsupported JSON Schema keywords are silently ignored.

There is no direct unit coverage for this client-owned validator today. The
only Rust host-tool E2E (`crates/client/tests/os_instructions_e2e.rs`) uses a
simple object schema and valid input, so it does not establish which layer
rejects malformed input or protect the supported subset.

`crates/client/src/config.rs::HostTool` correctly describes
`input_schema` as a schema forwarded to the sidecar, but the `ToolCallback`
documentation also promises validated JSON. That promise is why deleting
validation outright without an authoritative replacement would be a behavior
regression.

### Native sidecar already owns the dispatch boundary, but only partially

In `crates/native-sidecar/src/tools.rs`:

- `register_host_callbacks()` validates bounded registration shape through
  `agentos_native_sidecar_core::tools::validate_toolkit_registration` and stores
  the complete registration in `VmState.toolkits`.
- `resolve_toolkit_command()` resolves permissions, parses the retained schema,
  parses `--json`, `--json-file`, or schema-derived flags, then calls the local
  `validate_tool_input_schema()` before constructing `ToolCommandResolution::Invoke`.
- `spawn_tool_process_events()` in `crates/native-sidecar/src/execution.rs`
  dispatches that already-resolved request to the host. This is the correct
  authoritative validation point: invalid guest input can fail before a
  reverse request is admitted and before any client callback runs.
- The local native validator is only shallow. It checks a root object,
  `required`, first-level property types, and boolean
  `additionalProperties: false`. It does not enforce nested structures,
  branches, constants/enums, lengths, or numeric/array bounds.

The real native service regression
`tools_javascript_child_process_rejects_invalid_json_file_input_before_dispatch`
already proves this ownership boundary for a wrong first-level integer: the
tool exits nonzero and the host invocation count remains zero. It does not
cover the broader subset currently interpreted by the Rust client.

### Shared core already owns registration policy

`crates/native-sidecar-core/src/tools.rs` already owns toolkit/tool name limits,
description/schema/example bounds, timeout limits, registry capacity, command
names, and the host-tool prompt/reference. Both native and browser sidecars
depend on this crate and use `validate_toolkit_registration()`.

That file is the minimal home for the existing compatibility subset. Adding a
third-party full JSON Schema engine would broaden behavior, add dependency and
compile cost, and make the migration harder to characterize. Item 94 should
move the supported subset without silently changing it. Standards corrections
such as exact-one-match `oneOf` semantics should be a separate tracked change.

### TypeScript is already on the intended boundary

In `packages/core/src/agent-os.ts`:

- `toolToSidecarDefinition()` calls `zodToJsonSchema(tool.inputSchema)` once
  while preparing registration.
- `handleHostCallback()` calls
  `tool.inputSchema.safeParseAsync(payload.input)` once and executes with
  `parsed.data`.

`packages/core/src/host-tools-zod.ts` deliberately projects only the structural
pre-effect schema. Existing tests in `host-tools-zod.test.ts`,
`toolkit-permissions.test.ts`, and `sidecar-tool-dispatch.test.ts` prove that
conversion does not run effects and that refinements/transforms run exactly
once at callback execution. Do not move or duplicate this Zod behavior.

## Recommended production edits

### 1. Put the compatibility validator in shared sidecar core

File: `crates/native-sidecar-core/src/tools.rs`.

Move `ToolInputSchemaViolation` and the complete helper chain from
`crates/client/src/agent_os.rs` into this module with behavior and exact error
format preserved. Expose only the narrow public entrypoint and error type:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInputSchemaViolation {
    path: String,
    expected: String,
    actual: String,
}

pub fn validate_tool_input(
    schema: &serde_json::Value,
    input: &serde_json::Value,
) -> Result<(), ToolInputSchemaViolation> {
    validate_tool_input_at_path(schema, input, "$")
}
```

Keep the fields private unless a real sidecar consumer needs structured access;
the existing public contract is the stable display text
`ToolInputSchemaViolation at <path>: expected <expected>, got <actual>`.
Implement `std::error::Error` in shared core so callers can preserve it without
message parsing.

Export `validate_tool_input` and `ToolInputSchemaViolation` from
`crates/native-sidecar-core/src/lib.rs` beside the other `tools` exports. Do not
add the core crate as a client dependency.

### 2. Replace the native sidecar's weaker duplicate

File: `crates/native-sidecar/src/tools.rs`.

Import the shared entrypoint, preferably aliased as
`core_validate_tool_input`. In `resolve_toolkit_command()`, retain the existing
schema parse and input parse, then replace the local validation call with:

```rust
if let Err(error) = core_validate_tool_input(&input_schema, &input) {
    return Ok(ToolCommandResolution::Failure(error.to_string()));
}
```

Delete local `validate_tool_input_schema()` and
`validate_tool_input_value_type()`. Preserve the existing failure path:
invalid input becomes tool stderr plus exit code 1, and no reverse host request
is dispatched. Do not convert a schema violation into a sidecar transport
rejection or a client callback response.

The browser sidecar has registration but currently no equivalent
schema-derived toolkit command dispatcher. It receives the shared validator
through the common crate without needing a parallel browser state machine or
an unused call. Add no browser-only validation path.

### 3. Delete Rust client schema interpretation and retained schema state

File: `crates/client/src/agent_os.rs`.

- Delete `ToolInputSchemaViolation`, `validate_tool_input`, and every private
  validator/description helper through `compact_json`.
- Remove the validation branch from `run_host_callback()`.
- Narrow `VmHostToolRegistry.tool_map` from
  `HashMap<String, HostTool>` to `HashMap<String, ToolCallback>`.
- During `AgentOs::create()`, continue serializing the exact
  `tool.input_schema` into the wire registration, but store only
  `Arc::clone(&tool.execute)` in the host map.
- In `run_host_callback()`, call the resolved closure directly with the decoded
  `Value`.

Update the import from `crate::config` to use `ToolCallback` instead of
retaining `HostTool` solely for the registry. The public `HostTool` config type
and its `input_schema` field remain; callers still need to author and forward a
schema.

Keep these client checks because they are transport/host ownership, not schema
policy:

- malformed callback JSON returns `Invalid host callback input`;
- missing VM ownership is rejected;
- unknown VM registry or callback key is rejected;
- callback result serialization failure is returned; and
- the exact callback error string is forwarded.

No TypeScript production file should change.

## Before tests: characterize the behavior before moving it

The parent has no direct Rust validator unit tests, so first add a table-driven
test beside `crates/client/src/agent_os.rs::tests` and run it while the private
validator is still present. Name it
`tool_input_schema_supported_subset_is_characterized` and cover at least:

- null and `{}` accept arbitrary input;
- `anyOf` and current compatibility `oneOf` accept a matching branch and reject
  no-match input;
- `enum`, `const`, and string/array-valued `type`;
- Unicode-aware `minLength`/`maxLength`;
- `minimum`, `exclusiveMinimum`, `maximum`, and `exclusiveMaximum` for number
  and integer;
- `minItems`, `maxItems`, recursive `items`, and an indexed error path;
- implicit object shape, missing required property, recursive properties,
  `additionalProperties: false`, and schema-valued `additionalProperties`; and
- exact path/expected/actual display output for representative failures.

Run on the parent implementation:

```sh
cargo test -p agentos-client --lib tool_input_schema_supported_subset_is_characterized -- --nocapture
```

Record the passing command in the Item 94 tracker before moving the test. Then
move the same case table to `crates/native-sidecar-core/src/tools.rs::tests`
rather than maintaining copies in both crates.

Also retain and record the existing authoritative native characterization:

```sh
cargo test -p agentos-native-sidecar --test service \
  tools_javascript_child_process_rejects_invalid_json_file_input_before_dispatch \
  -- --nocapture
```

## After tests

### Shared-core ownership

The moved table test must pass unchanged from shared core:

```sh
cargo test -p agentos-native-sidecar-core \
  tool_input_schema_supported_subset_is_characterized -- --nocapture
```

Add a focused native test beside the existing tool tests for a case the old
native validator missed, such as nested `items` plus `minimum`, and assert:

1. the process exits 1;
2. stderr contains the exact nested path and bound;
3. the sidecar request handler invocation count remains zero.

Run the native tool slice:

```sh
cargo test -p agentos-native-sidecar --test service \
  tools_javascript_child_process -- --nocapture
cargo test -p agentos-native-sidecar tools::tests -- --nocapture
```

### Rust forwards the schema but does not interpret callback input

Add a recording-transport test in `crates/client/src/agent_os.rs::tests` named
`initialize_vm_forwards_host_tool_schema_unchanged`:

1. Create `SidecarTransport::recording_for_test()`.
2. Construct an explicit `AgentOsSidecar` whose retained
   `SharedConnection` uses that transport and a pre-authenticated test
   connection id. This can be done inside the crate test module without a new
   production constructor.
3. Spawn `AgentOs::create()` with one `HostTool` whose schema exercises nested
   objects, arrays, bounds, branches, constants, and additional properties.
4. Decode and answer `OpenSessionRequest`.
5. Decode the following `InitializeVmRequest`, parse the registered
   `input_schema` string back to `Value`, and assert equality with the exact
   caller-authored `Value`.
6. Abort the still-pending create task after the assertion, before a VM/tool
   registry is installed.

This test must not call the shared validator. It proves the Rust client is a
presence-preserving serializer, not a policy owner.

Add a source guard (a small repository verifier or a test reading the source)
for the high-value absence claims:

- `crates/client/src/agent_os.rs` has no `ToolInputSchemaViolation` or
  `validate_tool_input`;
- `crates/native-sidecar/src/tools.rs` has no private
  `validate_tool_input_schema` or `validate_tool_input_value_type`; and
- `packages/core/src/agent-os.ts` still has exactly one
  `.safeParseAsync(payload.input)` call.

Run:

```sh
cargo test -p agentos-client --lib initialize_vm_forwards_host_tool_schema_unchanged -- --nocapture
cargo test -p agentos-client --lib
cargo check --workspace
cargo fmt --all -- --check
pnpm --dir packages/core exec vitest run \
  tests/host-tools-zod.test.ts \
  tests/toolkit-permissions.test.ts \
  tests/sidecar-tool-dispatch.test.ts
pnpm --dir packages/core check-types
```

`sidecar-tool-dispatch.test.ts` requires the real native sidecar/package
fixture and is the important end-to-end proof that structural sidecar
validation still composes with exactly-once Zod refinement/transform behavior.

## Risks and non-goals

- **Do not add `agentos-native-sidecar-core` to `agentos-client`.** That would
  merely relocate the code while preserving client-owned behavior.
- **Do not use a full JSON Schema crate in this migration.** It would change
  accepted/rejected inputs and possibly error ordering. Preserve the currently
  supported subset first.
- **Do not validate schemas at registration by recursively compiling or
  caching another representation.** Existing size/depth registration checks
  are bounded, and the VM-owned schema string is the source used at dispatch.
- **Do not remove TypeScript Zod validation.** Structural JSON Schema cannot
  reproduce transforms, async refinements, defaults, or typed output.
- **Do not retain the complete Rust `HostTool` after registration.** Only its
  host closure is inaccessible to the sidecar and therefore legitimately
  client-owned.
- The existing validator treats `oneOf` as first-success compatibility rather
  than exact-one-match JSON Schema semantics and ignores unsupported keywords.
  Preserve that behavior for Item 94; standards corrections require their own
  behavior decision and regressions.
- Validation moves earlier for Rust: malformed input fails inside the sidecar
  before reverse-request admission instead of returning from the Rust callback.
  Assert externally stable tool exit/stderr and zero callback execution rather
  than preserving which internal component authored the same violation.

## Expected diff boundary

Production:

- `crates/native-sidecar-core/src/tools.rs`
- `crates/native-sidecar-core/src/lib.rs`
- `crates/native-sidecar/src/tools.rs`
- `crates/client/src/agent_os.rs`
- optionally `crates/client/src/config.rs` for documentation-only wording

Tests/tracking:

- shared-core unit tests in `crates/native-sidecar-core/src/tools.rs`
- focused native service/tool tests
- Rust client recording-transport/source-absence coverage
- TypeScript tests run unchanged
- `docs/thin-client-migration.md`

No protocol schema, generated wire file, TypeScript production file, browser
sidecar production file, package-manager behavior, filesystem behavior,
permission policy, or runtime default should change.

Implement Item 94 in its own child JJ revision, stacked after the preceding
completed numbered item, and mark its tracker checkboxes complete only after
the before evidence, after evidence, focused real-sidecar regression,
workspace gates, scoped diff review, and independent sub-agent review pass.
