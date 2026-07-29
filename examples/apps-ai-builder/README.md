# agentOS Apps: AI App Builder

This trusted host uses the Vercel AI SDK to generate a bounded three-file
RivetKit application. `deployApp()` runs the real TypeScript build in an
agentOS VM. Bounded diagnostics are fed back to the model for at most three
repairs, and a failed build never replaces the previous active release.

Run the checked workspace example with Node.js 22 or newer:

```sh
ANTHROPIC_API_KEY=... pnpm --dir examples/apps-ai-builder start
# In another terminal:
curl -X POST http://localhost:3000/deploy/ai-generated-app \
  -H 'content-type: application/json' \
  -d '{"prompt":"Build a collaborative counter"}'
```

RivetKit starts its local Engine automatically. Against an existing Rivet
deployment, use the standard Rivet connection variables instead. The successful app is mounted at
`http://localhost:3000/apps/ai-generated-app`.
