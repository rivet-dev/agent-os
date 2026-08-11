# agentOS Apps

Status: implemented proof of concept.

agentOS Apps is a multi-tenant application runner built from Rivet Actors and
AgentOS VMs. A deployment may be a static directory, a JavaScript HTTP handler,
or a complete RivetKit registry. The platform builds code once, stores the
immutable release in actor SQLite, and serves it from region-local warm
replicas.

## Public API

```ts
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

server.route("/apps", appsRouter);
```

`setupApps()` only constructs definitions. `deployApp()` and `appsRouter` use
ordinary RivetKit clients lazily and are independent of that return value.
There is no agentOS Apps client proxy. Guest actor actions use DirectActor
through `rivetkit/client`.

The registry keys are:

```text
agentOSAppsApp       stable deployment coordinator and SQLite owner
agentOSAppsScaler    one autoscaler per release and region
agentOSAppsReplica   one warm AgentOS VM serving one immutable release
```

## Request path

```text
Hono /apps/:appId/* forwards one request
        |
        v
stable app actor resolves active release and region
        |
        v
regional scaler acquires a renewable admission lease
        |
        v
warm execution replica
        |
        v
AgentOS VM -> generated Node HTTP adapter -> user fetch(request)
```

The edge keeps no placement cache. The regional scaler is the serialization
point for replica membership, active request counts, lease expiry, scale-up,
and scale-down. Responses stream from the VM. Request bodies are bounded and
currently buffered before entering the VM.

## Durable release path

```text
source URL or in-memory files
        |
        v
stable app actor SQLite
  release metadata
  normalized source BLOBs
        |
        v
short-lived bounded AgentOS build VM
  npm install or npm ci
  optional npm run build
  npm prune --omit=dev
  native-addon check
  deterministic package
        |
        v
checksummed SQLite artifact chunks
        |
        v
replica-scoped temporary .aospkg
        |
        v
read-only /app mount in serving VM
```

SQLite is the durable source of truth. A replica creates a fresh temporary
package, validates its byte count and SHA-256 hash, keeps it while lazy mount
readers may exist, and removes it only after the VM is disposed. Host restart
or replica replacement rehydrates from SQLite and requires no shared local
filesystem.

A new release is activated only after one replica is healthy in every selected
region. Failed builds and failed regional rollouts leave the previous release
active. Inactive releases are retired and removed only after their replicas
have drained.

## Build conventions

- A lockfile selects `npm ci`; otherwise the build VM runs bounded
  `npm install`.
- A package `build` script runs automatically.
- Server entrypoints resolve from `exports`, then `main`, then
  `src/index.mjs`, `src/index.js`, `index.mjs`, or `index.js`.
- A build with no server entrypoint serves `dist/index.html`.
- A package-free tree containing root `index.html` is served as a static site.
- Production dependencies remain inside the immutable package. Replicas never
  reinstall modules.
- `.node` native addons fail with a typed unsupported-addon error.

The generated Node adapter initializes the packaged RivetKit WASM bytes, loads
user code after the guest Rivet environment is installed, and mounts the
exported registry's serverless handler at `/api/rivet`. Guest code remains
ordinary RivetKit code and may keep its normal `registry.start()` statement;
the managed loader suppresses that standalone listener during module import
because the adapter already owns the VM's HTTP listener.

## Scaling

Defaults apply per deployed region:

| Setting | Default |
| --- | --- |
| Minimum replicas | `0` |
| Maximum replicas | `128` |
| Target concurrency | `8` admitted requests per replica |
| Excess warm retention | Five minutes |
| Admission lease | 60 seconds, renewed while the response is active |

`acquire()` uses projected concurrency to begin warming the next replica before
assigning the current request. Selection is least-loaded with round-robin
tie-breaking. Abandoned requests recover when their leases expire.

Scale-down removes at most 25% of the current replica set per reconciliation.
Configured minimum replicas remain hot. Excess replicas remain warm for five
minutes after their last lease, then retire. With the default minimum, the pool
eventually reaches zero and the next request performs a checked cold wake.

A scaler emits a structured warning whenever ready plus warming replicas cross
from at or below 50% to above 50% of `maxReplicas`. The warning is latched until
capacity returns to 50% or lower.

## Regions and namespaces

By default each app uses the namespace already configured for its ordinary
Rivet connection and agentOS Apps performs no namespace-management requests.
`createNamespace: true` idempotently provisions a namespace deterministic for
the `appId` within the configured host namespace. The opt-in path requires
namespace list/create permission and is recommended when multiple RivetKit apps
need independent runner configuration or tenants need namespace isolation.

Both paths derive a stable runner pool from `appId`, preventing deployments in
one namespace from overwriting each other's runner configuration. A shared
namespace remains one actor identity and persistence domain; it is not tenant
isolation. Deployments default to the stable app actor's current region and may
request several regions. Scalers and replicas are created in their respective
regions.

The current RivetKit package does not expose its resolved client connection or
a self-hosted namespace-scoped credential minting primitive. The host therefore
gives each VM an opaque capability URL on a loopback Engine proxy. The proxy
fixes the namespace and runner pool, supplies the host credential only on the
trusted side, rejects management routes, and verifies direct actor IDs belong
to the app namespace. Neither the management endpoint nor its token enters the
guest. Runner callbacks use a random per-app credential so old and new replicas
remain valid during a rollout; the stable app actor validates and strips it,
together with host authorization headers, before forwarding.

## DirectActor

HTTP and actor traffic both use the scaler path:

```text
ordinary RivetKit client
        |
        v
Rivet Engine
        |
        v
serverless runner callback
        |
        v
app actor -> regional scaler -> warm execution VM
        |
        v
guest RivetKit serverless registry
        |
        v
guest Rivet Actor
```

DirectActor state and SQLite belong to Rivet, not to a replica filesystem, and
survive replacement. The guest namespace's runner configuration points through
Rivet's query gateway to the stable app actor, so DirectActor callbacks acquire
and renew ordinary scaler admissions. DirectActor traffic therefore wakes a
replica from zero without wrapping or replacing the ordinary RivetKit client.

## Limits and follow-up work

File count, source bytes, individual paths, dependencies, build output,
artifact bytes and chunks, regions, admissions, request bodies, response
bodies, timeouts, and retained releases are bounded.

Remaining production work is:

- replace the host capability proxy when RivetKit provides native scoped guest
  credentials;
- stream request bodies into the VM instead of bounded buffering;
- add CPU-time enforcement to complement existing build resource limits;
- move release garbage collection to a retryable scheduled job rather than
  retrying during later deployments.
