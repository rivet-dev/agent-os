# agentOS Apps: SQLite

The deployed server defines a Rivet Actor backed by SQLite and still serves
HTTP. The separate `src/client.ts` deploys it, connects with the returned
namespace and pool, and adds and lists notes through DirectActor.

Run the checked workspace example with Node.js 22 or newer:

```sh
pnpm --dir examples/apps-sqlite start
# In another terminal:
pnpm --dir examples/apps-sqlite client
```

RivetKit starts its local Engine automatically. Against an existing Rivet
deployment, use the standard Rivet connection variables instead.

The deployed HTTP handler is available at
`http://localhost:3000/apps/sqlite-notes`.
