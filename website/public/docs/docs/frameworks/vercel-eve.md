# Vercel Eve

Run Vercel Eve with agentOS and Rivet World.

Eve owns the agent runtime and session lifecycle. agentOS maps every sandbox session to an isolated VM actor with a durable `/workspace` filesystem, while Rivet World runs Eve workflows on Rivet Actors.

[View the complete example →](https://github.com/rivet-dev/agentos/tree/main/examples/vercel-eve)

## Quickstart

```sh
npx eve@latest init my-agent
cd my-agent
```

```sh
npm add @rivet-dev/agentos @rivet-dev/agentos-eve @rivet-dev/vercel-world
```

- `@rivet-dev/agentos` and `@rivet-dev/agentos-eve`: Provide the agentOS VM and connect Eve's sandbox API to it.
- `@rivet-dev/vercel-world`: Runs Eve workflows on [Rivet World](https://workflow-sdk.dev/worlds).

Update `agent/agent.ts`:

Rivet World lets you run Eve on top of Rivet.

Add the World module import to `package.json`:

```json title="package.json"
{
	"imports": {
		"#world": "./world.ts"
	}
}
```

Create `world.ts`:

The first World operation starts this registry and waits for the Rivet envoy to
be ready.

Create `actors.ts`:

Create `agent/sandbox.ts`:

Install the Vercel CLI, then link Eve once so it can call your configured model:

```sh
npm install --global vercel@latest
npx eve link
```

Then run the agent:

```sh
npx eve dev
```

By default, agentOS runs locally with `npx rivetkit dev` — no infrastructure needed. To run in production, deploy to any of these targets:

See [Deployment](/docs/deployment) for managed, self-hosted, and agentOS Core options.

## Default Filesystem

agentOS persists the VM filesystem, including `/workspace`, to Rivet Actor storage by default. Additional mounts can be configured as needed.

## Configuration

### Virtual Machine

See the `agentOS()` [configuration reference](/docs/core#configuration-reference) to configure the VM.

### Eve Sandbox Backend

`agentOSBackend()` accepts:

| Option | Required | Description |
| --- | --- | --- |
| `actor` | Yes | Actor registered with `setup()`, such as `vm`. |
| `registry` | Yes | The application registry containing that actor. It is started lazily and shared by Eve sessions. |
| `client` | No | An existing client configured for the same registry. |

### Rivet World

Rivet World stores Eve workflow runs in Rivet Actors so they resume instead of restarting.

[Read the Rivet World documentation →](https://rivet.dev/docs/integrations/vercel-workflows)

## Advanced

### agentOS Core Backend

Use `agentOSCoreBackend()` when Eve should create agentOS Core VMs directly without Rivet Actor orchestration. The `create` callback owns the complete VM configuration:

```sh
pnpm add @rivet-dev/agentos-core
```

When using agentOS Core instead of regular agentOS, you lose:

- **Durable filesystem and session history.** Core's root filesystem is ephemeral by default, so you must provide your own persistent mount at `/workspace`.
- **Stable per-session actor identity.** Core cannot reconnect to the same VM across Eve process restarts.
- **Automatic sleep and wake.** The VM lives inside Eve's short-lived server process instead staying awake for a given grace period. `shutdown()` disposes it.

Use Core only when your application owns equivalent persistence and lifecycle management.