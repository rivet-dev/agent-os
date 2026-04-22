# TODO: Mirror `python.dbt` option into the Rivet actor

**Why deferred:** the Rivet repo wasn't available at `~/r-aos` (or any
`~/r*` path) at implementation time. Per `CLAUDE.md` line 34, every public
method/option change on `AgentOs` must mirror into the Rivet actor at
`~/r-aos/rivetkit-typescript/packages/rivetkit/src/agent-os/`. This file
documents the exact mirror work for whoever picks it up.

## What changed in agent-os

`AgentOsOptions.python?: PythonConfig` — new option in
[packages/core/src/agent-os.ts](../../packages/core/src/agent-os.ts).
Shape:

```ts
export interface PythonConfig {
  dbt?: boolean | DbtConfig;
}
export interface DbtConfig {
  wheelsPackage?: string;
  profilesDir?: string;
  projectsDir?: string;
  extraWheels?: string[];
}
```

When `python.dbt` is truthy, `AgentOs.create` resolves the
`@rivet-dev/agent-os-python-wheels` package on the host, mounts its
`wheels/` directory at `/wheels` inside the Pyodide worker via NODEFS,
preloads the dbt+DuckDB wheel set via micropip, and runs the
`DBT_BOOTSTRAP_SCRIPT` (multiprocessing monkey-patch + env defaults +
ThreadPool stub + `_multiprocessing` stub).

## What needs to change in the Rivet actor

In `~/r-aos/rivetkit-typescript/packages/rivetkit/src/agent-os/`:

1. Add `python?: PythonConfig` to the actor's input config type (mirror
   `PythonConfig` and `DbtConfig` shapes).
2. Forward the option into the underlying `AgentOs.create({ python })` call.
3. The wheels package path must resolve correctly inside the actor's
   runtime (Cloudflare Worker, Node, etc.). On Workers there is no host
   filesystem; document that `python.dbt` is unsupported on the Workers
   driver until a non-NODEFS mount path exists.
4. Add a driver-suite test under
   `~/r-aos/rivetkit-typescript/packages/rivetkit/src/driver-test-suite/tests/agent-os-dbt.test.ts`
   mirroring `packages/core/tests/dbt-smoke.test.ts`.

## Quickstart parity

Per `CLAUDE.md` line 36, `examples/quickstart/dbt.ts` must be mirrored at
`~/r-aos/examples/agent-os/dbt/`.

## Docs parity

Per `CLAUDE.md` line 158, mirror `docs/python-compatibility.mdx` updates
into `~/r-aos/docs/docs/agent-os/`.

## When to do this

Before any release that exposes `python.dbt` to actor users.
