# agentOS Apps: Multiplayer

The deployed server defines keyed room actors and an HTTP handler. The separate
`src/client.ts` deploys it, connects with the returned namespace and pool, then
joins and moves through DirectActor.

Run the checked workspace example with Node.js 22 or newer:

```sh
pnpm --dir examples/apps-multiplayer start
# In another terminal:
pnpm --dir examples/apps-multiplayer client
```

RivetKit starts its local Engine automatically. Against an existing Rivet
deployment, use the standard Rivet connection variables instead.

The deployed HTTP handler is available at
`http://localhost:3000/apps/multiplayer-room`.
