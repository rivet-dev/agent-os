# Secure-Exec Public Package Plan

## Goal

Reintroduce the documented public `secure-exec` package surface on top of the Agent OS runtime that now lives in this repo, while keeping the standalone `secure-exec` repo docs-only.

This plan is intentionally narrower than the earlier runtime-consolidation work. It only restores the public packages that current stable docs tell users to install.

## Public Scope

### In scope

- `secure-exec`
- `@secure-exec/typescript`

### Deferred

- `@secure-exec/browser`
- `@secure-exec/core`
- `@secure-exec/nodejs`
- `@secure-exec/v8`
- `@secure-exec/python`
- `@secure-exec/kernel`
- `@secure-exec/os-*`
- `@secure-exec/runtime-*`

## Source Of Truth For Scope

Use the standalone `secure-exec` docs repo as the product contract. The repo that contains `docs/docs.json` defines what is public.

Current stable docs show:

- `secure-exec` is the main package users install.
- `@secure-exec/typescript` is a companion package users install for sandboxed TypeScript tooling.
- The `kernel`, `runtime-*`, and related low-level packages live under experimental or advanced docs and are not part of the minimum compatibility target for this pass.

## Target Repos

### Agent OS repo

Agent OS becomes the implementation monorepo for both:

- the real runtime
- the `secure-exec` compatibility packages

Target package layout:

```text
packages/
  core/                    # existing high-level AgentOs SDK
  agent-os-core/           # new low-level runtime facade for compatibility layers
  secure-exec/             # public compatibility package
  secure-exec-typescript/  # public companion package
  browser/                 # existing runtime package, not public secure-exec scope for this pass
  dev-shell/
  kernel-legacy-staging/
  native-runtime-legacy-staging/
  posix/
  v8-sidecar-legacy-staging/
```

### Standalone secure-exec repo

The standalone `secure-exec` repo becomes docs-only:

```text
README.md
docs/
packages/
  README.md
package.json               # only if needed for docs tooling
```

`packages/README.md` should explain that runtime code moved into the Agent OS monorepo and point to the new package locations.

## Phase Plan

### Phase 1: Add `@rivet-dev/agent-os-core`

Create a new package whose job is to present the low-level runtime primitives that the compatibility layer needs.

Responsibilities:

- Re-export the runtime primitives that `secure-exec` wraps:
  - `NodeRuntime`
  - `createKernel`
  - `createNodeDriver`
  - `createNodeRuntimeDriverFactory`
  - `createInMemoryFileSystem`
  - `allowAll`, `allowAllFs`, `allowAllNetwork`, `allowAllChildProcess`, `allowAllEnv`
  - the related runtime, filesystem, permission, and stdio types
- Compose existing Agent OS runtime packages rather than duplicating logic.
- Stay low-level. This package is the compatibility substrate, not the `AgentOs` VM product API.

Implementation rule:

- `@rivet-dev/agent-os-core` should mostly be a curated facade over `@rivet-dev/agent-os-kernel`, `@rivet-dev/agent-os-nodejs`, and the existing runtime compatibility exports.

Non-goals:

- Do not move the high-level `AgentOs` class into this package.
- Do not rebuild legacy package topology under `@secure-exec/*`.

### Phase 2: Add `packages/secure-exec`

Create the public compatibility package named `secure-exec`.

Dependencies:

- `@rivet-dev/agent-os-core`

Exports:

- Only the documented stable surface that users import from `secure-exec`
- Re-export wrappers and types from `@rivet-dev/agent-os-core`

Compatibility target:

- Preserve the public Node runtime API shape where practical:
  - `NodeRuntime`
  - `NodeRuntimeOptions`
  - `createNodeDriver`
  - `createNodeRuntimeDriverFactory`
  - `createKernel`
  - filesystem helpers
  - permission helpers
  - documented types used by the stable docs

Deliberate exclusions for this pass:

- No `./browser` export
- No Python export
- No attempt to preserve every historical internal subpath

Implementation rule:

- `packages/secure-exec` should contain compatibility glue only.
- It must not become a second source of runtime truth.

### Phase 3: Add `packages/secure-exec-typescript`

Create the public companion package named `@secure-exec/typescript`.

Dependencies:

- `secure-exec`
- `typescript`
- `@rivet-dev/agent-os-core` only if the implementation needs direct low-level types

Compatibility target:

- Preserve `createTypeScriptTools`
- Preserve the documented request/result shapes
- Keep the compiler execution model inside the sandbox runtime

Migration source:

- Legacy `secure-exec` TypeScript package implementation
- Any already-ported logic from `examples/ai-agent-type-check`

Implementation rule:

- Keep the package narrowly focused on TypeScript tooling.
- Do not reintroduce broad runtime logic here.

### Phase 4: Trim Standalone secure-exec Repo To Docs

After the compatibility packages exist in Agent OS:

- delete the runtime workspaces from the standalone `secure-exec` repo
- keep the docs site and root docs files
- add `packages/README.md`
- update package references in the docs to match the supported public packages for this reduced scope

Required docs cleanup:

- `docs/api-reference.mdx` should list only the packages that remain part of the public compatibility promise for this pass
- `docs/sdk-overview.mdx` should match the actual install story
- `docs/quickstart.mdx` should use the wrapped API that now comes from Agent OS-backed compatibility packages
- `docs/features/typescript.mdx` should point at the restored `@secure-exec/typescript`

## Validation

### Package validation

- `pnpm --dir packages/agent-os-core build`
- `pnpm --dir packages/secure-exec build`
- `pnpm --dir packages/secure-exec-typescript build`

### Behavioral validation

- Port or add focused tests for the stable public `secure-exec` API
- Port or add focused tests for `createTypeScriptTools`
- Verify example flows covered by stable docs:
  - basic `NodeRuntime` execution
  - permissions
  - filesystem
  - TypeScript typecheck/compile

### Docs validation

- grep the standalone docs repo for references to removed public packages
- confirm install instructions only mention packages supported by this plan

## Acceptance Criteria

- `secure-exec` exists in Agent OS as a public compatibility package backed by Agent OS primitives
- `@secure-exec/typescript` exists in Agent OS as a public compatibility package backed by Agent OS primitives
- the standalone `secure-exec` repo contains docs only
- `packages/README.md` exists in the standalone `secure-exec` repo and points readers to the Agent OS monorepo
- public docs and exported package surfaces match

## Current Verification Snapshot

What is already true in the current Agent OS tree:

- the runtime internals have already been moved into Agent OS-owned packages
- the Node runtime primitives used by `secure-exec` already exist in the runtime packages
- the old `@secure-exec/*` dependencies are no longer active in the Agent OS runtime tree

What is still missing:

- `@rivet-dev/agent-os-core`
- `packages/secure-exec`
- `packages/secure-exec-typescript`
- the docs-only final state of the standalone `secure-exec` repo

## Workflow Rule For Future Work

When a request says "secure-exec" but does not name a package, treat it as ambiguous between:

- `secure-exec`
- `@secure-exec/typescript`

If the correct target is not obvious from the requested symbol or file path, ask the user which public package should own the change before editing code.
