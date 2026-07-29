# Flue

Use agentOS as the durable sandbox backend for Flue.

Flue owns the agent runtime and session lifecycle. Rivet maps each agent instance and workflow run to a durable Rivet Actor, while agentOS gives each Flue context an isolated VM with a persistent `/workspace` filesystem.

[View the complete example →](https://github.com/rivet-dev/agentos/tree/main/examples/flue)

## Quickstart

```sh
mkdir my-agent && cd my-agent
npm init -y
npm pkg set type=module

# Install the Flue packages
npm add "@flue/runtime@npm:@rivet-dev/labs-flue-runtime@1.0.0-beta.9-rivet.2"
npm add --save-dev "@flue/cli@npm:@rivet-dev/labs-flue-cli@1.0.0-beta.9-rivet.2"

# Install the Rivet packages
npm add @rivet-dev/flue @rivet-dev/agentos @rivet-dev/agentos-flue rivetkit

# Initialize the project
npx flue init --target node
```

- `@flue/cli` and `@flue/runtime`: Build and run the Flue project using Rivet's preview Flue packages.
- `@rivet-dev/flue`: Runs Flue agents and workflows as Rivet Actors.
- `@rivet-dev/agentos` and `@rivet-dev/agentos-flue`: Provide the agentOS VM and connect Flue's sandbox API to it.

*This uses [Rivet's Flue fork](https://github.com/rivet-dev/flue). We're working to merge its extension APIs upstream so Flue can support actor-model runtimes without a Rivet fork.*

Create `actors.ts`:

Update `flue.config.ts`:

Create `agents/assistant.ts`:

Set the provider key required by your model, such as `ANTHROPIC_API_KEY`, in `.env`.

Run the agent:

```sh
npx flue run assistant --id local \
  --input '{"message":"Write hello from Flue to /workspace/hello.txt, run wc -c /workspace/hello.txt, then read the file back."}'
```

Deploy to one of the supported platforms:

## Runtime model

agentOS does not support Cloudflare Workers yet. It works with Node.js, Bun, or
Deno on platforms such as Railway, Kubernetes, or Vercel.

## Default filesystem

agentOS persists the VM filesystem, including `/workspace`, to Rivet Actor storage by default. Additional mounts can be configured as needed.

## Configuration

### Virtual machine

See the `agentOS()` [configuration reference](/docs/core#configuration-reference) to configure the VM.

### Flue sandbox

`agentOSSandbox()` accepts:

| Option | Required | Description |
| --- | --- | --- |
| `actor` | Yes | Actor registered with `setup()`, such as `vm`. |
| `registry` | Yes | The application registry exported from `actors.ts`. |
| `cwd` | No | Base directory exposed to Flue. Defaults to `/workspace`. |
| `params` | No | Connection parameters forwarded to the actor's `onBeforeConnect` hook. |
| `client` | No | An existing client configured for the same registry. |