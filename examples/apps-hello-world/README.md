# agentOS Apps: Hello World

This smallest example registers the three infrastructure actors, mounts the
Apps router, and deploys generated files from a separate script. The app runs at
`http://localhost:3000/apps/hello-world/`.

The uploaded fixture serves an HTML page at `/` and a JSON endpoint at
`/api/hello`. It does not use RivetKit because it has no durable or coordinated
state; the SQLite, workflows, and multiplayer examples add RivetKit while still
serving ordinary HTTP requests.

Run the checked workspace example with Node.js 22 or newer:

```sh
pnpm --dir examples/apps-hello-world start
# In another terminal:
pnpm --dir examples/apps-hello-world deploy
```

agentOS Apps starts its local Rivet Engine automatically. Against an existing
Rivet deployment, use the standard Rivet connection variables instead.
