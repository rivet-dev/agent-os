# Execution API Redesign — Contexts, Processes, TypeScript

Status: implemented

Implementation handoff. This spec replaces the current `executionId` /
`createIfMissing` / `detached` model for language execution with three separate,
clearly-named concepts. Read it top to bottom before touching code — the three
changes are independent in code but share one mental model.

## Motivation

Today one identifier (`executionId`) and one boolean (`detached`) try to model
three unrelated things at once:

1. **Retained state** — a persistent realm holding globals/imports between calls.
2. **A running background process** — a dev server, `python -m http.server`.
3. **A single attached run** — execute-and-return-a-value.

This conflation is the root of every point of confusion:

- `detached: true` silently flips the return type (value vs descriptor).
- `createIfMissing` is resource-creation lifecycle smuggled into a per-call
  boolean; callers must remember "set it on the first call, not the rest."
- A retained execution reads like a running process ("shouldn't it need
  `detached` to stay alive?") when it is actually **idle state**.
- Two backgrounding mechanisms already coexist (`detached` executions **and**
  `process.spawn`) and overlap.

The fix splits these into concepts with names that fit their lifetime:

| Concept | What it is | Healthy when | Handle | Namespace |
|---|---|---|---|---|
| **Context** | retained language state (globals/imports) | **idle** — holding state between calls | `contextId` (string) | `vm.contexts.*` |
| **Process** | a running background program with a lifecycle | **running** — active until it exits | `pid` (number) | `vm.process.*` |
| **One-shot run** | attached execute/evaluate, returns a value | n/a — completes inline | none | `vm.<lang>.*` |

Interaction model, stated plainly:

- **Context** = an interpreter you *send snippets to and get values back from*
  (request/response, retains state).
- **Process** = a program you *start, observe, signal, and stop* (fire-and-manage,
  returns no value).

## Locked decisions

1. **Contexts replace `executionId` + `createIfMissing`.**
   - `vm.createContext(id: string)` — caller assigns the id (a plain string).
   - **Never auto-generate.** Create **errors if the id already exists**
     (`context_conflict`). There is no get-or-create — you create once,
     explicitly.
   - `createContext` returns nothing meaningful (the caller already holds the
     id). No `{ contextId }` wrapper object.
   - Every execution option that was `executionId` becomes `contextId`.
   - `createIfMissing` is **removed everywhere**.
2. **TypeScript flattens** from `vm.javascript.typescript.*` to top-level
   `vm.typescript.*`, a sibling of `vm.javascript` / `vm.python`.
3. **Cross-language in one context is a hard error** (`context_language_mismatch`),
   never silent-and-unretained. A context pins to the first inline language used.
4. **Process split** replaces `detached` executions. `detached` and
   `DetachedExecution` are **removed**. Every attached verb gets a `spawn` twin
   that returns a `pid`; process control lives on `vm.process.*`.
5. **No back-compat.** Client and sidecar ship same-version lockstep. Remove old
   fields/types outright — no aliases, converters, or dual-write paths.

## Open decision (must resolve before finishing)

**`contextId` on `spawn`** — may a background process run *inside* a retained
realm (`LanguageSpawnOptions.contextId`)?

- **Forbid (simpler, recommended default):** processes are always standalone
  with a fresh realm; contexts are only for attached `execute`/`evaluate`. The
  two axes stay fully independent.
- **Allow (more powerful):** a spawned process may read/mutate a context's state,
  but an alive process **pins that context busy** (no other op runs in it until
  the process exits), matching the existing one-active-op-per-slot rule.

Ship the forbid version unless the owner says otherwise; the field can be added
later without breaking anything.

---

## Change 1 — Contexts

### New surface

```ts
// Create a context. Caller-assigned string id. Errors if it already exists.
vm.createContext(id: string): Promise<void>            // throws context_conflict

// Lifecycle (moved off the old executions.* namespace)
vm.contexts.reset(id: string): Promise<void>           // clear retained memory, keep the slot
vm.contexts.delete(id: string): Promise<void>          // remove the slot
vm.contexts.get(id: string): Promise<ContextDescriptor>
vm.contexts.list(): Promise<ContextDescriptor[]>
```

```ts
interface ContextDescriptor {
  contextId: string;
  state: "idle" | "running" | "resetting" | "deleting" | "failed";
  language?: "javascript" | "python";   // pinned on first inline run
  createdAtMs: number;
  lastStartedAtMs?: number;
  lastCompletedAtMs?: number;
}
```

### Option-type changes

In `packages/core/src/language-execution.ts`:

- `LanguageExecutionOptions`: **remove** `executionId`, `createIfMissing`,
  `detached`. **Add** `contextId?: string`. (`detached` leaves because
  backgrounding is now `spawn` — see Change 3.)
- `InlineExecutionOptions`: unchanged except it inherits the above (keeps
  `inputs`).
- `TypeScriptCheckOptions`: rename `executionId → contextId`, remove
  `createIfMissing`.
- `JavaScriptEvaluationOptions` / `TypeScriptEvaluationOptions`: currently
  `Omit<…, "detached">`. Since `detached` no longer exists on the base, drop the
  `Omit`.

### Behavior

- Referencing an unknown `contextId` on any op → typed `context_not_found`.
- A context pins to its first inline language; a later op in the other language
  → `context_language_mismatch` (Change 3).
- Non-inline ops (files, modules, npm, `check`) may still pass a `contextId` for
  grouping but run in fresh processes and do **not** touch retained memory —
  same as today.
- Context creation obeys the existing live/completed-execution limits; over-
  creation fails with the typed limit error naming the knob.
- Contexts are **VM-lifetime, not durable across sleep/wake.** Retained memory
  dies when the actor VM is disposed on sleep. A `contextId` persisted in
  durable actor state will dangle after wake and must return `context_not_found`
  (fail loud, never silently run in a fresh realm). Document the actor-idiomatic
  pattern: create the context lazily on first stateful use after wake; do not
  assume a stored id survives sleep.

### Before / after

```ts
// BEFORE
await vm.javascript.execute("const answer = 40", { executionId: "analysis", createIfMissing: true });
const r = await vm.javascript.evaluate("answer + inputs.increment",
  { executionId: "analysis", inputs: { increment: 2 } });
await vm.executions.reset("analysis");
await vm.executions.delete("analysis");

// AFTER
await vm.createContext("analysis");
await vm.javascript.execute("const answer = 40", { contextId: "analysis" });
const r = await vm.javascript.evaluate("answer + inputs.increment",
  { contextId: "analysis", inputs: { increment: 2 } });
await vm.contexts.reset("analysis");
await vm.contexts.delete("analysis");
```

---

## Change 2 — TypeScript flattening

Move the whole namespace up one level.

```ts
// BEFORE                          // AFTER
vm.javascript.typescript.execute      → vm.typescript.execute
vm.javascript.typescript.evaluate     → vm.typescript.evaluate
vm.javascript.typescript.executeFile  → vm.typescript.executeFile
vm.javascript.typescript.check        → vm.typescript.check
vm.javascript.typescript.checkProject → vm.typescript.checkProject
```

- `vm.typescript` is a top-level language module, a peer of `vm.javascript` and
  `vm.python`.
- The "TS shares the JS realm" fact is now expressed by **passing the same
  `contextId`** to both — it is no longer encoded in the method path:

```ts
await vm.createContext("build");
await vm.typescript.execute("const total: number = 40", { contextId: "build" });
await vm.javascript.evaluate("total + 2", { contextId: "build" }); // 42, same realm
```

- Execution still transpiles without semantic type checking; `check` /
  `checkProject` remain the explicit diagnostics path.
- `vm.typescript` also gets the `spawn` twins from Change 3.

---

## Change 3 — Process split (replaces `detached`)

`detached: true` and the `DetachedExecution` descriptor are **deleted**. Every
attached verb gets a `spawn` twin returning a `pid`; all run-control methods live
on `vm.process.*`.

### Launch — `spawn` mirrors `execute`

```ts
// Shell (exists today; now the only background path for shell)
vm.process.spawn(command: string, args?: string[], opts?: SpawnOptions): Promise<ProcessDescriptor>

// Language source
vm.javascript.spawn(source: string, opts?: LanguageSpawnOptions): Promise<ProcessDescriptor>
vm.typescript.spawn(source: string, opts?: LanguageSpawnOptions): Promise<ProcessDescriptor>
vm.python.spawn(source: string, opts?: LanguageSpawnOptions): Promise<ProcessDescriptor>

// File / module twins (parallel to executeFile / executeModule)
vm.javascript.spawnFile(path: string, opts?: LanguageSpawnOptions): Promise<ProcessDescriptor>
vm.python.spawnFile(path: string, opts?: LanguageSpawnOptions): Promise<ProcessDescriptor>
vm.python.spawnModule(module: string, opts?: LanguageSpawnOptions): Promise<ProcessDescriptor>
```

`evaluate` has **no** spawn twin — a background process returns no value.

### Control — keyed by `pid`, all under `vm.process.*`

```ts
vm.process.wait(pid: number): Promise<ProcessExit>
vm.process.signal(pid: number, signal: ExecutionSignal): Promise<void>
vm.process.kill(pid: number): Promise<void>                          // SIGKILL convenience
vm.process.writeStdin(pid: number, data: string | Uint8Array): Promise<void>
vm.process.closeStdin(pid: number): Promise<void>
vm.process.resizePty(pid: number, size: { cols: number; rows: number }): Promise<void>
vm.process.readOutput(pid: number, opts?: { after?: number }): Promise<OutputReplay>  // requires retainEvents
vm.process.get(pid: number): Promise<ProcessDescriptor>
vm.process.list(): Promise<ProcessDescriptor[]>
vm.process.tree(): Promise<ProcessNode[]>
```

### Types

```ts
interface ProcessDescriptor {
  pid: number;
  state: "running" | "exited";
  language?: "javascript" | "python";   // absent for shell
  command?: string;                       // present for shell
  startedAtMs: number;
}

interface ProcessExit {
  pid: number;
  outcome: "exited" | "signalled" | "timed_out";
  exitCode?: number;
  signal?: ExecutionSignal;
}

interface SpawnOptions {
  cwd?: string;
  env?: Record<string, string>;
  args?: string[];
  stdin?: string | Uint8Array;
  timeoutMs?: number;
  pty?: { cols?: number; rows?: number };
  signal?: AbortSignal;                    // abort → kills the process
  onStdout?: (chunk: Uint8Array) => void;  // live stream
  onStderr?: (chunk: Uint8Array) => void;
  output?: { retainEvents?: boolean };     // NB: no `capture`
}

interface LanguageSpawnOptions extends SpawnOptions {
  contextId?: string;   // gated by the open decision above
}
```

**Key difference from attached `exec`:** `SpawnOptions.output` has **no
`capture`.** A background process cannot hand back completed stdout inline — you
stream it live (`onStdout`) or replay it later (`retainEvents` + `readOutput`).
`capture` remains only on the attached path.

### Live output

```ts
// Core callbacks (exist today)
vm.onProcessOutput(pid, (chunk) => { ... });
vm.onProcessExit(pid, (exit) => { ... });
// Actor: broadcast as `processOutput` / `processExit` events
```

### Before / after

```ts
// BEFORE — background dev server via detached execution
await vm.javascript.execute("startServer()", {
  executionId: "dev", detached: true, output: { retainEvents: true },
});
await vm.executions.readOutput("dev");
await vm.executions.signal("dev", "SIGTERM");
await vm.executions.wait("dev");

// AFTER — a process
const proc = await vm.javascript.spawn("startServer()", { output: { retainEvents: true } });
await vm.process.readOutput(proc.pid);
await vm.process.signal(proc.pid, "SIGTERM");
await vm.process.wait(proc.pid);
```

```ts
// BEFORE — python -m http.server in the background
await vm.python.executeModule("http.server", {
  args: ["8000"], executionId: "server", detached: true, output: { retainEvents: true },
});

// AFTER
const proc = await vm.python.spawnModule("http.server", {
  args: ["8000"], output: { retainEvents: true },
});
await vm.process.readOutput(proc.pid);
```

### Full attached ↔ background pairing

| Intent | Attached (blocks, returns result) | Background (returns `pid`) |
|---|---|---|
| JS source | `javascript.execute` / `evaluate` | `javascript.spawn` |
| JS file | `javascript.executeFile` | `javascript.spawnFile` |
| TS source | `typescript.execute` / `evaluate` | `typescript.spawn` |
| Python source | `python.execute` / `evaluate` | `python.spawn` |
| Python module | `python.executeModule` | `python.spawnModule` |
| Shell | `process.exec` / `execFile` | `process.spawn` |

---

## The old `executions.*` namespace

It is split and **removed**:

- State lifecycle (`reset`, `delete`, `get`, `list`) → `vm.contexts.*`.
- Run control (`wait`, `readOutput`, `signal`, `writeStdin`, `closeStdin`,
  `resizePty`, `cancel`) → `vm.process.*`, keyed by `pid`.

There is no `vm.executions.*` after this change.

---

## Actor / RivetKit compliance (must hold)

- All handles are **ids**: `contextId` (string), `pid` (number). Descriptors and
  results are JSON-serializable. **No object handles cross the wire** — this
  satisfies core's "reference resources by ID, no object references in the public
  API" invariant.
- Any `ctx.execute()`-style sugar, if ever added, must be a **thin client-side
  wrapper** over `contextId`; it can never be the canonical/actor surface.
- New method paths (`createContext`, `contexts.*`, `typescript.*`, `*.spawn*`,
  new `process.*` controls) must be **mirrored as actor actions** with dotted
  wire names and registered as reserved in `packages/agentos/src/actor.ts`, same
  treatment `javascript.execute` gets today. Callbacks become `processOutput` /
  `processExit` events.
- **TS and Rust clients change in lockstep and stay behaviorally identical.**

---

## Files to change

**TypeScript (client):**
- `packages/core/src/language-execution.ts` — option types, descriptors, result
  types; add `ContextDescriptor`, `ProcessDescriptor`, `ProcessExit`,
  `SpawnOptions`, `LanguageSpawnOptions`; remove `DetachedExecution`,
  `createIfMissing`, `detached`, `executionId`.
- `packages/core/src/agent-os.ts` — implement `createContext`, `contexts.*`,
  `vm.typescript.*`, `*.spawn` / `spawnFile` / `spawnModule`, new `process.*`
  controls; delete the detached-execution admission branch and `executions.*`;
  rewrite the `execute`/`evaluate` overload signatures (the `detached?: false`
  overload blocks go away).
- `packages/core/src/generated-protocol.ts` — **generated**; regenerate
  from the `.bare` schema after the wire fields change (drop `createIfMissing` /
  `detached`, rename `executionId → contextId`, add process/context messages).
- `packages/core/type-tests/nested-api.ts` — update to the new nesting.
- `packages/core/tests/public-api-exports.test.ts` — keep the entrypoint truthful.
- `packages/agentos/src/actor.ts` — register the new reserved action names and
  events; drop the removed ones.

**Rust (sidecar, same-version lockstep):**
- `crates/vm/*` and `crates/sidecar/*` — wire fields,
  context/process state machines, error variants (`context_not_found`,
  `context_conflict`, `context_language_mismatch`). Rust owns the state; the TS
  client forwards.

**Docs:**
- `website/src/content/docs/docs/bash.mdx`
- `website/src/content/docs/docs/javascript.mdx`
- `website/src/content/docs/docs/python.mdx`
- `docs/features/typescript.mdx`
- `website/src/content/docs/docs/core.mdx`
- Add a "Contexts" section; retarget the TS namespace; replace all `executionId`
  / `createIfMissing` / `detached` call sites with `createContext` / `contextId`
  / `spawn`. Validate with `pnpm --dir website build`.

**Examples:**
- `examples/js-sdk-overview/src/contexts.ts`
- `examples/python-sdk-overview/src/contexts.ts`
- `examples/js-typescript/*` (retarget `vm.typescript.*`)
- `examples/js-dev-servers/*` (detached → `spawn`)
- Any quickstart using `executionId` / `createIfMissing` / `detached`.

---

## New error types (typed, name the offending id/field)

- `context_not_found` — op referenced an unknown `contextId`.
- `context_conflict` — `createContext` called with an existing id.
- `context_language_mismatch` — ran a different language in a pinned context.
- Existing bounded-limit errors continue to name the knob to raise.

---

## Suggested sequencing

1. **Contexts** (Change 1) + **TypeScript flattening** (Change 2) together — they
   share the option-type edits and are self-contained.
2. **Process split** (Change 3) — larger; deletes `detached` and rewrites the
   overloads. Resolve the open `contextId`-on-`spawn` decision first.
3. Docs + examples + actor mirroring alongside each change (not deferred).
4. Regenerate the protocol and update the Rust side in the same PR as the TS
   change — lockstep, no intermediate compatibility state.
```
