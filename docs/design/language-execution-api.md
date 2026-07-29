# Language Execution API

Status: superseded by `execution-api-redesign.md`

Audience: AgentOS client, actor, sidecar, JavaScript, TypeScript, Python, and
documentation owners

> Historical design only. The current context/process API is specified by
> `execution-api-redesign.md`.

## 1. Decision

AgentOS will expose first-class JavaScript, TypeScript, and Python execution on
both the core `AgentOs` client and the `@rivet-dev/agentos` actor. These are
language-native conveniences over the existing VM, filesystem, process, and
package-manager capabilities. They are not a separate product or runtime.

**Execution** is the only public lifecycle noun. An execution is identified by
`executionId`, can receive multiple sequential operations, and can retain
JavaScript or Python in-memory state between those operations. There is no
second public resource or identifier for retained language state. There is no
standalone `createExecution` method; the first Bash or language operation
creates the execution atomically when requested.

Arbitrary commands use `process.exec` for configured-shell command strings and
`process.execFile` for exact argv invocation. The existing managed-process
surface is `process.spawn`. We will not add `executeBash` or another
shell-specific abstraction.

Core and actor actions use the same nested shape. RivetKit serializes nested
actor actions with dotted wire names such as `javascript.execute`,
`javascript.typescript.check`, and `python.execute`. Public TypeScript
identifiers use the canonical `TypeScript` capitalization in type names.

The API must cover the normal language ecosystem without forcing users to know
how AgentOS invokes `node`, `python`, `python -m`, `npm`, or `pip`. It must not
mirror every package-manager subcommand or CLI flag. Generic command APIs
remain the escape hatch for unusual workflows.

## 2. Required repository guidance

The implementation change must add a short scope rule to applicable
`CLAUDE.md` files. At minimum it belongs in `packages/CLAUDE.md`.

The guidance must preserve this meaning:

> Language-specific modules own the common end-to-end workflows of their
> ecosystem. A user working purely in JavaScript, TypeScript, or Python should
> be able to execute source and files, evaluate values, install dependencies,
> run project entry points, and use standard module/script workflows without
> manually invoking `node`, `python`, `python -m`, `npm`, or `pip`. Add typed,
> injection-safe conveniences for stable workflows, not one method per CLI
> subcommand or flag. Keep `process.exec`, `process.execFile`, and
> `process.spawn` as the explicit escape hatch for uncommon commands.

A helper belongs in a language module when it represents a common
language-level intent and can provide safer or clearer behavior than assembling
a command. It does not belong merely because a package-manager flag exists.

## 3. Goals and non-goals

### Goals

- Make inline code, existing files, Python modules, npm scripts, and dependency
  installation available without direct runtime binaries.
- Use one execution lifecycle across Bash, argv, JavaScript, TypeScript,
  Python, and package-manager operations.
- Retain language state across operations when callers reuse an execution ID.
- Keep JavaScript, TypeScript, and Python results structurally consistent.
- Make TypeScript execution transpile-only by default while providing explicit
  type-check APIs.
- Use argv-based internal invocation for user-supplied package names, scripts,
  modules, paths, and arguments.
- Keep core, actor, TypeScript, and Rust behavior identical.
- Keep actor inputs, events, results, and errors serializable.
- Preserve `process.exec`, `process.execFile`, and `process.spawn` for
  arbitrary Linux workflows.

### Non-goals

- A method for every npm, pip, Node.js, Python, or TypeScript CLI operation.
- A generic package-manager framework or immediate pnpm, Yarn, Bun, uv, or
  Poetry parity.
- Live JavaScript handles, Python proxy objects, or isolate-owned values
  crossing the actor boundary.
- Live notebook display handles or guest-owned display objects.
- A new Bash API.
- Client-side language behavior or command construction.

## 4. Protocol-first execution model

An execution is a sidecar-owned unit identified by `executionId`.
Shell, argv, JavaScript, TypeScript, Python, and package-manager operations are
submitted to an execution. An execution admits at most one active operation;
another submission while it is active throws `ExecutionBusyError`. Different
executions may run concurrently.

An execution may retain JavaScript or Python variables, imports, functions,
loaded modules, and guest objects between operations. It may also own a root OS
process or process tree while an operation is active. `processId` and `pid` are
observational details; `executionId` is the only public lifecycle identity.

The shared lifecycle is defined in Bare and implemented in the sidecar. The
TypeScript, Rust, and actor clients validate and serialize explicit caller
input, route events, and expose generated results. They must not translate
language methods into `node`, `python`, `npm`, `pip`, or shell commands
themselves.

### 4.1 Shared Bare shape

The fragments below show the required factoring, not the complete generated
schema. The implementation must define request and response variants for every
semantic operation listed in sections 5 and 7. Exact generated names may
change, but the schema must preserve this shape:

```bare
type ExecutionState enum {
  CREATING
  IDLE
  RUNNING
  RESETTING
  DELETING
  FAILED
}

type ExecutionOutcome enum {
  SUCCEEDED
  FAILED
  CANCELLED
  TIMED_OUT
}

type RetainedExecutionLanguage enum {
  JAVA_SCRIPT
  PYTHON
}

type ExecutionStreamChannel enum {
  STDOUT
  STDERR
  PTY
}

type JavaScriptModuleFormat enum {
  MODULE
  COMMON_JS
}

type ExecutionIdentityOptions struct {
  executionId: optional<str>
  createIfMissing: optional<bool>
}

type PtyOptions struct {
  cols: optional<u16>
  rows: optional<u16>
}

type ProcessExecutionOptions struct {
  identity: ExecutionIdentityOptions
  detached: optional<bool>
  cwd: optional<str>
  env: optional<map<str><str>>
  args: list<str>
  stdin: optional<data>
  timeoutMs: optional<u64>
  pty: optional<PtyOptions>
}

type ShellExecutionRequest struct {
  process: ProcessExecutionOptions
  command: str
}

type ArgvExecutionRequest struct {
  process: ProcessExecutionOptions
  command: str
}

type JavaScriptExecutionRequest struct {
  process: ProcessExecutionOptions
  source: str
  format: optional<JavaScriptModuleFormat>
  filePath: optional<str>
  inputs: optional<JsonUtf8>
}

type TypeScriptExecutionRequest struct {
  process: ProcessExecutionOptions
  source: str
  filePath: optional<str>
  tsconfigPath: optional<str>
  compilerOptions: optional<JsonUtf8>
  inputs: optional<JsonUtf8>
}

type PythonExecutionRequest struct {
  process: ProcessExecutionOptions
  source: str
  inputs: optional<JsonUtf8>
}
```

File, Python-module, npm-script, npm-package, package-install, evaluation, and
type-check requests use the same identity and add only their stable
operation-specific fields. Each distinct semantic operation has a first-class
Bare request. Clients do not implement semantic language conveniences by
calling `process.execFile`.

Common lifecycle and output are also protocol-owned:

```bare
type ExecutionDescriptor struct {
  executionId: str
  generation: u64
  state: ExecutionState
  retainedLanguage: optional<RetainedExecutionLanguage>
  processId: optional<str>
  pid: optional<u32>
  createdAtMs: u64
  lastStartedAtMs: optional<u64>
  lastCompletedAtMs: optional<u64>
  lastOutcome: optional<ExecutionOutcome>
  lastExitCode: optional<i32>
}

type ExecutionErrorData struct {
  code: str
  name: str
  message: str
  stack: optional<str>
  details: optional<JsonUtf8>
}

type ExecutionCompletedResponse struct {
  execution: ExecutionDescriptor
  outcome: ExecutionOutcome
  exitCode: optional<i32>
  error: optional<ExecutionErrorData>
  stdout: data
  stderr: data
  stdoutTruncated: bool
  stderrTruncated: bool
  evaluationValue: optional<JsonUtf8>
  typeScriptCheckResult: optional<JsonUtf8>
}

type ExecutionOutputEvent struct {
  executionId: str
  generation: u64
  processId: optional<str>
  sequence: u64
  channel: ExecutionStreamChannel
  chunk: data
  timestampMs: u64
}

type ExecutionCompletedEvent struct {
  executionId: str
  generation: u64
  outcome: ExecutionOutcome
  exitCode: optional<i32>
  error: optional<ExecutionErrorData>
}

type GetExecutionRequest struct {
  executionId: str
}
type ListExecutionsRequest void
type WaitExecutionRequest struct {
  executionId: str
}
type CancelExecutionRequest struct {
  executionId: str
}
type SignalExecutionRequest struct {
  executionId: str
  signal: str
}
type ResetExecutionRequest struct {
  executionId: str
}
type DeleteExecutionRequest struct {
  executionId: str
}
type WriteExecutionStdinRequest struct {
  executionId: str
  chunk: data
}
type CloseExecutionStdinRequest struct {
  executionId: str
}
type ResizeExecutionPtyRequest struct {
  executionId: str
  cols: u16
  rows: u16
}
type ReadExecutionOutputRequest struct {
  executionId: str
  cursor: optional<str>
  limit: optional<u32>
}
```

A detached operation returns `ExecutionDescriptor` after sidecar admission.
`executions.wait` waits for the execution's sole active operation. When the
execution is already idle or failed, it returns the most recently retained
result or throws `ExecutionResultNotFoundError` if no operation has completed
since creation or reset.

The sidecar internally orders output for the current or most recently completed
operation by monotonically increasing sequence number. Admitting a new
operation increments the execution's observational `generation`, replaces the
previous retained result and output buffer, and invalidates its cursors; it does
not clear retained language memory. `executions.readOutput` exposes only an opaque
cursor for bounded pagination and replay after actor reconnects. Resetting also
increments `generation`, clears retained output and language state, and
invalidates old cursors. `generation` is a revision marker, not a second
lifecycle identity.

The current `ExecuteRequest`, PID-only output events, and client-maintained
process records are migration inputs, not the target architecture. The
protocol has no backwards-compatibility requirement, so implementation should
replace them in lockstep rather than add a compatibility path. Optional Bare
fields mean "use the sidecar default"; clients must not duplicate defaults.

### 4.2 Identity and atomic create-if-missing

When `executionId` is omitted, the sidecar generates a collision-resistant ID
and creates a fresh execution. When it is supplied, `createIfMissing` defaults
to `false`:

- if the ID is absent, the sidecar atomically creates it and submits the
  operation only when `createIfMissing` is `true`;
- if it exists and is idle, the sidecar submits the operation to that execution;
- if it exists and is active, the sidecar throws `ExecutionBusyError`;
- if it is absent and creation was not requested, the sidecar throws
  `ExecutionNotFoundError`;
- concurrent requests for the same absent ID create exactly one execution;
- incompatible retained-language reuse throws `ExecutionLanguageConflictError`.

JavaScript and TypeScript share one retained JavaScript realm. Python uses a
retained Python interpreter. Process-only operations do not pin a language and
may run before or between language operations. One execution cannot retain
JavaScript and Python state simultaneously.

Execution IDs are scoped to their owning VM. The same caller-supplied string in
two VMs identifies two independent executions. Supplying
`createIfMissing` without `executionId` is invalid; omission of the ID already
means "create with a generated ID."

`executionId` is not an idempotency key for every subsequent operation. The
protocol and actor transport retain internal request identities for retry
deduplication, but those identities are not part of the end-user API. Atomic
creation/admission and busy checking are one sidecar operation; client-side
`executions.get` followed by a language operation is prohibited.

### 4.3 Public shared types

The names below are illustrative public TypeScript definitions. Final code may
factor them differently, but must preserve their behavior.

```ts
type JsonPrimitive = string | number | boolean | null;
type JsonValue =
  | JsonPrimitive
  | JsonValue[]
  | { [key: string]: JsonValue };

type ExecutionSignal =
  | "SIGHUP"
  | "SIGINT"
  | "SIGQUIT"
  | "SIGTERM"
  | "SIGKILL"
  | "SIGSTOP"
  | "SIGCONT"
  | "SIGUSR1"
  | "SIGUSR2";

type ActorData =
  | { encoding: "utf8"; data: string }
  | { encoding: "base64"; data: string };

interface LanguageExecutionOptions {
  executionId?: string;
  createIfMissing?: boolean;
  cwd?: string;
  env?: Record<string, string>;
  args?: string[];
  stdin?: string | Uint8Array;
  /** Optional wall-clock deadline. Omitted means no caller deadline. */
  timeoutMs?: number;
  detached?: boolean;
  pty?: { cols?: number; rows?: number };

  // Core client only.
  signal?: AbortSignal;
  onStdout?: (chunk: Uint8Array) => void;
  onStderr?: (chunk: Uint8Array) => void;
}

interface InlineExecutionOptions extends LanguageExecutionOptions {
  inputs?: Record<string, JsonValue>;
}

type AttachedInlineExecutionOptions = Omit<
  InlineExecutionOptions,
  "detached"
>;

interface ActorLanguageExecutionOptions
  extends Omit<
    LanguageExecutionOptions,
    "stdin" | "signal" | "onStdout" | "onStderr"
  > {
  stdin?: ActorData;
}

interface ActorInlineExecutionOptions
  extends ActorLanguageExecutionOptions {
  inputs?: Record<string, JsonValue>;
}

type ExecutionState =
  | "creating"
  | "idle"
  | "running"
  | "resetting"
  | "deleting"
  | "failed";

type ExecutionOutcome =
  | "succeeded"
  | "failed"
  | "cancelled"
  | "timed_out";

interface ExecutionDescriptor {
  executionId: string;
  generation: number;
  state: ExecutionState;
  retainedLanguage?: "javascript" | "python";
  processId?: string;
  pid?: number;
  createdAtMs: number;
  lastStartedAtMs?: number;
  lastCompletedAtMs?: number;
  lastOutcome?: ExecutionOutcome;
  lastExitCode?: number;
}

interface DetachedExecution extends ExecutionDescriptor {
  detached: true;
}

interface ExecutionErrorData {
  code: string;
  name: string;
  message: string;
  stack?: string;
  details?: JsonValue;
}

interface CodeExecutionResultBase {
  executionId: string;
  generation: number;
  detached: false;
  exitCode?: number;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
}

type CodeExecutionResult =
  | (CodeExecutionResultBase & {
      outcome: "succeeded";
      error?: never;
    })
  | (CodeExecutionResultBase & {
      outcome: "failed" | "cancelled" | "timed_out";
      error: ExecutionErrorData;
    });

type CodeEvaluationResult<T = JsonValue> =
  | (CodeExecutionResult & {
      outcome: "succeeded";
      value: T;
    })
  | (CodeExecutionResult & {
      outcome: "failed" | "cancelled" | "timed_out";
      value?: never;
    });

interface ExecutionOutputEvent<TChunk = Uint8Array> {
  executionId: string;
  generation: number;
  processId?: string;
  sequence: number;
  channel: "stdout" | "stderr" | "pty";
  chunk: TChunk;
  timestampMs: number;
}

interface ExecutionOutputPage<TChunk = Uint8Array> {
  executionId: string;
  generation: number;
  events: ExecutionOutputEvent<TChunk>[];
  nextCursor: string;
  hasMore: boolean;
  truncated: boolean;
}

interface ExecutionCompletedEventBase {
  executionId: string;
  generation: number;
  exitCode?: number;
}

type ExecutionCompletedEvent =
  | (ExecutionCompletedEventBase & {
      outcome: "succeeded";
      error?: never;
    })
  | (ExecutionCompletedEventBase & {
      outcome: "failed" | "cancelled" | "timed_out";
      error: ExecutionErrorData;
    });

executions.get(executionId: string): Promise<ExecutionDescriptor>;
executions.list(): Promise<ExecutionDescriptor[]>;
executions.wait(executionId: string): Promise<CodeExecutionResult>;
executions.cancel(executionId: string): Promise<ExecutionDescriptor>;
executions.signal(
  executionId: string,
  signal: ExecutionSignal,
): Promise<ExecutionDescriptor>;
executions.reset(executionId: string): Promise<ExecutionDescriptor>;
executions.delete(executionId: string): Promise<void>;
executions.writeStdin(
  executionId: string,
  data: string | Uint8Array,
): Promise<void>;
executions.closeStdin(executionId: string): Promise<void>;
executions.resizePty(
  executionId: string,
  size: { cols: number; rows: number },
): Promise<void>;
executions.readOutput(
  executionId: string,
  options?: {
    cursor?: string;
    limit?: number;
  },
): Promise<ExecutionOutputPage>;

// Core client subscriptions. The actor broadcasts equivalent events.
onExecutionOutput(
  executionId: string,
  handler: (event: ExecutionOutputEvent) => void,
): () => void;
onExecutionCompleted(
  executionId: string,
  handler: (event: ExecutionCompletedEvent) => void,
): () => void;
```

`inputs` is exposed to guest code as a read-only top-level `inputs` object and
is serialized through the protocol rather than interpolated into source.
Evaluation values must be JSON-serializable; the generic type parameter is a
caller-side assertion, not a serializer.

`executions.cancel` performs bounded graceful cancellation followed by forced
termination. `executions.signal` sends an explicit POSIX signal to the active
execution-owned process group. `executions.delete` accepts only an idle or
failed execution; callers cancel and wait before deleting active work.

Only inline `javascript.execute`, `javascript.evaluate`,
`javascript.typescript.execute`, `javascript.typescript.evaluate`,
`python.execute`, and `python.evaluate` operations retain language memory.
JavaScript and TypeScript share one retained realm; Python uses a separate
interpreter and cannot share that execution. File, module, type-check,
arbitrary-command, npm, and Python-install operations use a fresh process and
do not mutate retained language memory. Every operation in a VM sees the same
filesystem, so files and installed packages remain visible across executions
until the VM filesystem is removed.

The `outcome` field is the sole result discriminator. A succeeded evaluation
has `value` and no `error`; failed, cancelled, and timed-out operations have a
serializable `error`. Validation, transport, and other failures before
admission throw typed host errors. Once admitted, an
operation always reaches a retained result and completion event, including
sidecar enforcement failures. Buffered `stdout` and `stderr` are UTF-8 strings;
invalid byte sequences use the Unicode replacement character, and their
truncation flags are always explicit. Evaluation values and TypeScript
diagnostics use their purpose-specific result fields. Output events retain
exact bytes as `Uint8Array` in core and tagged `ActorData` on the actor wire.
With a PTY, merged terminal bytes use the `pty` event channel and the
`onStdout`/`stdout` attached surface; `stderr` is empty.

For methods supporting detached operation, omitted or `detached: false`
returns `CodeExecutionResult`, while `detached: true` returns
`DetachedExecution`. No language-specific spawn twins are added. The existing
managed `process.spawn` API remains distinct and returns a PID.

Actor actions accept only actor-safe options and tagged `ActorData`. They do
not carry `AbortSignal`, callbacks, `Uint8Array`, guest proxies, or class
instances. Core callback subscriptions become actor `executionOutput` and
`executionCompleted` events. Actor output events and
`executions.readOutput` use `ExecutionOutputEvent<ActorData>` and
`ExecutionOutputPage<ActorData>` respectively; core uses their default
`Uint8Array` chunk type.

`timeoutMs` is an operation property shared by Bash and every language helper.
It starts when the sidecar admits the operation and covers staging,
transpilation or compilation, guest execution, and result collection. Omission
means no caller-configured deadline. Sidecar safety watchdogs and resource
limits remain bounded independently. Timeout cancellation, process-tree
termination, and cleanup are sidecar-owned; cleanup has its own bounded grace
period so a deadline cannot silently abandon resources.

`timeoutMs` is the only execution-specific limit option introduced by this
API. CPU, memory, process, filesystem, networking, output, and other resource
policies come from the owning VM's existing AgentOS core configuration. The
language helpers inherit and report those limits; they do not duplicate them
under language-specific names or add resource-usage result fields.

The core `AbortSignal` cancels only the operation submitted by that call. If it
fires before admission, the request is withdrawn; after admission it has the
same operation-level effect as `executions.cancel`. `executions.cancel` targets
the sole active operation and throws a typed state error when the execution is
idle. Cancelling or timing out an inline JavaScript, TypeScript, or Python
operation invalidates that execution's retained language memory before
returning it to idle. Its generation already identifies the admitted operation.
Cancelling a fresh-process operation leaves retained language memory unchanged.

Public state transitions are:

| From | Operation | To |
| --- | --- | --- |
| absent | admitted create and operation | `creating` → `running` |
| `idle` | admitted operation | `running` |
| `running` | success, guest failure, cancellation, or timeout | `idle` |
| `running` | sidecar enforcement or cleanup failure | `failed` |
| `idle` or `failed` | reset | `resetting` → `idle` |
| `idle` or `failed` | delete | `deleting` → absent |

Reset and delete reject an active execution. A failed execution accepts only
inspection, output replay, reset, or deletion.

## 5. Core API

All execution semantics are implemented once in the sidecar/runtime. Core and
actor clients only serialize input, route events, and expose typed results.

### 5.1 Arbitrary commands and managed processes

The process namespace owns configured-shell commands, exact argv execution, and
managed child processes; no Bash-specific method is added:

```ts
process.exec(
  command: string,
  options?: LanguageExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
process.exec(
  command: string,
  options: LanguageExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

process.execFile(
  command: string,
  args?: readonly string[],
  options?: Omit<LanguageExecutionOptions, "args"> & { detached?: false },
): Promise<CodeExecutionResult>;
process.execFile(
  command: string,
  args: readonly string[],
  options: Omit<LanguageExecutionOptions, "args"> & { detached: true },
): Promise<DetachedExecution>;

process.spawn(
  command: string,
  args?: readonly string[],
  options?: SpawnOptions,
): { pid: number };
```

`process.exec` retains configured-shell command-string semantics.
`process.execFile` is the injection-safe arbitrary-command API.
`process.spawn` retains the established managed-process contract: it returns a
PID and is controlled with the remaining `process.*` methods. It is not an
alias for a detached execution. No language-specific `spawn*` methods are
added.

### 5.2 Retained execution state

Reusing an execution ID retains supported language state:

```ts
await agentOs.javascript.execute("globalThis.answer = 40", {
  executionId: "analysis",
  createIfMissing: true,
});

const result = await agentOs.javascript.evaluate("answer + 2", {
  executionId: "analysis",
});
```

The second operation returns `42`. JavaScript and TypeScript reuse one realm;
Python reuses interpreter globals. A second operation cannot be submitted until
the first is terminal. `inputs` is replaced per operation and is not retained.

An operation without `executionId` creates a fresh execution with a generated
ID. The returned result includes that ID, so callers may reuse it later. Idle
lifetime, retained memory, output, and execution count are bounded. Expiration
or deletion releases retained resources and invalidates the ID.

| Methods | Retained language memory | Process behavior |
| --- | --- | --- |
| inline JS/TS execute and evaluate | Shared JavaScript realm | Runs in the execution-owned realm |
| inline Python execute and evaluate | Python interpreter globals | Runs in the execution-owned interpreter |
| JS/TS/Python file and Python module | None | Fresh process |
| Bash, argv, npm, installs, and type checks | None | Fresh process or sidecar compiler operation |

All rows share the VM filesystem. Per-operation `cwd`, `env`, and `args` do not
persist; guest changes to the process cwd or environment are restored after the
operation, and a shell `cd` or `export` affects only that shell operation.

Retained JavaScript is a cell/REPL contract, not merely reuse of the same V8
global object. Top-level `let`, `const`, `var`, function, class, and import
bindings created by a successful JavaScript or TypeScript operation are
available to later inline operations. The implementation must not accomplish
this by replaying earlier source and repeating side effects. A syntax or
transpilation failure does not mutate retained bindings; a guest exception may
leave mutations performed before the exception visible, matching interactive
interpreter behavior. Python follows the same partial-mutation rule. A hard
interruption from cancellation or timeout clears retained memory because its
consistency cannot be guaranteed, but keeps the retained language assignment;
`executions.reset` clears both memory and the language assignment so another
language can claim the execution.

### 5.3 JavaScript and TypeScript

```ts
interface JavaScriptExecutionOptions extends InlineExecutionOptions {
  format?: "module" | "commonjs"; // default: "module"
  filePath?: string;
}

type JavaScriptEvaluationOptions = Omit<
  JavaScriptExecutionOptions,
  "detached"
>;

interface TypeScriptExecutionOptions extends InlineExecutionOptions {
  filePath?: string;
  tsconfigPath?: string;
  compilerOptions?: Record<string, JsonValue>;
}

type TypeScriptEvaluationOptions = Omit<
  TypeScriptExecutionOptions,
  "detached"
>;

interface TypeScriptFileExecutionOptions extends LanguageExecutionOptions {
  tsconfigPath?: string;
  compilerOptions?: Record<string, JsonValue>;
}

interface TypeScriptCheckOptions {
  executionId?: string;
  createIfMissing?: boolean;
  cwd?: string;
  filePath?: string;
  tsconfigPath?: string;
  compilerOptions?: Record<string, JsonValue>;
  timeoutMs?: number;
  signal?: AbortSignal; // Core client only.
}

interface TypeScriptDiagnostic {
  code: number;
  category: "error" | "warning" | "suggestion" | "message";
  message: string;
  filePath?: string;
  line?: number;
  column?: number;
}

type TypeScriptCheckResult =
  | (CodeExecutionResult & {
      outcome: "succeeded";
      hasErrors: boolean;
      diagnostics: TypeScriptDiagnostic[];
    })
  | (CodeExecutionResult & {
      outcome: "failed" | "cancelled" | "timed_out";
      hasErrors?: never;
      diagnostics: TypeScriptDiagnostic[];
    });

javascript.execute(
  source: string,
  options?: JavaScriptExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
javascript.execute(
  source: string,
  options: JavaScriptExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

javascript.evaluate<T = JsonValue>(
  expression: string,
  options?: JavaScriptEvaluationOptions,
): Promise<CodeEvaluationResult<T>>;

javascript.executeFile(
  path: string,
  options?: LanguageExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
javascript.executeFile(
  path: string,
  options: LanguageExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

javascript.typescript.execute(
  source: string,
  options?: TypeScriptExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
javascript.typescript.execute(
  source: string,
  options: TypeScriptExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

javascript.typescript.evaluate<T = JsonValue>(
  expression: string,
  options?: TypeScriptEvaluationOptions,
): Promise<CodeEvaluationResult<T>>;

javascript.typescript.executeFile(
  path: string,
  options?: TypeScriptFileExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
javascript.typescript.executeFile(
  path: string,
  options: TypeScriptFileExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

javascript.typescript.check(
  source: string,
  options?: TypeScriptCheckOptions,
): Promise<TypeScriptCheckResult>;
```

TypeScript execute/evaluate methods transpile without semantic type checking.
They may fail for invalid syntax or an emit failure. Users call
`javascript.typescript.check` explicitly when they want diagnostics. There is no
`checkTypes` option and no public `emitTypeScript` or `compileTypeScript` API.
For check results, semantic type errors set `hasErrors: true` while the
operation outcome remains `succeeded`; the outcome is `failed` only when the
checker itself cannot complete. Diagnostic lines and columns are one-based.

`filePath` on inline source is diagnostic and module-resolution identity; it
does not read that path. File methods are unambiguous for reading and running
existing files.

### 5.4 npm workflows

```ts
interface NpmProjectInstallOptions
  extends Omit<LanguageExecutionOptions, "args" | "stdin" | "detached"> {
  frozen?: boolean;
}

interface NpmPackageInstallOptions
  extends Omit<LanguageExecutionOptions, "args" | "stdin" | "detached"> {
  dev?: boolean;
  global?: boolean;
}

interface NpmScriptOptions extends LanguageExecutionOptions {}

javascript.npm.install(
  options?: NpmProjectInstallOptions,
): Promise<CodeExecutionResult>;
javascript.npm.install(
  packages: string | string[],
  options?: NpmPackageInstallOptions,
): Promise<CodeExecutionResult>;

javascript.npm.runScript(
  script: string,
  options?: NpmScriptOptions & { detached?: false },
): Promise<CodeExecutionResult>;
javascript.npm.runScript(
  script: string,
  options: NpmScriptOptions & { detached: true },
): Promise<DetachedExecution>;
```

The options-only install overload installs the project in `cwd`. With
`frozen: true`, it requires the existing npm lockfile and performs a clean,
lockfile-exact install equivalent to `npm ci`; combining `frozen` with named or
global packages is invalid. Named packages are passed as separate argv entries.
`dev` and `global` cover stable install intents; uncommon npm flags remain
available through `process.execFile`.

`javascript.npm.runScript("build", { args: ["--watch"] })` maps to
`npm run build -- --watch`. npm owns the script's shell semantics; AgentOS does
not construct an outer shell command.

### 5.5 Python

```ts
interface PythonInstallOptions
  extends Omit<LanguageExecutionOptions, "args" | "stdin" | "detached"> {
  upgrade?: boolean;
  requirementsFile?: string;
  indexUrl?: string;
  extraIndexUrls?: string[];
}

python.execute(
  source: string,
  options?: InlineExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
python.execute(
  source: string,
  options: InlineExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

python.evaluate<T = JsonValue>(
  expression: string,
  options?: AttachedInlineExecutionOptions,
): Promise<CodeEvaluationResult<T>>;

python.executeFile(
  path: string,
  options?: LanguageExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
python.executeFile(
  path: string,
  options: LanguageExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

python.executeModule(
  module: string,
  options?: LanguageExecutionOptions & { detached?: false },
): Promise<CodeExecutionResult>;
python.executeModule(
  module: string,
  options: LanguageExecutionOptions & { detached: true },
): Promise<DetachedExecution>;

python.install(
  options?: PythonInstallOptions,
): Promise<CodeExecutionResult>;
python.install(
  packages: string | string[],
  options?: PythonInstallOptions,
): Promise<CodeExecutionResult>;
```

`python.executeModule("http.server", { args: ["8000"] })` is the typed
equivalent of `python -m http.server 8000`.

The options-only install overload installs the project in `cwd` unless
`requirementsFile` is supplied. Providing both named packages and a
requirements file is invalid.

Inline Python execution and evaluation support top-level `await`, `async for`,
and `async with`. Awaited work is part of the operation and subject to its
timeout. The API does not promise that background or otherwise unawaited
`asyncio` tasks continue between operations; reset, deletion, expiry, or
timeout cancels execution-owned asynchronous work.

npm and Python installs modify the VM-wide filesystem rather than an
execution-local environment. Installed packages are visible to every execution
in that VM and survive execution reset or deletion. Python requirements files
and pinned package specs provide the reproducible Python workflow; the API does
not invent a cross-ecosystem `frozen` meaning for pip. The sidecar serializes
package mutations at VM scope so installs submitted through different
executions cannot corrupt shared package-manager state. One mutation is
admitted at a time; another install submitted while it is active fails
immediately with `ExecutionBusyError`, so callers can wait for the active
execution before retrying without an implicit unbounded install queue.

## 6. Actor surface and serialization

The actor exports the same nested action tree as Core:

```text
process.exec
process.execFile
process.spawn
process.get
process.list
process.listAll
process.tree
process.wait
process.stop
process.kill
process.writeStdin
process.closeStdin
javascript.execute
javascript.evaluate
javascript.executeFile
javascript.typescript.execute
javascript.typescript.evaluate
javascript.typescript.executeFile
javascript.typescript.check
javascript.typescript.checkProject
javascript.npm.install
javascript.npm.runScript
javascript.npm.runPackage
python.execute
python.evaluate
python.executeFile
python.executeModule
python.install
executions.get
executions.list
executions.wait
executions.cancel
executions.writeStdin
executions.closeStdin
executions.readOutput
executions.reset
executions.delete
executions.signal
executions.resizePty
terminal.open
terminal.write
terminal.resize
terminal.wait
terminal.close
filesystem.readFile
filesystem.writeFile
filesystem.readFiles
filesystem.writeFiles
filesystem.stat
filesystem.mkdir
filesystem.readdir
filesystem.readdirEntries
filesystem.readdirRecursive
filesystem.exists
filesystem.move
filesystem.remove
filesystem.export
filesystem.mount
filesystem.unmount
filesystem.listMounts
network.httpRequest
software.list
software.link
agents.list
sessions.open
sessions.get
sessions.list
sessions.delete
sessions.unload
sessions.prompt
sessions.cancelPrompt
sessions.respondPermission
sessions.readHistory
sessions.getConfig
sessions.setConfigOption
sessions.getCapabilities
sessions.getAgentInfo
cron.schedule
cron.list
cron.cancel
```

RivetKit represents these on the wire with the same dotted names. Every action
maps to its corresponding sidecar request and does not reconstruct commands.
The actor-only preview URL actions remain outside this shared tree.

Actor signatures are derived mechanically from the core signatures: replace
`LanguageExecutionOptions` with `ActorLanguageExecutionOptions`, replace binary
chunks with `ActorData`, and omit core-only `AbortSignal` and callback fields.
This changes only host representation, never admission or runtime behavior.

The core and actor have matching behavior but intentionally different host
types:

| Core | Actor wire |
| --- | --- |
| `AbortSignal` | explicit `executions.cancel` action |
| callback functions | `executionOutput` and `executionCompleted` events |
| `Uint8Array` | tagged UTF-8/base64 `ActorData` |
| typed error class | serializable `{ code, message, details }` |
| caller-side generic evaluation type | runtime `JsonValue` |

Nested serializable option objects such as `pty: { cols, rows }` are allowed.
Actor methods must reject unsupported rich values rather than silently
dropping them. Every JavaScript
numeric input corresponding to Bare `u16`, `u32`, or `u64` is integer- and
range-checked. Every Bare integer exposed as a JavaScript `number` is checked
before conversion and must remain within JavaScript's safe-integer range;
bounded counts and retention make that enforceable.

The actor automatically acquires an internal keepalive lease before submitting
any attached or detached operation. It holds the lease until the operation is
terminal, all prior output and the completion event have been routed, and
required cleanup has finished; a pre-admission failure releases it immediately.
This behavior has no public option or action. An idle execution does not keep
the actor alive. If hibernation disposes the VM, its in-memory executions
expire; a later lookup returns
`ExecutionNotFoundError` unless the caller explicitly requests creation again.

## 7. Additional language and lifecycle APIs

### 7.1 Project TypeScript checking

```ts
javascript.typescript.checkProject(options?: {
  executionId?: string;
  createIfMissing?: boolean;
  cwd?: string;
  tsconfigPath?: string;
  timeoutMs?: number;
  signal?: AbortSignal; // Core client only.
}): Promise<TypeScriptCheckResult>;
```

This checks the complete project graph selected by `tsconfig.json` and does not
emit JavaScript.

### 7.2 One-shot npm package binaries

```ts
javascript.npm.runPackage(
  packageSpec: string,
  options?: LanguageExecutionOptions & { detached?: false } & {
    binary?: string;
  },
): Promise<CodeExecutionResult>;
javascript.npm.runPackage(
  packageSpec: string,
  options: LanguageExecutionOptions & { detached: true } & {
    binary?: string;
  },
): Promise<DetachedExecution>;
```

This covers the stable `npm exec`/`npx` intent. `binary` handles packages whose
executable differs from the package name. It does not expand into npm init,
pack, publish, audit, outdated, or every npm subcommand.

### 7.3 Output replay, reset, deletion, signals, and PTY

The shared lifecycle includes the methods already defined in section 4.3:

```text
executions.readOutput
executions.reset
executions.delete
executions.signal
executions.resizePty
```

`executions.readOutput` pages the current or most recently completed operation's
retained events with an opaque cursor. Omitting the cursor starts at its oldest
retained event. `nextCursor` is always returned and can be persisted even when
`hasMore` is false so a later call resumes from the same point while that
operation is still active. `limit` is a bounded event count; event chunk size is
also bounded independently. `truncated` reports that earlier output expired
from bounded retention. A new operation, reset, deletion, or recreation
invalidates old cursors with `ExecutionOutputCursorExpiredError` rather than
mixing output from different generations. Replay returns immediately; live
delivery uses the output subscription/event surface.
`executions.reset` accepts only an idle or failed execution, cancels
execution-owned background tasks, clears the retained language assignment and
state, results, and output, increments generation, and leaves filesystem
changes and installed packages untouched.

`executions.delete` requires an idle or failed execution. It removes retained
state, results, and output immediately. It does not implicitly cancel active
work or revert VM filesystem changes.

`executions.signal` targets the active execution-owned process group. A present
`pty` option allocates a terminal; `executions.resizePty` changes its dimensions.
PTY output uses the `pty` channel because terminal output merges stdout and
stderr.

`executions.writeStdin` resolves only after the sidecar has accepted the bytes
into its bounded input path, providing backpressure. It rejects input after
stdin is closed or when the active operation has no writable stdin.

## 8. Underlying mappings

Commands below are sidecar implementation details. Public callers provide
typed arguments; the sidecar uses argv invocation and does not construct an
outer shell string except for `process.exec`, whose purpose is configured-shell
execution.

| Public method | Sidecar operation | Shell involved |
| --- | --- | --- |
| `process.exec` | Submit configured-shell command to the execution | Yes |
| `process.execFile` | Submit exact command and argv | No |
| `process.spawn` | Start a managed child process and return its PID | No |
| `executions.get` / `executions.list` | Read bounded execution metadata | No command |
| `executions.wait` | Wait for the sole active operation, or return the last retained result | No command |
| `executions.cancel` | Gracefully terminate, then force the active process tree | No command |
| `executions.signal` | Signal the active execution-owned process group | No command |
| `executions.reset` | Clear retained state/output and increment generation | No command |
| `executions.delete` | Remove an idle execution and retained data | No command |
| `executions.readOutput` | Page bounded retained output | No command |
| `executions.writeStdin` / `executions.closeStdin` | Route input to the active process or terminal | No command |
| `executions.resizePty` | Resize the active terminal | No command |
| `javascript.execute` | Stage and run JS or evaluate it in the retained JS realm | No |
| `javascript.evaluate` | Evaluate in fresh or retained JS state and serialize the value | No |
| `javascript.executeFile` | `node <path> ...args` | No |
| `javascript.typescript.execute` | Transpile internally, then execute emitted JS | No |
| `javascript.typescript.evaluate` | Transpile internally, evaluate, and serialize | No |
| `javascript.typescript.executeFile` | Project-aware transpile and entry execution | No |
| `javascript.typescript.check` / `javascript.typescript.checkProject` | Internal TypeScript compiler API | No command |
| `javascript.npm.install` | `npm install ...`, or `npm ci` for a frozen project install | No |
| `javascript.npm.runScript` | `npm run <script> -- ...args` | npm owns script shell semantics |
| `javascript.npm.runPackage` | `npm exec --package=<spec> -- <binary> ...args` | No outer shell |
| `python.execute` | Run source in fresh or retained Python state | No |
| `python.evaluate` | Async evaluation and result serialization | No |
| `python.executeFile` | `python <path> ...args` | No |
| `python.executeModule` | `python -m <module> ...args` | No |
| `python.install` | `python -m pip install ...` | No |

Temporary source and result files use collision-free paths and are removed on
success, guest failure, cancellation, timeout, and host-side failure. Cleanup
failures are observable. Timeouts terminate the underlying process tree rather
than merely stopping the wait.

## 9. Provider and runtime survey

The survey was performed against official documentation available in July
2026. It informs this design but does not require provider compatibility.

| Product | Relevant surface | Decision for AgentOS |
| --- | --- | --- |
| E2B Code Interpreter | Language-selected code runs, persistent interpreter state, streaming callbacks, rich results, and typed template package installation | Keep language-specific execute/evaluate, typed installs, retained executions, and structured evaluation values. |
| Cloudflare Sandbox SDK | JS/TS/Python interpreters, top-level await, streaming callbacks, execution counts, and rich outputs | Supports retained executions and structured evaluation values; AgentOS keeps typed language methods rather than one generic language switch. |
| Cloudflare Dynamic Workers | Fresh/cached workers, module maps, bindings, network policy, and bundling | Existing VMs, filesystem, bindings, and networking own these concerns. |
| Daytona | JS/TS/Python execution, retained Python interpreter state, env, cwd, timeout, streaming, and structured errors | Confirms retained execution state and structured guest errors. |
| Freestyle | General `vm.exec`; guides stage temporary JS/Python files and invoke binaries | AgentOS should own those helpers and collision-free cleanup. |
| Vercel Sandbox | argv commands, cwd, env, streaming, detached execution, persistence, snapshots, files, and network policy | Existing process, VM, filesystem, and policy APIs cover this layer. |
| Modal Sandbox | General exec, streamed stdio, package installation, files, volumes, and snapshots | Keep uncommon setup in generic command/lifecycle APIs. |
| Deno Sandbox | Process spawning, tagged shell, files, package managers, services, volumes, and network policy | Reinforces a capable generic process escape hatch. |
| Pyodide | Async Python evaluation, persistent globals, top-level await, package loading, and live proxies | Adopt structured inputs, retained Python state, and top-level await; do not expose live proxies. |
| quickjs-emscripten | Eval, globals, modules, limits, interrupts, async jobs, and explicit value lifetimes | Adopt structured inputs and explicit limits; do not expose executor handles. |
| isolated-vm | Retained realms, compiled scripts/modules, arguments, timeouts, limits, metrics, and live references | Supports retained executions and structured inputs; live references remain deferred. |

## 10. Explicit exclusions

Do not add the following to this design:

- `executeBash`, `evaluateBash`, or a nested `bash` object;
- language-specific `spawn*` twins;
- abbreviated `npmInstall` or `pipInstall` names;
- one method per npm/pip maintenance subcommand;
- arbitrary raw package-manager flag bags;
- automatic type checking inside `javascript.typescript.execute`;
- a second retained-state resource or identifier beside execution and
  `executionId`;
- live guest values, JS references, Python proxies, or executor handles;
- implicit unbounded output, executions, package lists, temporary files,
  safety watchdogs, or cleanup grace periods.

Persistence does not require live host objects. Guest objects may remain in an
execution and later operations may refer to them, while returned values remain
JSON-compatible. A future live-object API may use opaque, generation-scoped RPC
reference IDs with explicit `get`, `set`, `call`, and `dispose`, leases, and
hard limits. Raw V8, QuickJS, or Python handles never cross the actor wire.

## 11. Errors and limits

- Validation and transport failures before admission throw typed host errors.
  After admission, guest, process-start, cancellation, timeout, enforcement,
  and cleanup failures produce a retained result and completion event with a
  structured `ExecutionErrorData`; enforcement or cleanup failure may also
  leave the execution in `failed` state.
- Evaluation serialization failure clearly states that returned values must be
  JSON-serializable.
- An absent explicit ID throws `ExecutionNotFoundError` unless
  `createIfMissing: true` was supplied.
- Incompatible retained-language reuse throws
  `ExecutionLanguageConflictError`.
- Submitting while an execution is active throws `ExecutionBusyError`.
- `executions.signal`, stdin, terminal resize, and cancellation against an
  idle execution throw typed state errors rather than silently succeeding.
- Package names, modules, scripts, paths, and args are never inserted into a
  shell command by AgentOS.
- Source size, input JSON, stdout, stderr, evaluation values, package count,
  execution count, retained memory, output history, and idle
  lifetime are bounded by default. Each limit warns near threshold and fails
  with a typed error naming the limit and which existing AgentOS VM/core option
  raises it.
- `timeoutMs` covers staging, compilation, guest execution, and result
  collection. Timeout terminates the active process tree; cleanup then gets an
  independent bounded grace period. The execution returns to idle unless
  enforcement or cleanup itself failed.

## 12. Implementation ownership

1. The sidecar/runtime owns execution admission, serialization, retained
   language state, default cwd, staging, invocation, transpilation, evaluation
   serialization, package command construction, process records, timeouts,
   application of existing VM limits, output retention, reset, and cleanup.
2. The TypeScript and Rust clients validate and serialize explicit caller input
   and expose generated methods. They do not construct language or package
   commands.
3. `@rivet-dev/agentos` actor actions forward requests and retain only
   actor-specific event routing.
4. TypeScript and Rust clients remain behaviorally identical for every public
   method and wire behavior added here.

## 13. Examples and documentation

- Rename examples from `examples/exec-{lang}-*` to `examples/{lang}-*`
  (`js-*` and `python-*`).
- Runnable docs code comes from real example files through `<CodeSnippet>`.
- Show language helpers first and `process.execFile` only as the
  uncommon-operation
  escape hatch.
- Show reuse of one execution ID to retain JavaScript and Python state.
- Show detached execution, output replay, cancellation, reset, and deletion.
- The npm guide shows project install and
  `javascript.npm.runScript("build")`.
- The Python guide shows package install, requirements install, file execution,
  module execution, and top-level `await` without direct binaries.
- The TypeScript guide states prominently that execution does not type check
  and demonstrates explicit `javascript.typescript.check`.
- Actor docs use tagged UTF-8/base64 data and events rather than callbacks or
  host-only values.

## 14. Acceptance criteria

- Every public method exists on `AgentOs`, in the Bare protocol, in the Rust
  client, and as a nested actor action with matching behavior and method path.
- `executionId` is the only public lifecycle identity. No second retained-state
  resource, identifier, or API exists.
- Reusing an execution ID preserves JS/TS or Python variables, imports,
  functions, and loaded modules. A concurrent submission fails with
  `ExecutionBusyError` immediately.
- Inline JavaScript retention preserves top-level lexical and import bindings
  without replaying previous source; file, module, Bash, package, and type-check
  operations use fresh processes and retain no language memory.
- Concurrent create-if-missing requests create exactly one execution; one
  operation is admitted and every conflicting submission fails as busy.
- Every language and package convenience has a first-class sidecar handler;
  clients never construct underlying binary commands.
- Detached operations return promptly, continue emitting ordered output, and
  are manageable through wait, cancel, signal, stdin, reset, and delete.
- Output replay recovers retained events after reconnect and reports truncation
  when earlier output expired. Public pagination uses opaque cursors rather
  than generation or sequence inputs.
- Admitting a new operation increments `generation`, replaces the previous
  result/output replay buffer, and invalidates its cursors without clearing
  healthy retained language memory.
- Reset clears retained language state and output, increments generation, and
  preserves filesystem changes and installed packages.
- Delete rejects active work and removes idle retained data immediately.
- PTY allocation, input, output, signals, and resizing work through the shared
  lifecycle.
- Python inline execution supports top-level `await`.
- TypeScript with a semantic type error executes when it can transpile, while
  explicit checking reports the error.
- Evaluation values and `inputs` remain JSON-compatible and injection-safe.
- Every admitted operation produces exactly one discriminated result and
  completion event; failed outcomes carry a serializable structured error, and
  buffered stream truncation is explicit.
- Evaluation values and TypeScript diagnostics are bounded,
  actor-serializable, and exposed through purpose-specific result fields.
- npm installs/scripts/package binaries and Python installs/modules work
  without users invoking runtime or package-manager binaries.
- Package installs are VM-wide and serialized against concurrent mutations
  from other executions.
- Actor actions accept only serializable values; output and completion use
  plain tagged events.
- Attached and detached operations automatically hold the actor keepalive lease
  through terminal event delivery and cleanup; no public keepalive API exists.
- Omitted `timeoutMs` means no caller deadline, while explicit values cover the
  complete admitted operation and independent safety limits remain bounded.
- Existing AgentOS VM limits apply unchanged, warn near threshold, and fail
  with typed actionable errors; language helpers add no duplicate limit knobs
  or usage fields.
- Timeouts terminate the active process tree and temporary files are cleaned on
  every exit path.
- Applicable `CLAUDE.md` files contain the scope rule from section 2.
- Public docs and examples use final package names and renamed example paths.
- `pnpm build`, `pnpm check-types`, focused language tests, actor tests, and
  `pnpm --dir website build` pass.

## 15. Remaining API candidates

These remain outside the committed surface:

| Candidate | Reason to consider | Why deferred |
| --- | --- | --- |
| Python editable and constraints-file installs | Common development and enterprise pip workflows | Add from demonstrated workflows, not one API per pip flag. |
| Execution artifacts | Bounded metadata for intentionally produced files | Filesystem APIs already provide access; discovery and retention are undefined. |
| Project package-manager selection | Honor npm/pnpm/Yarn/Bun or pip/uv project tooling | Detection and availability behavior must be explicit and reproducible. |
| Durable retained state | Recreate an execution after sidecar or actor restart | Guest heaps are not safely serializable; replay versus snapshot behavior needs design. |
| Execution snapshot/fork | Notebook checkpoints and branching experiments | Heap cloning is runtime-specific and potentially large. |
| Background tasks between operations | Keep timers and `asyncio` tasks alive while idle | Requires explicit liveness, output, cancellation, and accounting rules. |
| Opaque live guest references | RPC access to non-JSON guest objects | Requires leases, generations, hard limits, and disposal. |

Do not add language-specific spawn methods, runtime-binary methods, one API per
npm/pip subcommand, or generic raw flags. Detached execution and
`process.execFile` already cover those cases.

## 16. References

- E2B: <https://e2b.dev/docs/sdk-reference/code-interpreter-js-sdk/v2.0.0/sandbox>
- E2B package installation: <https://e2b.dev/docs/quickstart/install-custom-packages>
- Cloudflare Sandbox interpreter: <https://developers.cloudflare.com/sandbox/api/interpreter/>
- Cloudflare Dynamic Workers API: <https://developers.cloudflare.com/dynamic-workers/api-reference/>
- Daytona process and code execution: <https://www.daytona.io/docs/en/process-code-execution/>
- Freestyle Node.js guide: <https://www.freestyle.sh/docs/guides/run-nodejs-in-a-sandbox>
- Freestyle Python guide: <https://www.freestyle.sh/docs/guides/run-python-in-a-sandbox>
- Vercel Sandbox SDK: <https://vercel.com/docs/sandbox/sdk-reference>
- Modal Sandboxes: <https://modal.com/docs/guide/sandboxes>
- Deno Sandbox: <https://docs.deno.com/sandbox/>
- Pyodide JavaScript API: <https://pyodide.org/en/stable/usage/api/js-api.html>
- Python top-level-await compiler flag: <https://docs.python.org/3/library/ast.html#ast.PyCF_ALLOW_TOP_LEVEL_AWAIT>
- TypeScript compiler API: <https://github.com/microsoft/TypeScript/wiki/Using-the-Compiler-API>
- quickjs-emscripten: <https://github.com/justjake/quickjs-emscripten>
- isolated-vm: <https://github.com/laverdet/isolated-vm>
