# Execution Retention and Output

Status: superseded by `execution-api-redesign.md`

Audience: AgentOS client, actor, sidecar, protocol, JavaScript, TypeScript,
Python, Rust, and documentation owners

This is a historical revision of `language-execution-api.md`. The current
context/process API is specified by `execution-api-redesign.md`.

## 1. Decision

The default attached execution is ephemeral. Persistent execution identity,
language state, completed results, and replayable output are opt-in.

- An attached operation without `executionId` is ephemeral.
- An operation with an explicit `executionId` is retained.
- A detached operation requires an explicit `executionId`.
- Completed execution records expire after a bounded, configurable VM-level
  TTL.
- Capturing stdout and stderr in a completed result is opt-in.
- Retaining ordered output events for later reads is separately opt-in.
- Live output delivery is independent from capture and event retention.
- Evaluation values and structured guest errors use dedicated protocol fields;
  they never depend on stdout or stderr capture.

The common path therefore creates no durable public execution record, retains
no interpreter after completion, stores no replay history, and captures no
stdio unless the caller requests it.

## 2. Terminology

**Ephemeral operation**

An attached operation submitted without `executionId`. It may use an internal
routing token while active, but it has no public execution identity and never
enters the retained-execution collection.

**Retained execution**

An execution identified by a caller-supplied `executionId`. It may retain
language state, its most recent structured result, and—when requested—ordered
output events. It remains addressable until explicit deletion, idle expiry, or
VM disposal.

**Capture**

Aggregation of stdout and/or stderr into the result returned by an attached
operation or `executions.wait`.

**Live delivery**

Delivery of output chunks while an operation runs. Core uses per-call callbacks
or execution subscriptions. Actor clients use execution events.

**Event retention**

Storage of ordered output chunks for later pagination with
`executions.readOutput`. This is sometimes described as replay, but the public
configuration name is `retainEvents`.

## 3. Behavior matrix

| Request | Public record | Language state | Capture | Retained events |
| --- | --- | --- | --- | --- |
| Attached, no ID | No | Released on completion | Opt-in | Invalid |
| Attached, explicit ID | Yes | Retained when supported | Opt-in | Opt-in |
| Detached, no ID | Invalid | N/A | N/A | N/A |
| Detached, explicit ID | Yes | Retained when supported | Opt-in | Opt-in |

Supplying `createIfMissing` without `executionId` remains invalid. An explicit
ID that does not exist requires `createIfMissing: true`; omission continues to
mean `false`.

## 4. Public API

### 4.1 VM limits

Execution retention is VM policy:

```ts
const vm = await AgentOs.create({
  limits: {
    execution: {
      completedTtlMs: 5 * 60_000,
      maxCompletedExecutions: 1_024,
      liveExecutionWarningThreshold: 64,
    },
  },
});
```

Public configuration:

```ts
interface ExecutionLimits {
  /**
   * Time from terminal transition until a completed execution expires.
   * Default: 300_000 (five minutes).
   */
  completedTtlMs?: number;

  /**
   * Maximum completed execution records retained in one VM.
   * Default: 1_024.
   */
  maxCompletedExecutions?: number;

  /**
   * Warn when concurrently running executions reach this count.
   * This warning does not terminate running work.
   * Default: 64.
   */
  liveExecutionWarningThreshold?: number;
}
```

All values must be positive safe integers. There is no public infinite-TTL
sentinel. VM disposal always releases all executions regardless of TTL.

When completion would exceed `maxCompletedExecutions`, the sidecar removes the
oldest eligible completed records until the collection is within the bound.
It never evicts a running execution. The sidecar emits a rate-limited warning
when live execution count reaches `liveExecutionWarningThreshold`; existing
process, memory, CPU, and VM resource limits remain the hard enforcement for
running work. Configuration validation errors name the exact invalid field.

Existing runtime output byte limits continue to bound captured stdio.
Existing protocol event count and byte limits continue to bound retained event
history. This change does not introduce per-operation options that can raise VM
policy; a later per-operation byte setting may only lower the applicable VM
limit.

### 4.2 Output options

```ts
type OutputCapture = "none" | "stderr" | "all";

interface ExecutionOutputOptions {
  /**
   * Include aggregated output in the completed result.
   * Default: "none".
   */
  capture?: OutputCapture;

  /**
   * Retain ordered output events for executions.readOutput().
   * Requires an explicit executionId.
   * Default: false.
   */
  retainEvents?: boolean;
}

interface LanguageExecutionOptions {
  executionId?: string;
  createIfMissing?: boolean;
  detached?: boolean;
  output?: ExecutionOutputOptions;

  // Existing cwd, env, args, stdin, timeout, PTY, signal, and callback fields
  // remain unchanged.
}
```

`retainEvents: true` without `executionId` is a validation error.

Live callbacks do not imply capture or event retention:

```ts
await vm.javascript.execute(source, {
  onStdout(chunk) {
    process.stdout.write(chunk);
  },
  output: {
    capture: "none",
    retainEvents: false,
  },
});
```

When capture and event retention are disabled and there is no live consumer,
the sidecar drains and discards guest output at the earliest bounded transport
layer. It must not construct execution events, append result buffers, or copy
discarded bytes into actor/session queues.

### 4.3 Ephemeral attached calls

```ts
const result = await vm.javascript.execute(`console.log("hello")`);
```

The result has no `executionId` or `generation`. Stdout and stderr are absent
because capture defaults to `none`.

```ts
const result = await vm.javascript.execute(`console.log("hello")`, {
  output: { capture: "all" },
});

console.log(result.stdout);
```

The sidecar releases all internal operation state after the completed response
has been committed to its registered response waiter. Cleanup also occurs on
validation-after-admission failure, launch failure, cancellation, timeout,
transport disconnect, and VM shutdown.

An ephemeral inline JavaScript, TypeScript, or Python call does not preserve its
interpreter after completion.

### 4.4 Retained calls

```ts
await vm.javascript.execute(`globalThis.answer = 40`, {
  executionId: "analysis",
  createIfMissing: true,
});

const result = await vm.javascript.evaluate(`answer + 2`, {
  executionId: "analysis",
});
```

Supplying an explicit ID opts into the retained execution lifecycle and, for
supported inline language operations, retained language state. It does not
implicitly opt into stdio capture or retained events.

Callers may delete retained state before its TTL:

```ts
await vm.executions.delete("analysis");
```

### 4.5 Detached calls

Detached calls require explicit ownership:

```ts
const execution = await vm.javascript.execute(serverSource, {
  executionId: "server",
  createIfMissing: true,
  detached: true,
  output: {
    retainEvents: true,
  },
});
```

This is invalid:

```ts
await vm.javascript.execute(serverSource, {
  detached: true,
});
```

Core, actor, TypeScript, and Rust clients reject this before admission. The
sidecar repeats the validation as the enforcement point and returns
`InvalidExecutionIdentityError` if a client bypasses local validation.

### 4.6 Result types

Ephemeral and retained results are distinct:

```ts
interface CodeExecutionResultBase {
  detached: false;
  outcome: ExecutionOutcome;
  exitCode?: number;
  error?: ExecutionErrorData;

  // Present only for the channels selected by output.capture.
  stdout?: string;
  stderr?: string;
  stdoutTruncated?: boolean;
  stderrTruncated?: boolean;
}

interface CodeExecutionResult extends CodeExecutionResultBase {
  executionId?: never;
  generation?: never;
}

interface RetainedCodeExecutionResult extends CodeExecutionResultBase {
  executionId: string;
  generation: number;
}
```

Uncaptured output is represented by absent fields, not empty strings. Empty
strings mean that the channel was captured and produced no bytes.

Representative overloads:

```ts
execute(
  source: string,
  options?: JavaScriptExecutionOptions & {
    executionId?: never;
    createIfMissing?: never;
    detached?: false;
  },
): Promise<CodeExecutionResult>;

execute(
  source: string,
  options: JavaScriptExecutionOptions & {
    executionId: string;
    detached?: false;
  },
): Promise<RetainedCodeExecutionResult>;

execute(
  source: string,
  options: JavaScriptExecutionOptions & {
    executionId: string;
    detached: true;
  },
): Promise<DetachedExecution>;
```

The same distinction applies to JavaScript, TypeScript, Python, package
workflows, `process.exec`, and `process.execFile`.

### 4.7 Evaluation values and structured errors

Evaluation values use a dedicated protocol result field:

```ts
const result = await vm.javascript.evaluate("21 * 2");
// result.value === 42
```

They must not be encoded as markers in stdout or extracted from captured
output. Disabling output capture therefore has no effect on evaluation.

Guest exceptions and sidecar enforcement failures also use structured result
fields. Stderr may contain additional program diagnostics when captured, but it
is never the only representation of an execution error.

## 5. Live and retained output APIs

### 5.1 Live Core delivery

Per-call callbacks work for ephemeral and retained operations:

```ts
await vm.javascript.execute(source, {
  onStdout(chunk) {
    process.stdout.write(chunk);
  },
});
```

Execution subscriptions require a retained ID:

```ts
const unsubscribe = vm.onExecutionOutput("server", (event) => {
  if (event.channel === "stderr") process.stderr.write(event.chunk);
  else process.stdout.write(event.chunk);
});
```

Actor live output events require an explicit retained ID. An ephemeral actor
action can return captured output, but it has no public ID on which to route
reconnectable live events.

### 5.2 Retained event reads

`retainEvents: true` stores ordered, bounded events:

```ts
interface ExecutionOutputEvent<TChunk = Uint8Array> {
  executionId: string;
  generation: number;
  processId?: string;
  sequence: number;
  channel: "stdout" | "stderr" | "pty";
  chunk: TChunk;
  timestampMs: number;
}
```

Callers page them with:

```ts
const page = await vm.executions.readOutput("server", {
  cursor,
  limit: 100,
});
```

```ts
interface ExecutionOutputPage<TChunk = Uint8Array> {
  executionId: string;
  generation: number;
  events: ExecutionOutputEvent<TChunk>[];
  nextCursor: string;
  hasMore: boolean;
  truncated: boolean;
}
```

If `retainEvents` was false, `readOutput` returns an empty page with
`truncated: false`; it does not pretend that output was retained and lost.

Starting a new operation on the same execution increments its generation and
invalidates older cursors. Bounded eviction sets `truncated: true`. Expiry or
deletion makes the execution unavailable and returns
`ExecutionNotFoundError`.

Reading or subscribing to output does not extend the idle TTL.

## 6. Idle TTL

### 6.1 Start and reset rules

The TTL measures time since the most recent operation became terminal. It
applies only to completed/idle records.

- Admission cancels any scheduled expiry for that execution.
- Success, guest failure, cancellation, timeout, and reset schedule expiry.
- A sidecar cleanup/enforcement failure that leaves the execution in `failed`
  state also schedules expiry.
- Inspection, `wait`, `readOutput`, and event subscription do not extend TTL.
- Running executions never expire or participate in completed-record eviction.
- Explicit deletion and VM disposal remove state immediately.

Reset clears retained state and output before scheduling a new idle TTL.

### 6.2 Expiry cleanup

Expiry removes all execution-owned resources:

- descriptor and retained result;
- captured stdout and stderr;
- retained output events and cursors;
- V8 or Python resident context;
- execution-to-process routing entries;
- process group and resident process bookkeeping;
- deadline/expiry registrations;
- package-mutation ownership, if stale;
- generation-scoped callbacks and waiters.

Expiry must not leave a guest process or host task running. Cleanup failures
must be logged with the VM and execution IDs and surfaced through structured
tracing; they must not be silently swallowed.

### 6.3 Scheduling architecture

Use one VM-level indexed deadline queue serviced by the existing sidecar
reactor. Do not create one Tokio task, operating-system thread, or recurring
poll timer per retained execution.

The queue contains at most one expiry entry per retained execution. Admission
removes that entry; a terminal transition inserts its replacement. Each entry
contains the execution ID, expiry time, and an expiry generation/token so an
entry already selected for cleanup cannot delete a newer generation. Do not
use an unbounded lazy heap whose stale entries accumulate every time an
execution is reused.

Expiry work is bounded per reactor turn. If many executions expire at once,
the sidecar coalesces the wake and drains cleanup fairly without blocking other
VMs.

## 7. Protocol changes

The protocol distinguishes ephemeral operations from retained execution
identity:

```bare
type ExecutionIdentityOptions struct {
  executionId: optional<str>
  createIfMissing: optional<bool>
}

type ExecutionOutputOptions struct {
  capture: optional<ExecutionOutputCapture>
  retainEvents: optional<bool>
}
```

Omitted `executionId` means ephemeral attached execution. It no longer means
"generate a reusable public ID."

The accepted response for an ephemeral attached operation uses an internal
request correlation identity, not an `ExecutionDescriptor`. Its terminal
response is routed directly to the registered call waiter. The implementation
must not make a synchronous waiter scan or consume unrelated events.

Retained and detached operations continue to return an
`ExecutionDescriptor`. Detached admission without an ID is rejected.

Completed responses carry evaluation values and TypeScript check results
directly. No control value is tunneled through stdout or stderr.

Client, sidecar, actor, and Rust protocol changes ship in same-version
lockstep; no compatibility path is required.

## 8. Sidecar state model

The sidecar keeps active ephemeral operations separate from retained public
executions:

```text
active ephemeral operation
  internal request token
  active process/context
  optional transient capture buffers
  optional live callbacks

retained execution
  public executionId + generation
  descriptor and last structured result
  optional resident language context
  optional capture buffers for the active/latest operation
  optional retained event history
  idle expiry token
```

Ephemeral state is removed after terminal response commitment. While running,
it contributes to the live execution warning count, but it never enters the
completed-record collection. Existing active process, queue, memory, and output
limits remain its hard bounds.

The sidecar avoids output work that was not requested:

1. Always drain guest file descriptors so the guest cannot block.
2. Encode a live event only when there is a live consumer or retained-event
   policy.
3. Append to a capture buffer only for selected channels.
4. Append to replay history only when `retainEvents` is true.
5. Serialize captured strings only in completed responses that requested them.

## 9. Actor and client parity

- TypeScript Core, Rust, and actor actions expose the same lifecycle choices.
- Actor-safe output options contain no callbacks or `AbortSignal`.
- Actor live execution events require an explicit execution ID.
- Ephemeral actor actions may opt into completed-result capture.
- Rust represents ephemeral and retained results as distinct enum variants or
  types rather than populating a meaningless empty ID.
- Clients validate for fast feedback; the sidecar owns final validation,
  defaults, TTL, cleanup, and enforcement.
- Clients do not implement TTL timers or delete ephemeral executions.

## 10. Errors

Required typed errors include:

- `InvalidExecutionIdentityError`: detached operation without `executionId`,
  `createIfMissing` without an ID, or `retainEvents` without an ID.
- `ExecutionNotFoundError`: explicit ID absent or expired.
- `ExecutionBusyError`: retained ID already has an active operation.
- `ExecutionLanguageConflictError`: retained ID belongs to another language.
- `ExecutionOutputCursorExpiredError`: cursor belongs to an older generation or
  evicted event range.

Errors caused by limits name the limit and the configuration path used to
raise it.

## 11. Validation

### 11.1 Lifecycle

- Thousands of sequential attached no-ID calls leave retained count at zero.
- Ephemeral completion releases JavaScript and Python contexts.
- Ephemeral launch failure, cancellation, timeout, and disconnect clean up.
- Detached calls without an ID are rejected by clients and sidecar.
- Explicit IDs retain supported language state across operations.
- Manual deletion releases all retained resources.
- VM disposal releases active and idle execution resources.

### 11.2 TTL

- Idle retained execution expires at the configured deadline.
- Reuse before expiry invalidates the old expiry entry.
- Reset clears state and schedules a fresh deadline.
- Running executions do not expire.
- `get`, `list`, `wait`, `readOutput`, and subscriptions do not extend TTL.
- Expiry clears resident runtimes, results, buffers, events, and routing maps.
- Many simultaneous expirations are drained with bounded fair work.
- Stale expiry entries cannot delete a reused execution.
- Reusing one retained execution repeatedly does not grow the expiry queue.

### 11.3 Output

- Default execution retains and captures no stdout/stderr.
- `capture: "stderr"` captures only stderr.
- `capture: "all"` captures both channels with correct truncation flags.
- Live callbacks work without capture or retained events.
- No-consumer output is discarded without event construction or buffer growth.
- `retainEvents` preserves byte-exact order and cursor pagination.
- `readOutput` is empty when events were not retained.
- PTY output uses the merged `pty` channel.
- Evaluation succeeds with capture disabled.
- Structured guest errors remain available with stderr capture disabled.

### 11.4 Limits and observability

- Config binding generation covers both execution limit fields.
- Completed count never exceeds
  `limits.execution.maxCompletedExecutions`; oldest eligible records are
  removed first.
- Live concurrency warning is rate-limited and names
  `limits.execution.liveExecutionWarningThreshold`.
- TTL, count, and warning values reject zero, negative, fractional, and unsafe
  values.
- Cleanup failures reach logs/traces with VM and execution identity.

## 12. Documentation changes

Update language, process, actor, limits, and lifecycle documentation to state:

- no ID means ephemeral;
- detached requires an explicit ID;
- explicit ID opts into retained lifecycle and language state;
- retained idle executions expire after the VM TTL;
- capture, live delivery, and retained events are independent;
- output capture defaults to none;
- `retainEvents` is required for later `readOutput`;
- evaluation values do not require stdout capture;
- explicit deletion remains available for prompt cleanup.

Examples should use the ephemeral default unless they specifically demonstrate
state reuse, detachment, event replay, or lifecycle management.

## 13. Resolved decisions

- Public generated execution IDs are removed from attached no-ID calls.
- Detached operations never generate an ID implicitly.
- Explicit IDs opt into execution/language retention, not output retention.
- Completed-record TTL is mandatory and configurable only at VM policy level.
- Default TTL is five minutes.
- Stdio capture defaults to none.
- Event retention defaults to false and is named `retainEvents`.
- Live delivery does not imply capture or retention.
- Evaluation/control values use dedicated protocol fields.
- Reads and subscriptions do not refresh idle TTL.
- No backward-compatibility layer is required.

## 14. Review checklist

Reviewers should specifically challenge:

- whether five minutes is the correct default TTL;
- whether `capture: "none"` is acceptable developer experience for failures;
- whether actor callers need ephemeral live-output correlation;
- whether existing event byte/count limits are correctly scoped per execution;
- whether terminal response commitment and ephemeral cleanup can race;
- whether live output is guaranteed to be queued before terminal completion;
- whether retained interpreter teardown is complete for V8 and Python;
- whether TTL cleanup can terminate work due to stale generation state;
- whether `maxCompletedExecutions` and the live warning threshold have suitable
  defaults;
- whether package operations need different capture defaults;
