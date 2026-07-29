# agentOS Apps end-to-end verification

This expensive check starts a local Rivet engine, installs the real `rivetkit`
npm dependency inside an AgentOS build VM, serves the packed app, calls a guest
actor through DirectActor, replaces the execution replica, and verifies both
SQLite state recovery and failed-build rollback.

```sh
pnpm --filter @rivet-dev/agentos-e2e-apps test:e2e
```

For routing and host-runtime iteration, cache only the content-addressed
`.aospkg` build artifact:

```sh
pnpm --filter @rivet-dev/agentos-e2e-apps test:e2e:fast
```

The first fast run performs the full package build. Later runs stop after HTTP
and DirectActor checks and reuse `/tmp/agentos-apps-artifact-cache-v16`. Every
run still starts with a fresh Rivet database, uploads the artifact into actor
SQLite, materializes it in a replica, and exercises the real routing path.
Remove that cache directory to force a clean build. The normal `test:e2e`
command never reads the cache and remains the required final gate.

To test an unpublished local RivetKit package without checking a tarball into
this repository:

```sh
AGENTOS_APPS_RIVETKIT_TARBALL=/absolute/path/to/rivetkit.tgz \
  pnpm --filter @rivet-dev/agentos-e2e-apps test:e2e:fast
```

The test uploads that tarball as `vendor/rivetkit.tgz` and installs it through
`file:./vendor/rivetkit.tgz`. The guest still installs
`@rivetkit/rivetkit-wasm@2.3.9` directly and omits optional dependencies.
