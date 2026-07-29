---
name: release
description: Cut an AgentOS release via the scripts/publish flow. Use when the user asks to release, publish, cut a release, or bump the AgentOS version.
---

# AgentOS Release

The publish flow lives in `scripts/publish` and is driven by
`.github/workflows/publish.yaml`.

AgentOS is the source of truth. It publishes npm packages, crates, runtime
sidecars, Pyodide/R2 assets, and `@agentos-software/*` registry packages from
this repository. agentos releases are generated compatibility shims that
follow the AgentOS version; never release agentos first.

## Procedure

1. Start from a clean, pushed `merge-aos` or `main` bookmark.
2. Run the local trigger:

```bash
just release --version 0.2.0       # exact version
just release --version 0.2.0-rc.1  # rc (npm tag `rc`)
just release --patch               # semver bump from latest git tag
```

3. Watch the workflow:

```bash
run=$(gh run list -R rivet-dev/agentos --workflow=publish.yaml -L1 --json databaseId --jq '.[0].databaseId')
gh run watch -R rivet-dev/agentos "$run" --exit-status
```

## Notes

- Never publish to npm or crates.io locally; always go through `publish.yaml`.
- `scripts/publish/src/local/cut-release.ts` is a pure trigger; version changes
  happen in the ephemeral CI checkout.
- `workspace:*` deps are rewritten to literal versions by the publish bump pass.
- Generated agentos shims are dispatched after AgentOS publishes and must
  use the same version.
- If anything fails, stop and report — do not retry automatically.
