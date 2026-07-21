# agentOS Apps

Deploy user-generated applications in agentOS VMs.

agentOS Apps runs user-generated HTTP applications on Rivet. Apps can add
durable SQLite state, workflows, multiplayer and realtime state, queues, and
cron jobs.

agentOS Apps is in preview and its API is subject to change.

## Architecture

**agentOS Apps is a library, not a hosted AI-generated app deployment
platform.** Unlike managed platforms, you can deploy it anywhere and customize
the server, routing, authentication, and deployment flow.

Requests reach your Hono server, where `appsRouter` routes them to a prewarmed
agentOS VM serving the generated application. Rivet handles request routing and
orchestrates the pool of prewarmed VMs.

      <text x="50" y="50" text-anchor="middle" dominant-baseline="central" font-family="var(--sl-font)" font-weight="700" font-size="38" fill="#1b1916">OS</text>

  <text x="75" y="91" text-anchor="middle" font-family="var(--sl-font)" font-size="14" font-weight="600" fill="#1b1916">Request</text>
  <text x="75" y="110" text-anchor="middle" font-family="var(--sl-font)" font-size="10.5" fill="#56524a">Agent · Browser · API</text>

  <text x="206" y="65" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Your Hono server</text>
  <text x="277" y="99" text-anchor="middle" font-family="var(--sl-font)" font-size="12" font-weight="600" fill="#1b1916">appsRouter</text>
  <text x="277" y="116" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">route the request</text>

  <text x="388" y="82" text-anchor="middle" font-family="var(--sl-font)" font-size="10" fill="#56524a">Rivet</text>

  <text x="432" y="65" font-family="var(--sl-font)" font-size="13" font-weight="600" fill="#1b1916">Prewarmed VM</text>
  <text x="485" y="99" font-family="var(--sl-font)" font-size="11.5" font-weight="600" fill="#1b1916">AI-generated app</text>
  <text x="485" y="116" font-family="var(--sl-font)" font-size="10" fill="#56524a">serves the response</text>

## Quickstart

[View the complete Quickstart example on GitHub](https://github.com/rivet-dev/agentos/tree/main/examples/apps-hello-world).

```sh
npm add @rivet-dev/agentos @rivet-dev/agentos-apps
npm add @hono/node-server hono
npm add --save-dev tsx
npm pkg set type=module
```

Setup the HTTP server that will serve requests for AI-generated apps. Also set
up the actors that power the deployments.

Run the server:

```sh
npx tsx src/server.ts
```

Pass generated files directly to `deployApp()`. This can be called by an agent,
an upload endpoint, or any other part of your system:

```sh
npx tsx src/deploy.ts
```

Open `http://localhost:3000/apps/hello-world/`. Pass this URL to agents,
frontends, or any other part of your system that needs to visit the deployment.

Deploy the host server to any supported target:

See [Deployment](/docs/deployment) for managed and self-hosted options.

## Deploy App Reference

Deploy a directory:

```ts
await deployApp({
  appId: "hello-world",
  source: new URL("../fixtures/app/", import.meta.url),
});
```

Or deploy generated files:

```ts
await deployApp({
  appId: "generated-app",
  files: {
    "index.html": "<h1>Hello</h1>",
  },
});
```

### TypeScript repair

`deployApp()` returns build diagnostics when generated TypeScript does not
compile. An agent can use those diagnostics to repair the files and call
`deployApp()` again:

```ts
for (let attempt = 0; attempt < 3; attempt++) {
  try {
    await deployApp({ appId: "generated-app", files });
    break;
  } catch (error) {
    if (attempt === 2) throw error;
    files = await repairWithAgent(files, String(error));
  }
}
```

A failed build does not replace the currently active release. See the
[AI App Builder example](https://github.com/rivet-dev/agentos/tree/main/examples/apps-ai-builder).

`appId` must contain 1–63 lowercase letters, numbers, or hyphens. Pass exactly
one of `source` or `files`.

### Configuration

```ts
await deployApp({
  appId: "my-app",
  source,
  regions: ["atl", "fra"],
  createNamespace: true,
  scaling: {
    minReplicas: 0,
    maxReplicas: 128,
    targetConcurrency: 8,
  },
});
```

| Option | Default |
| --- | --- |
| `regions` | Current Rivet region |
| `createNamespace` | `false` |
| `scaling.minReplicas` | `0` |
| `scaling.maxReplicas` | `128` |
| `scaling.targetConcurrency` | `8` |

By default, apps use the namespace configured on the ordinary Rivet client.
Enable `createNamespace` only when the app needs its own namespace; it requires
Rivet namespace list and create permissions.

## Route requests

Mount all deployed apps:

```ts
server.route("/apps", appsRouter);
```

This routes `/apps/:appId` and `/apps/:appId/*`. To use an explicit RivetKit
client:

```ts
import { createAppsRouter } from "@rivet-dev/agentos-apps/advanced";

server.route("/apps", createAppsRouter({ client }));
```

## Add Capabilities to Apps

Agents can generate more than pages and REST APIs. These examples show apps
with durable SQLite data, workflows, multiplayer state, queues, and scheduled
jobs. The server snippets represent AI-generated app code; the client snippets
show how another part of your system connects to it.

### SQLite

Example AI-generated app code that stores durable data in an actor-owned SQLite
database. [View the complete SQLite example](https://github.com/rivet-dev/agentos/tree/main/examples/apps-sqlite).

### Workflows

Example AI-generated app code that runs durable multi-step jobs that can sleep
and resume. [View the complete workflows example](https://github.com/rivet-dev/agentos/tree/main/examples/apps-workflows).

### Multiplayer

Example AI-generated app code that shares realtime state between clients.
[View the complete multiplayer example](https://github.com/rivet-dev/agentos/tree/main/examples/apps-multiplayer).

### Queues

AI-generated apps can use actor queues for durable background work and ordered
processing.

### Cron jobs

AI-generated apps can schedule recurring work from an actor. See
[Cron Jobs](/docs/cron).

These capabilities use RivetKit and its ordinary DirectActor client. agentOS
Apps does not wrap the client.

## Build Apps with AI

Give an agent the app requirements, let it generate the project files, and pass
those files to `deployApp()`. If the build returns TypeScript diagnostics, give
them back to the agent and deploy its repaired files again.

[View the complete AI App Builder example](https://github.com/rivet-dev/agentos/tree/main/examples/apps-ai-builder).

## Authentication

Use normal Hono middleware to authenticate requests before they reach deployed
apps:

```ts
server.use("/apps/*", authMiddleware);
server.route("/apps", appsRouter);
```

## Planned Improvements

- Automatically include agent skills based on the
  [Rivet Cookbooks](https://rivet.dev/cookbook/) for better app generation.
- Billing API for tracking and charging for app usage.
- Built-in error reporting for generated apps.