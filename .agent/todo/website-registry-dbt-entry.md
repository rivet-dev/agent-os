# TODO: Add `@rivet-dev/agent-os-python-wheels` to website registry

**Why deferred:** the website registry data file lives outside this repo
(no `website/src/data/registry.ts` exists locally). Per `CLAUDE.md` line
159, every new `@rivet-dev/agent-os-*` package must have a corresponding
website entry.

## What to add

In `website/src/data/registry.ts` (in the website repo), add a new entry
under the "Software" category:

```ts
{
  name: "@rivet-dev/agent-os-python-wheels",
  category: "software",
  description:
    "Pre-built Pyodide wheels for the dbt-core + dbt-duckdb + DuckDB stack. Enables `python.dbt: true` in AgentOs.create().",
  // Other fields per the existing entries (homepage, npm, etc.).
}
```

## When to do this

When the website repo is next updated, or as part of the same release
that publishes `@rivet-dev/agent-os-python-wheels` to npm.
