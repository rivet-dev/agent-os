# agentOS Apps

agentOS Apps deploys user-generated JavaScript and static sites into isolated
agentOS VMs and routes HTTP through Rivet Actors.

Install agentOS Apps in a Node.js 22 or newer project:

```sh
npm add @rivet-dev/agentos @rivet-dev/agentos-apps
npm add @hono/node-server hono
npm add --save-dev tsx
npm pkg set type=module
```

RivetKit starts a local Engine automatically. For an existing Rivet deployment,
use its standard Rivet connection variables and credentials.

`src/actors.ts`:

```ts
import { setup } from "@rivet-dev/agentos";
import { setupApps } from "@rivet-dev/agentos-apps";

const { appsActors } = setupApps();

export const registry = setup({
	use: {
		...appsActors,
	},
});
```

`src/server.ts`:

```ts
import { serve } from "@hono/node-server";
import { appsRouter } from "@rivet-dev/agentos-apps";
import { Hono } from "hono";
import { registry } from "./actors.js";

registry.start();

const server = new Hono();
server.route("/apps", appsRouter);

serve({ fetch: server.fetch, port: 3000 });
```

`src/deploy.ts`:

```ts
import { deployApp } from "@rivet-dev/agentos-apps";

await deployApp({
	appId: "hello-world",
	files: {
		"index.html": "<h1>Hello from agentOS Apps</h1>",
	},
});
```

The common API has three primary entry points:

- `setupApps()` returns the `agentOSAppsApp`, `agentOSAppsScaler`, and
  `agentOSAppsReplica` actor definitions.
- `deployApp({ appId, source | files })` builds and activates an immutable
  release. It lazily uses an ordinary RivetKit client.
- `appsRouter` routes `/:appId` and `/:appId/*` to deployed applications.

It also exports the deployment input, result, scaling, and typed error types.
Supplying a custom ordinary client remains an option on `deployApp()` rather
than a separate Apps client abstraction.

Submitted source and packed release chunks are stored in the stable app actor's
SQLite database. Serving replicas materialize a verified temporary `.aospkg`
for the VM lifetime and delete it after VM disposal. No durable local artifact
directory is required.

Dependencies and build tools exist only inside a disposable build VM. The
platform-owned Apps builder emits a minimal release containing bundled
JavaScript, imported WASM modules, and static assets; serving replicas never
install packages and releases do not contain tenant `node_modules`.

Deployments use the ordinary Rivet connection's configured namespace by
default and do not require namespace-management permission. Set
`createNamespace: true` on `deployApp()` to idempotently create a stable,
isolated namespace for that `appId` within the configured host namespace. Every
deployment returns its stable `pool` along with its `namespace` for ordinary
DirectActor clients.

The `scaling` options default to `minReplicas: 0`, `maxReplicas: 128`, and
`targetConcurrency: 8`.

Guest Rivet Actors use the ordinary DirectActor API from `rivetkit/client`.
agentOS Apps does not export or wrap a RivetKit client. Host management tokens
are never exposed inside guest VMs. Each VM receives an opaque, app-scoped
Engine capability that fixes the namespace and runner pool and rejects
management routes. Rivet Engine callbacks use a random per-app credential that
the trusted app actor validates and strips before forwarding.

See `examples/apps-hello-world` for the smallest runnable server.
