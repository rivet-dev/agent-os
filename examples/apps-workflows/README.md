# agentOS Apps: Workflows

The deployed server defines a keyed `job` actor and an HTTP handler. The
separate `src/client.ts` deploys it, connects with the returned namespace and
pool, and creates a durable workflow through DirectActor.

Run the checked workspace example with Node.js 22 or newer:

```sh
pnpm --dir examples/apps-workflows start
# In another terminal:
pnpm --dir examples/apps-workflows client
```

RivetKit starts its local Engine automatically. Against an existing Rivet
deployment, use the standard Rivet connection variables instead. The deployed
HTTP handler is available at
`http://localhost:3000/apps/durable-workflow`.
