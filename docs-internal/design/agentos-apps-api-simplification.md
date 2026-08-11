# agentOS Apps API Simplification

Status: implemented and end-to-end validated proof of concept; production
credential plumbing and repo-wide release gates remain open.

This document records the implemented agentOS Apps public API and tracks the
remaining work required to take the proof of concept to production. The runtime
moves platform plumbing out of the user-facing surface while preserving
ordinary RivetKit clients and DirectActor calls.

## Goals

- Keep `setup()` and `createClient()` as ordinary RivetKit APIs.
- Make `setupApps()` responsible only for creating agentOS Apps actor
  definitions.
- Give every infrastructure actor a stable `agentOSApps*` registry name.
- Deploy a directory or generated in-memory file tree with one function.
- Mount all application HTTP routes on a Hono server without manual path
  parsing or request forwarding.
- Use the ordinary RivetKit client defaults. Examples must not read or forward
  `RIVET_*` environment variables.
- Store submitted files and immutable releases durably in the application
  actor's SQLite database.
- Keep local files disposable. No persistent artifact directory may be required
  for recovery or replica placement.
- Preserve the direct RivetKit actor path. agentOS Apps must never proxy or
  reinterpret DirectActor calls.

## Target API

### Actor setup

`setupApps()` creates actor definitions and nothing else:

```ts
import { setup } from "@rivet-dev/agentos";
import { setupApps } from "@rivet-dev/agentos-apps";

const { appsActors } = setupApps();

export const registry = setup({
  use: {
    ...appsActors,
  },
});

registry.start();
```

The exported map must use explicit, collision-resistant registry keys:

```ts
const appsActors = {
  agentOSAppsApp,
  agentOSAppsScaler,
  agentOSAppsReplica,
};
```

`setupApps()` must not:

- call `setup()` or start a registry;
- create or wrap a RivetKit client;
- construct an HTTP router;
- read Rivet endpoint, token, namespace, or pool environment variables;
- create directories or perform other import-time I/O.

The initial common API should require no options:

```ts
const { appsActors } = setupApps();
```

Actor implementation overrides may be added later under an explicitly advanced
surface. They must not make the default example configure VM permissions,
runtime connection details, namespace provisioning, or artifact storage.

### Deploying an application

`deployApp()` is independent from the value returned by `setupApps()`:

```ts
import { deployApp } from "@rivet-dev/agentos-apps";

const deployment = await deployApp({
  appId: "hello-world",
  source: new URL("../fixtures/app/", import.meta.url),
});
```

It lazily creates an ordinary `createClient()` when no client is supplied.
RivetKit resolves the ordinary request client's endpoint, token, namespace, and
pool defaults. The proof of concept still duplicates the standard connection
variables for its internal namespace control-plane calls; removing that
duplication requires the RivetKit primitive tracked below.

An existing ordinary client can be supplied without creating an agentOS Apps
client or wrapper:

```ts
await deployApp({
  appId: "hello-world",
  source: new URL("../fixtures/app/", import.meta.url),
}, { client });
```

The input supports a local directory for checked examples and an in-memory file
tree for generated applications:

```ts
type DeployAppInput =
	  | {
	      appId: string;
	      source: URL;
	      createNamespace?: boolean;
	      regions?: string[];
	      scaling?: AppScaling;
	    }
	  | {
	      appId: string;
	      files: Record<string, string | Uint8Array>;
	      createNamespace?: boolean;
	      regions?: string[];
	      scaling?: AppScaling;
	    };
```

The result should contain only stable application information:

```ts
interface Deployment {
  appId: string;
  release: string;
  namespace: string;
  pool: string;
  regions: string[];
}
```

`appId` is always a required property. There is no positional application
identifier and no generated default. Public and internal implementation
identifiers must consistently use `appId`; remove ambiguous identifier names
such as `name`, `app`, and `appKey`.

The common-path defaults are:

| Setting | Default |
| --- | --- |
| `regions` | The stable application actor's current Rivet region |
| `scaling.minReplicas` | `0`; active releases scale to zero when idle |
| `scaling.maxReplicas` | `128` replicas per deployed region |
| `scaling.targetConcurrency` | `8` admitted requests per replica |
| Excess replica warm retention | Five minutes |
| RivetKit client | Lazily create the ordinary default client |
| Rivet namespace | Reuse the namespace configured for the ordinary Rivet connection |
| Dependency installation | `npm ci` with a lockfile; otherwise bounded `npm install` |
| Build | Run `npm run build` when the package defines a build script |
| Entrypoint | Infer from `exports`, then `main`, then the documented default |
| Release activation | Boot and verify at least one replica in every requested region before activation, even when `minReplicas` is `0` |

`source` and `files` are mutually exclusive and exactly one is required.
`warmIdleTimeout` and infrastructure limits remain bounded internal or advanced
settings; callers should not need them for a normal deployment.

### Hono routing

HTTP routing is also independent from `setupApps()`:

```ts
import { appsRouter } from "@rivet-dev/agentos-apps";
import { Hono } from "hono";

const server = new Hono();

server.route("/apps", appsRouter);
```

The router uses a lazy ordinary RivetKit client and handles:

- `/:appId` and `/:appId/*`;
- bounded `appId` parsing;
- removal of the mounted application prefix;
- region selection;
- scaler admission and renewable leases;
- bounded request buffering and response streaming;
- cancellation and backpressure;
- hop-by-hop header removal and repeated response headers;
- typed mapping of expected routing errors to HTTP responses.

For a custom client, an advanced adapter may construct the same router without
coupling it to `setupApps()`:

```ts
import { createAppsRouter } from "@rivet-dev/agentos-apps/advanced";

server.route("/apps", createAppsRouter({ client }));
```

There is no public `routeAppRequest()` in the target common API.

### Complete server example

The intended example server is:

```ts
import { serve } from "@hono/node-server";
import { setup } from "@rivet-dev/agentos";
import {
  appsRouter,
  deployApp,
  setupApps,
} from "@rivet-dev/agentos-apps";
import { Hono } from "hono";

const { appsActors } = setupApps();

export const registry = setup({
  use: {
    ...appsActors,
  },
});

registry.start();

await deployApp({
  appId: "hello-world",
  source: new URL("../fixtures/app/", import.meta.url),
});

const server = new Hono();

server.route("/apps", appsRouter);

serve({
  fetch: server.fetch,
  port: 3000,
});
```

Runner registration races must be handled inside agentOS Apps with a bounded
retry for known readiness errors. This retry does not belong in examples.

## Durable Storage

### SQLite is the source of truth

The stable application actor owns the submitted source and built release in its
SQLite database. Actor state should retain only small coordination fields; file
content and artifacts belong in explicit tables.

Proposed logical schema:

```text
app_releases
  release_id
  created_at
  status
  entrypoint
  artifact_hash
  artifact_bytes
  build_error

app_release_files
  release_id
  path
  content
  byte_length

app_release_artifact_chunks
  release_id
  chunk_index
  content
  byte_length
```

All tables and operations must be bounded:

- maximum releases retained per application;
- maximum files and source bytes per release;
- maximum path length;
- maximum individual and aggregate artifact bytes;
- fixed artifact chunk size and maximum chunk count;
- maximum build stdout and stderr retained;
- bounded transactions and batch sizes;
- typed errors naming the violated limit and its configuration.

Deployment must be transactional from the caller's perspective:

1. Validate and normalize every submitted path.
2. Compute the canonical release hash.
3. Insert the release and source files as a non-active building release.
4. Build in a short-lived AgentOS VM.
5. Persist and verify the immutable artifact chunks.
6. Warm the required regional replicas.
7. Atomically make the release active.
8. Leave the previous release active on any failure.

Failed release records may retain bounded diagnostics, but partial artifact
chunks must be removed.

### Local files are disposable

AgentOS currently requires a host path when mounting a packed `.aospkg`. A
replica may therefore materialize an artifact from SQLite into a bounded
temporary file:

```text
application actor SQLite
        |
        | bounded, checksummed chunks
        v
replica-owned temporary .aospkg
        |
        v
AgentOS VM read-only package mount
```

The temporary file:

- is not the durable source of truth;
- is scoped to a replica or bounded cache entry;
- is recreated after process or host restart;
- remains present for the VM's entire lifetime because package reads may be
  lazy;
- is cleaned after the VM and all lazy mount readers are finished;
- must never require a user-configured artifact directory.

Delete `localArtifacts()` and the `.data/apps-artifacts` example directory.
There should be no `.agentos/apps/artifacts` persistent requirement either.

### Replica wake, warm retention, and cleanup

The replica lifecycle is:

```text
wake
  -> stream the active release from application SQLite
  -> write a fresh replica-scoped temporary .aospkg
  -> verify its size and content hash
  -> boot the AgentOS VM
  -> report ready to the scaler

retire, sleep, destroy, or startup failure
  -> stop accepting new leases
  -> drain bounded in-flight requests
  -> stop and dispose the VM
  -> delete the temporary .aospkg and its directory
```

Cleanup must run in `finally` on every terminal path. A failed cleanup must be
logged and retried or returned as a typed error; it must not be swallowed. A
subsequent wake always creates a new temporary path and never trusts a leftover
file from a previous VM.

Warm retention and actor sleep are separate policies:

- `warmIdleTimeout` controls how long the scaler keeps an excess replica hot
  after its last lease;
- the configured minimum replica count remains hot indefinitely;
- the actor sleep grace period only controls the actor lifecycle and cleanup
  window. It is not the warm-pool autoscaling policy.

The current 30-second scale-down delay is too aggressive for a VM that must
rehydrate an npm application, initialize V8, and boot its HTTP server. Use a
five-minute default `warmIdleTimeout` for excess replicas, while keeping it an
advanced bounded setting rather than common setup configuration.

`minReplicas` defaults to `0`. A deployment still boots and health-checks one
replica in every requested region before activating the release. That verified
replica remains warm until the normal idle timeout and can then retire, leaving
the active release at zero replicas. The next request rehydrates the artifact
from SQLite and cold-starts a replica.

Replicas currently opt out of automatic actor sleep, so the scaler must
explicitly retire and destroy excess replicas after the warm idle timeout.
This preserves an accurate distinction between ready, warm replicas and
nonexistent replicas. If replicas later use engine-driven sleep, a sleeping
replica must first be removed from the scaler's ready set and must complete the
full wake-and-readiness sequence before receiving another request.

### Scaler capacity warning

Each regional scaler must emit a host-visible warning when its provisioned
replica count transitions from at or below 50% to above 50% of
`scaling.maxReplicas`. Count both ready and warming replicas so concurrent
scale-up cannot hide approaching capacity.

For the default `maxReplicas: 128`, the warning is emitted when the count first
reaches `65`. It is transition-based rather than request-based: latch the
warning while usage remains above 50%, clear the latch after usage returns to
50% or below, and warn again only after a later upward crossing.

The structured warning must include `appId`, release, region, ready replicas,
warming replicas, `maxReplicas`, and the utilization percentage. It must name
the limit and explain how to raise it. Reaching the warning threshold does not
reject traffic or force another scale-up by itself.

### Retention and garbage collection

When an inactive release exceeds the configured retention count:

1. Drain its regional scalers and replicas.
2. Confirm no replica still references its artifact.
3. Delete its artifact chunks.
4. Delete its source files.
5. Delete its release metadata.

Cleanup failures must be logged and retried. They must not be silently ignored.

## Namespace and Runtime Plumbing

The common path reuses the namespace configured for the ordinary Rivet
connection and makes no namespace-management request. Callers may set
`createNamespace: true` to idempotently create a stable, isolated namespace
for `appId` within the configured host namespace.

agentOS Apps must internally:

1. Resolve the namespace from the ordinary Rivet connection by default.
2. When opted in, derive and idempotently create a namespace deterministic for
   `appId` within the configured host namespace.
3. Configure a stable agentOS Apps guest runner pool derived from `appId`.
4. Mint or resolve credentials scoped to that namespace and runner connection.
5. Inject only the namespace, endpoint, pool, scoped credential, and monotonic
   release version into the guest process.
6. Keep management credentials out of actor state, SQLite, artifacts, logs, and
   guest-visible environment variables.

Remove these common API concepts:

- `rivetNamespaceProvisioner()`;
- the `provision` callback;
- the `runtime()` callback;
- `AppRuntimeConfig`;
- manual endpoint, token, namespace, and pool configuration.

If current RivetKit APIs cannot resolve default client configuration or create a
scoped runner credential without duplicating environment parsing, add the
necessary primitive to RivetKit. Do not keep the callback-based public API as a
workaround.

## Source Build and Package Conventions

For `{ source: URL }`, recursively load the directory with these rules:

- accept only a `file:` directory URL;
- reject symlinks, devices, sockets, and paths escaping the root;
- enforce file-count, individual-file, total-byte, and path-length bounds while
  reading;
- preserve empty files and binary static assets;
- sort normalized paths before hashing and upload;
- ignore only a documented fixed set of local build artifacts;
- never follow a user `.gitignore` implicitly.

The in-memory API accepts byte values for static assets:

```ts
files: Record<string, string | Uint8Array>
```

Dependency installation and compilation happen once per immutable release in a
short-lived AgentOS build VM. They never run in the trusted host process and
never run independently on every serving replica:

```text
deployApp()
  -> validate and normalize the submitted source
  -> persist the source in the application actor's SQLite
  -> start an isolated, bounded AgentOS build VM
  -> materialize the source into the build VM workspace
  -> install dependencies
  -> run the build, if present
  -> resolve and smoke-test the HTTP entrypoint or static output
  -> prune build-only dependencies
  -> pack source, output, and runtime node_modules into one .aospkg
  -> stream checksummed artifact chunks into application SQLite
  -> destroy the build VM and its temporary filesystem
```

Package behavior is inferred from `package.json`:

1. Use `npm ci` when `package-lock.json` exists.
2. Otherwise use bounded `npm install` and retain the generated lockfile with
   the immutable release.
3. Run lifecycle scripts only inside the untrusted build VM. The VM receives no
   host secrets and has bounded CPU, memory, filesystem, process, output,
   network, and wall-clock limits.
4. Run `npm run build` when a build script is present.
5. Resolve a server entrypoint from `package.json.exports`, then
   `package.json.main`, then the documented source default.
6. If there is no server entrypoint but the build produced `dist/index.html`,
   package `dist/` with the agentOS Apps static HTTP entrypoint.
7. If there is no `package.json` and the submitted root contains `index.html`,
   package the submitted tree as a static website without installing modules.
8. Fail with a typed error when the server/static mode or entrypoint is
   ambiguous.

After a server build, remove development-only dependencies while retaining
runtime dependencies. The resulting `.aospkg` contains the application and its
runtime `node_modules`, so ordinary Node package resolution works inside every
replica without another install. This is also how a guest application imports
the published `rivetkit` npm package.

Native Node addons are not silently accepted when the AgentOS JavaScript runtime
cannot load them. Installation or the smoke test must return a typed unsupported
module error naming the package. Pure JavaScript and WebAssembly packages use
normal package resolution.

The first implementation should not add a shared mutable `node_modules` cache.
An identical immutable release may reuse its verified artifact; otherwise each
release receives a clean build VM. Build logs, artifact size, dependency count,
process count, network destinations, and build duration are all bounded and
reported through typed deployment errors.

The common deployment API does not require `entrypoint`, `buildCommand`,
artifact paths, install commands, or VM options. Advanced explicit overrides
can be considered only when real applications prove these conventions
insufficient.

## Actor Changes

- Rename the stable application actor registry key to `agentOSAppsApp`.
- Rename the regional scaler actor registry key to `agentOSAppsScaler`.
- Rename the execution replica actor registry key to `agentOSAppsReplica`.
- Return those definitions from `setupApps()` as `appsActors`.
- Keep the actors infrastructure-only; users do not call them for guest actor
  actions.
- Replace artifact-path metadata with SQLite release and artifact references.
- Add bounded artifact chunk read actions used only by execution replicas.
- Preserve renewable admission leases and scale-to-zero behavior.
- Preserve monotonic serverless runner versions across releases.
- Keep rollout preparation idempotent across actor retries and process restarts.
- Ensure failed new releases retire every partially created scaler and replica.

## Client and DirectActor Behavior

agentOS Apps does not export a client and does not wrap `createClient()`.

Guest actors are called through ordinary RivetKit:

```ts
import { createClient } from "rivetkit/client";

const deployment = await deployApp({ appId: "hello-world", source });
const client = createClient({
  namespace: deployment.namespace,
  poolName: deployment.pool,
});
```

The guest application namespace returned by `deployApp()` is used with the
ordinary DirectActor API when an explicit namespace is required. Calls travel:

```text
RivetKit client
  -> Rivet Engine
  -> serverless callback through agentOSAppsApp
  -> regional scaler admission
  -> guest RivetKit registry in a warm AgentOS VM
  -> guest actor
```

This remains the ordinary DirectActor protocol: agentOS Apps does not wrap the
client or reinterpret actions. Rivet's serverless callback travels through
`agentOSAppsApp` and the regional scaler so actor demand can wake a replica
from zero. It does not require the user-facing Hono router.

## Public API Changes

Target common exports:

```ts
export {
  AgentOSAppsError,
  appsRouter,
  deployApp,
  setupApps,
  type AppScaling,
  type DeployAppInput,
  type Deployment,
};
```

Remove from the common public surface:

```text
agentOSApps
localArtifacts
rivetNamespaceProvisioner
routeAppRequest
AgentOSAppsRoutingClient
AppRuntimeConfig
ArtifactStore
LocalArtifactsOptions
```

This repository does not guarantee protocol or API backward compatibility.
Update examples and documentation directly rather than carrying two competing
public APIs.

## Flat Examples

Replace the combined `examples/apps/` project with standalone directories:

```text
examples/
  apps-hello-world/
  apps-sqlite/
  apps-workflows/
  apps-multiplayer/
  apps-static-website/
  apps-ai-builder/
```

Each example uses:

```text
package.json
tsconfig.json
src/
  server.ts
fixtures/
  app/
```

Rules:

- trusted host and server code lives in `src/`;
- uploaded application code and static assets live in `fixtures/`;
- examples export `const registry = setup(...)` and call `registry.start()` on a
  later statement;
- examples spread `{ ...appsActors }` into `use`;
- examples do not read `RIVET_*` variables;
- examples do not contain local runner-readiness retry loops;
- examples do not contain persistent artifact directories;
- directory examples do not need `files.ts`.

Move comprehensive validation and load tooling out of beginner examples:

```text
tests/e2e/agentos-apps/
benchmarks/agentos-apps/
```

### Hello World

Demonstrate only actor setup, directory deployment, Hono mounting, and one
response.

### SQLite

Demonstrate application data persisted through RivetKit actor SQLite. Verify
that data survives request routing to a different agentOS Apps execution
replica.

### Workflows

Demonstrate a durable RivetKit workflow defined inside the deployed application,
including starting it over HTTP, observing progress, and resuming after an
execution replica is replaced.

### Multiplayer

Demonstrate a RivetKit multiplayer actor with multiple connected clients,
shared state, reconnect behavior, and execution-replica replacement. Keep the
example focused on the application API rather than load-generation machinery.

### Static Website

Demonstrate deploying HTML, CSS, JavaScript, and binary assets without a
`package.json`. Also document the `dist/index.html` convention for a built
static site.

### AI App Builder

Tie the complete flow together with the Vercel AI SDK: generate a bounded source
tree, deploy it, feed bounded TypeScript/build diagnostics back to the agent,
repair it, and activate only a successful release.

Use the Vercel AI SDK on the trusted host:

```text
prompt
  -> coding agent edits bounded in-memory files
  -> deployApp() runs the real TypeScript build
  -> bounded diagnostics return to the agent
  -> agent repairs the files
  -> successful immutable release activates
```

The host, not the model, decides whether the workflow is complete. Require a
successful deployment, cap model steps and repair attempts, limit editable
paths, and retain the previous valid release during failed iterations.

## Implementation Order

### 1. Lock the public contract

- [x] Add type-level tests for the exact `setupApps()` example.
- [x] Add type-level tests for directory and in-memory `deployApp()` calls.
- [x] Change `deployApp()` to one object input with a required `appId`; remove
      the positional application identifier.
- [x] Rename every application identifier field, variable, actor input, route
      parameter, error, example, and result to `appId`; remove identifier uses
      of `name`, `app`, and `appKey`.
- [x] Add tests for all `deployApp()` defaults and partial scaling overrides.
- [x] Change the default `scaling.minReplicas` to `0` and test idle
      scale-to-zero followed by a successful cold wake.
- [x] Change the default `scaling.maxReplicas` to `128` per region and replace
      the current hard maximum of `64` with a bounded platform limit that
      permits at least the default.
- [x] Add a latched structured warning when a regional scaler crosses above
      50% of `scaling.maxReplicas`, counting ready and warming replicas.
- [x] Test that the capacity warning fires once per upward crossing, rearms
      after returning to 50% or below, and includes the required metadata.
- [x] Add a Hono mounting test for `appsRouter`.
- [x] Add a test proving the registry keys are exactly `agentOSAppsApp`,
      `agentOSAppsScaler`, and `agentOSAppsReplica`.
- [x] Add a test proving `setupApps()` performs no I/O or client creation.

### 2. Move releases into actor SQLite

- [x] Design and migrate the release, file, and artifact-chunk tables.
- [x] Persist normalized source files before starting a build.
- [x] Stream build artifacts into bounded SQLite chunks.
- [x] Verify artifact length and content hash before marking a build ready.
- [x] Rehydrate an artifact into a replica-owned temporary file.
- [x] Keep the temporary artifact for the VM lifetime, then delete it after VM
      disposal on retire, sleep, destroy, startup failure, and runtime error.
- [ ] Make cleanup retryable and observable; never reuse an unverified leftover
      artifact on wake.
- [x] Replace the 30-second scale-down delay with a five-minute
      `warmIdleTimeout` default for excess replicas.
- [x] Prove configured minimum replicas remain warm while excess replicas
      retire after the configured timeout.
- [x] Prove deployment verifies one replica per region before activation even
      when the configured minimum is zero.
- [x] Prove a cold start succeeds after deleting all local temporary data.
- [x] Implement release retention and garbage collection retried by later
      deployments.
- [x] Remove the local artifact store implementation and configuration.

### 3. Internalize namespace and runner setup

- [x] Replace provisioning callbacks with internal idempotent namespace setup.
- [x] Make namespace creation opt-in and reuse the configured namespace by
      default.
- [ ] Reuse RivetKit's default connection configuration.
- [ ] Add a RivetKit primitive if default config is not safely reusable.
- [x] Configure a stable per-app guest runner pool automatically.
- [ ] Create namespace-scoped guest credentials without exposing management
      credentials.
- [x] Move runner-readiness retries into bounded internal deployment logic.
- [x] Delete the public runtime and provisioning APIs.

### 4. Implement the simple deployment facade

- [x] Add bounded directory loading.
- [x] Support binary in-memory files.
- [x] Run dependency installation, lifecycle scripts, builds, pruning, and
      entrypoint smoke tests only inside a bounded short-lived build VM.
- [x] Infer install, build, server entrypoint, and static output behavior from
      the submitted tree and `package.json`.
- [x] Pack runtime dependencies into the immutable artifact so replicas never
      install modules on wake.
- [x] Return a typed error for unsupported native Node addons.
- [x] Support package-free static trees rooted at `index.html` and built static
      output rooted at `dist/index.html`.
- [x] Preserve content-addressed, deterministic release hashing.
- [x] Let callers optionally pass an ordinary RivetKit client.
- [x] Lazily create the default client without import-time side effects.
- [x] Return stable deployment information and typed build errors.

### 5. Implement the Hono router

- [x] Add the `/:appId` and `/:appId/*` routes.
- [x] Strip the mount prefix correctly.
- [x] Preserve response streaming, cancellation, backpressure, and repeated
      headers. Request bodies remain bounded and buffered.
- [x] Select regions without maintaining edge-local placement state.
- [x] Use a lazy default client.
- [x] Provide custom-client construction only in the advanced surface.
- [x] Remove public `routeAppRequest()`.

### 6. Rewrite examples and documentation

- [x] Replace `examples/apps/` with the flat examples.
- [x] Add focused Hello World, SQLite, Workflows, Multiplayer, and Static
      Website examples and corresponding documentation.
- [x] Move E2E verification to `tests/e2e/agentos-apps/`.
- [x] Move the load driver to `benchmarks/agentos-apps/`.
- [x] Add the AI SDK generate, type-check, repair, and deploy example.
- [x] Rewrite the package README around the target API.
- [x] Rewrite the website Apps page in this order: product overview, checked
      Hello World quick start, application structure, deployment, builds and
      dependencies, HTTP routing, SQLite and RivetKit persistence, scaling and
      cold starts, regions and isolation, examples, API reference, and current
      limitations.
- [x] Lead the website page with the deployable user API; keep scaler,
      namespace, artifact, and runner internals after the quick start.
- [x] Source every runnable website snippet from the checked flat examples
      through the docs theme `<CodeSnippet>` mechanism.
- [x] Include the deployment defaults table, build pipeline, disposable-replica
      versus durable-SQLite diagram, request routing diagram, scale-to-zero
      lifecycle, and 50% scaler-capacity warning.
- [x] Link and briefly describe the Hello World, SQLite, Workflows, Multiplayer,
      Static Website, and AI App Builder examples without duplicating their
      complete READMEs.
- [x] Document the ordinary DirectActor path without an agentOS Apps client
      proxy.
- [x] Update the main agentOS Apps design wherever the old artifact-store and
      routing APIs appear.
- [x] Remove obsolete environment-variable and artifact-directory guidance.

### 7. Validate the complete behavior

- [x] Run package unit tests and type checks.
- [x] Run `cargo check --workspace`.
- [ ] Run `pnpm build` and `pnpm check-types`.
- [x] Run fixed-version and publish-helper checks.
- [x] Build the website.
- [x] Run a real RivetKit guest from a packed npm dependency tree.
- [x] Verify DirectActor state survives replica replacement.
- [x] Verify deployment recovery with an empty local filesystem.
- [x] Verify failed TypeScript builds return bounded diagnostics and do not
      replace the active release.
- [x] Verify abandoned HTTP requests recover through admission lease expiry.
- [x] Run the bounded load test and record cold-start and warm-request latency.

## Completion Criteria

The simplification is complete when a new user can understand the hello-world
server without learning about artifacts, runtime callbacks, namespace
provisioners, route forwarding, or Rivet environment variables; the same
implementation must still recover every deployed release from actor SQLite and
run real RivetKit actors through the ordinary DirectActor API.
