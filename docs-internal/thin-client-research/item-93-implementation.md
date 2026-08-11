# Item 93 exact implementation brief: sidecar-owned VM runtime/config defaults

Status: research and implementation brief only. This document does not change
production behavior or mark Item 93 complete.

Researched on **2026-07-16** against the stack containing Items 1-92 and 100.
Priority is **P1**. Root-cause confidence is **high (99%)** and recommended-fix
confidence is **high (98%)**.

## Outcome

Make only the atomic `InitializeVm` request presence-aware:

1. `runtime` becomes `optional<GuestRuntimeKind>` and `config` becomes
   `optional<JsonUtf8>` in the lockstep BARE schema;
2. TypeScript and Rust omit those fields when the caller supplied no runtime or
   VM configuration;
3. one helper in `agentos-native-sidecar-core` resolves omission to
   `GuestRuntimeKind::JavaScript` and the JSON text `{}`; and
4. native and browser `initialize_vm` call that helper before invoking their
   existing `create_vm` implementation.

This is a move to the shared sidecar/runtime, not a deletion of the defaults.
The sidecar still needs a concrete runtime and a parseable `CreateVmConfig` to
create a VM. The SDKs do not need to choose either value. Do **not** make the
lower-level `CreateVmRequest` optional in this item: it is already the concrete
sidecar-internal creation operation, while `InitializeVm` is the high-level
AgentOS transaction whose omitted values require normalization.

No compatibility branch is needed. The protocol, clients, native sidecar, and
browser sidecar release in lockstep.

## Current issue, with exact code

### TypeScript manufactures both defaults

`packages/core/src/agent-os.ts`, inside `AgentOs.create`, always constructs a
`CreateVmConfig` and always calls:

```ts
const nativeVm = await client.initializeVm(session, {
	runtime: "java_script",
	config: createVmConfig,
	// explicit mounts/packages/callbacks follow
});
```

With ordinary omitted VM options, `createVmConfig` serializes as `{}`. Its
`permissions` property is currently inserted with an `undefined` value, which
JSON serialization drops, but the client still authored and forwarded the
empty object.

`packages/runtime-core/src/sidecar-process.ts`,
`SidecarProcess.initializeVm`, requires both fields in its options type and
copies them into the live request. `packages/runtime-core/src/request-payloads.ts`,
`toGeneratedRequestPayload`, then always converts the runtime and stringifies
the config.

The existing characterization is visible in
`packages/runtime-core/tests/sidecar-process.test.ts`: the initialization
request is expected to contain `runtime: "java_script"` and `config: {}`.
`packages/runtime-core/tests/request-payloads.test.ts` similarly expects the
generated values `GuestRuntimeKind.JavaScript` and `"{}"`.

### Rust manufactures the same defaults

`crates/client/src/agent_os.rs`, `AgentOs::create`, always serializes the result
of `serialize_create_vm_config_for_sidecar`. `CreateVmConfig::default()` has no
present fields and serializes as `{}`, but the string is still put on the wire.
The adjacent request literal always sets:

```rust
wire::InitializeVmRequest {
    runtime: wire::GuestRuntimeKind::JavaScript,
    config: create_vm_config,
    // presence-aware collections follow
}
```

The high-level Rust API does not expose a caller runtime selection, so there is
no caller runtime value to preserve here. It should always send `None`. An
explicit non-default `CreateVmConfig` must remain `Some(exact_json)`.

### The protocol makes omission impossible

`crates/sidecar-protocol/protocol/agentos_sidecar_v1.bare` currently declares:

```bare
type InitializeVmRequest struct {
  runtime: GuestRuntimeKind
  config: JsonUtf8
  # ...
}
```

Consequently the generated Rust fields are concrete and
`packages/runtime-core/src/generated-protocol.ts` exposes concrete
`GuestRuntimeKind` and `JsonUtf8` values. This is why merely changing the two
high-level clients is insufficient.

### Native and browser currently consume concrete values independently

`crates/native-sidecar/src/service.rs`, `NativeSidecar::initialize_vm`, and
`crates/native-sidecar-browser/src/wire_dispatch.rs`,
`BrowserWireDispatcher::initialize_vm`, each construct a `CreateVmRequest`
directly from `payload.runtime` and `payload.config`. There is no shared
normalization point today.

The concrete `create_vm` implementations in
`crates/native-sidecar/src/vm.rs` and
`crates/native-sidecar-browser/src/wire_dispatch.rs` already parse and validate
the config and apply shared environment, permission, filesystem, and limit
defaults. They should remain unchanged. Item 93 only supplies their concrete
runtime/config inputs from one shared omission normalizer.

## Exact patch

### 1. Make the generated wire fields optional

Edit `InitializeVmRequest` in
`crates/sidecar-protocol/protocol/agentos_sidecar_v1.bare`:

```bare
type InitializeVmRequest struct {
  runtime: optional<GuestRuntimeKind>
  config: optional<JsonUtf8>
  mounts: optional<list<MountDescriptor>>
  packages: optional<list<PackageDescriptor>>
  packagesMountAt: optional<str>
  hostCallbacks: optional<list<RegisterHostCallbacksRequest>>
}
```

Regenerate TypeScript with:

```sh
pnpm --dir packages/build-tools build:protocol
```

Expected generated changes in
`packages/runtime-core/src/generated-protocol.ts` are `GuestRuntimeKind | null`
and `JsonUtf8 | null`, using the generated optional readers/writers. Rust code
is generated at build time from the same schema and becomes
`Option<GuestRuntimeKind>` / `Option<JsonUtf8>` automatically. Do not hand-edit
generated Rust output under `target/`.

### 2. Add the one shared normalizer

Add `crates/native-sidecar-core/src/vm_initialization.rs` with a pure helper:

```rust
use agentos_sidecar_protocol::wire::{
    CreateVmRequest, GuestRuntimeKind, InitializeVmRequest,
};

pub fn initialize_vm_create_request(payload: &InitializeVmRequest) -> CreateVmRequest {
    CreateVmRequest {
        runtime: payload
            .runtime
            .clone()
            .unwrap_or(GuestRuntimeKind::JavaScript),
        config: payload
            .config
            .clone()
            .unwrap_or_else(|| String::from("{}")),
    }
}
```

Declare the module and re-export `initialize_vm_create_request` from
`crates/native-sidecar-core/src/lib.rs`.

Keep this helper deliberately small. It chooses only the two values that the
concrete `CreateVmRequest` requires; it must not parse config, add environment,
select permissions, bootstrap the filesystem, or duplicate anything already
owned by `create_vm` and shared runtime helpers.

Add unit tests in the new module that cover all presence combinations:

- both omitted become JavaScript plus exact `"{}"`;
- explicit WebAssembly/JavaScript runtime survives unchanged;
- explicit config JSON survives byte-for-byte when runtime is omitted; and
- explicit runtime with omitted config gets only the config default.

Testing the combinations independently prevents an incorrect all-or-nothing
normalizer.

### 3. Use the helper in both sidecars

In `crates/native-sidecar/src/service.rs`, replace the inline create request in
`NativeSidecar::initialize_vm`:

```rust
let create_payload =
    agentos_native_sidecar_core::initialize_vm_create_request(&payload);
let created_dispatch = self.create_vm(request, create_payload).await?;
```

In `crates/native-sidecar-browser/src/wire_dispatch.rs`, make the corresponding
replacement in `BrowserWireDispatcher::initialize_vm`:

```rust
let create_payload =
    agentos_native_sidecar_core::initialize_vm_create_request(&payload);
let created_dispatch = self.create_vm(request, create_payload);
```

The rest of each atomic configure/register/rollback transaction stays exactly
as it is. Borrow the full payload for normalization before moving its mounts,
packages, or callbacks.

### 4. Preserve omission in runtime-core

In `packages/runtime-core/src/request-payloads.ts`, make the live payload fields
optional:

```ts
| {
		type: "initialize_vm";
		runtime?: LiveGuestRuntimeKind;
		config?: CreateVmConfig;
		// existing optional fields
	}
```

Change the generated conversion to distinguish omission from a present value:

```ts
runtime:
	payload.runtime === undefined
		? null
		: toGeneratedGuestRuntimeKind(payload.runtime),
config:
	payload.config === undefined
		? null
		: stringifyJsonUtf8(payload.config, "initialize VM config"),
```

Do not use truthiness: explicit empty configuration `{}` is a present caller
value and must still serialize as `Some("{}")`/`"{}"`.

In `packages/runtime-core/src/sidecar-process.ts`, make `runtime` and `config`
optional in `SidecarProcess.initializeVm` and conditionally spread only fields
that are not `undefined` into the live request. The transport must not choose a
fallback.

### 5. Stop TypeScript Core from authoring defaults

In `packages/core/src/agent-os.ts`, build `createVmConfig` using conditional
spreads for every explicit input. In particular, replace the unconditional
`permissions: sidecarPermissions` property with:

```ts
...(sidecarPermissions === undefined
	? {}
	: { permissions: sidecarPermissions }),
```

Remove `runtime: "java_script"` from the `initializeVm` call. Include `config`
only when the built object has at least one own field:

```ts
const nativeVm = await client.initializeVm(session, {
	...(Object.keys(createVmConfig).length === 0 ? {} : { config: createVmConfig }),
	// existing explicit mounts/packages/callbacks
});
```

`Object.keys` is safe only after all absent inputs are conditionally omitted.
Do not stringify and parse the object in Core, and do not drop explicit empty
nested values such as `rootFilesystem: {}` or explicit empty lists.

### 6. Stop the Rust client from authoring defaults

In `crates/client/src/agent_os.rs`, retain the typed serialized config long
enough to compare it with its true default:

```rust
let create_vm_config = serialize_create_vm_config_for_sidecar(&config)?;
let create_vm_config = (create_vm_config != vm_config::CreateVmConfig::default())
    .then(|| {
        serde_json::to_string(&create_vm_config).map_err(|error| {
            ClientError::Sidecar(format!(
                "failed to serialize create VM config: {error}"
            ))
        })
    })
    .transpose()?;
```

Then construct:

```rust
wire::InitializeVmRequest {
    runtime: None,
    config: create_vm_config,
    // existing presence-aware mounts/packages/callbacks
}
```

Typed equality is preferable to checking whether a JSON string equals `"{}"`.
It preserves explicit default-valued presence represented by `Some(...)`, such
as empty loopback ports, explicit root configuration, or explicit nested
runtime configuration. Existing Item 46 tests for explicit presence must stay
green.

If borrow/move ergonomics make the inline block noisy, extract a private
`serialize_initialize_vm_config` returning `Result<Option<String>, ClientError>`
beside `serialize_create_vm_config_for_sidecar`; do not introduce a new public
client abstraction.

## Tests to add or update

### Before-change characterization

Add these tests before changing production behavior and record their parent
result in the Item 93 tracker row:

1. **TypeScript high-level capture:** extend
   `packages/core/tests/overlay-sidecar-resolution.test.ts` (its fake spawned
   `SidecarProcess` already records `initializeVm`) or add a focused
   `initialize-vm-omission.test.ts`. Create `AgentOs` with
   `defaultSoftware: false` and otherwise omitted VM options. Against the
   parent, assert the spy receives `runtime: "java_script"` and `config: {}`.
2. **Rust request builder:** add a private focused builder/test in
   `crates/client/src/agent_os.rs` around the `InitializeVmRequest` construction.
   Against the parent, the default config produces JavaScript and `"{}"`.
   This avoids requiring a real sidecar merely to inspect serialization.

The after-change expectations for the same tests become absent runtime/config.
Add explicit cases in both clients proving a present nested config remains
present and unchanged. Rust has no public runtime override, so only the shared
normalizer and protocol round-trip tests need explicit non-JavaScript runtime
coverage.

### Protocol tests

Update `packages/runtime-core/tests/request-payloads.test.ts`:

- `{ type: "initialize_vm" }` maps to generated `runtime: null`, `config: null`;
- explicit JavaScript plus `{}` maps to JavaScript plus `"{}"`; and
- explicit non-empty config survives JSON serialization unchanged.

Update `packages/runtime-core/tests/sidecar-process.test.ts` so
`initializeVm(session, {})` records an `initialize_vm` payload with neither
field. Add an explicit call retaining both values.

Add a BARE round-trip test in `crates/sidecar-protocol/src/wire.rs` for an
`InitializeVmRequest` with both `None`, then the same frame with explicit
runtime/config. Assert generated -> compat -> generated equality as well as
codec decode equality. This catches either generated endpoint losing presence.

### Authoritative sidecar tests

Update `crates/native-sidecar/tests/initialize_vm.rs` helpers to accept optional
runtime/config, then add focused tests:

- omission initializes successfully and returns the ordinary default resolved
  VM view;
- explicit config (for example a raised process retention limit) still changes
  the resolved response; and
- the existing atomic rollback/host-callback behavior remains unchanged.

Update
`crates/native-sidecar-browser/tests/wire_dispatch.rs::browser_wire_dispatcher_initializes_vm_atomically`
to send both values as `None`. Keep/add an explicit-config case proving browser
config still reaches `create_vm`. The pure shared-core tests own exact runtime
selection parity because the browser execution shell does not need to invent a
second runtime-default assertion.

Do not add default selection tests to the client suites after the change. The
client tests should assert absence; sidecar/core tests should assert the
resolved JavaScript/empty-config behavior.

## Validation commands

### Focused before/after commands

```sh
pnpm --dir packages/runtime-core exec vitest run \
  tests/request-payloads.test.ts tests/sidecar-process.test.ts
pnpm --dir packages/core exec vitest run \
  tests/initialize-vm-omission.test.ts
cargo test -p agentos-client create_vm_config_omits_client_owned_defaults
cargo test -p agentos-sidecar-protocol initialize_vm
cargo test -p agentos-native-sidecar-core initialize_vm
cargo test -p agentos-native-sidecar --test initialize_vm -- --nocapture
cargo test -p agentos-native-sidecar-browser \
  browser_wire_dispatcher_initializes_vm_atomically -- --nocapture
```

Use the actual Core test path if the capture case is added to
`overlay-sidecar-resolution.test.ts` instead of a new focused file.

### Generated, type, and workspace gates

```sh
pnpm --dir packages/build-tools build:protocol
node scripts/check-generated-artifacts.mjs
pnpm --dir packages/runtime-core check-types
pnpm --dir packages/runtime-core build
pnpm --dir packages/core check-types
pnpm --dir packages/core build
cargo fmt --all -- --check
cargo check -p agentos-sidecar-protocol
cargo check -p agentos-client
cargo check -p agentos-native-sidecar-core
cargo check -p agentos-native-sidecar
cargo check -p agentos-native-sidecar-browser
cargo check --workspace
git diff --check
```

Run the repository's known root JavaScript gates if their separately tracked
nested-worktree/npm-shim blockers are cleared; do not weaken those checks in
Item 93.

## Risks and non-goals

- **Presence collapse:** `??`, truthiness, JSON-string comparison, or
  unconditional `undefined` keys can turn explicit `{}`, `[]`, `false`, or an
  explicit default-shaped nested config into omission. Use `=== undefined`,
  conditional object construction, and Rust `Option`/typed equality.
- **Divergent defaults:** do not put one fallback in native and another in
  browser. Both must call the shared-core helper.
- **Over-broad protocol change:** do not change `CreateVmRequest`; do not make
  execute runtime defaults part of this item (those already belong to shared
  execute normalization).
- **Config-policy duplication:** the new helper must not deserialize or validate
  config. Existing native/browser `create_vm` paths remain authoritative.
- **Generated drift:** schema and committed TypeScript output must be generated
  together; Rust output under `target` is never committed.
- **No package-manager exception involved:** this item does not touch
  TypeScript's allowed default package list.

## Completion checklist for the tracker

- [ ] Parent TypeScript and Rust characterization records show JavaScript plus
      `{}` were client-authored.
- [ ] BARE schema and generated TypeScript preserve optional runtime/config.
- [ ] TypeScript high-level and runtime-core requests omit absent values and
      preserve every explicit value.
- [ ] Rust sends `None`/`None` for default AgentOS creation and preserves an
      explicit non-default config.
- [ ] Shared-core tests prove the one JavaScript/`{}` normalization contract.
- [ ] Native and browser atomic initialization tests consume the shared helper.
- [ ] Client suites contain only omission/forwarding tests for this behavior;
      default behavior is tested in shared/sidecar suites.
- [ ] Focused, generated, type/build, formatting, workspace, and diff gates pass.
- [ ] Independent review finds no P0-P2 issue.
- [ ] Item 93 is isolated in its own stacked `jj` revision and its tracker row
      is marked `done` with the exact revision ID and validation evidence.
