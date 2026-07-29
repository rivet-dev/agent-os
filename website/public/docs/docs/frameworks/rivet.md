# Rivet

Run agentOS natively as Rivet Actors.

Rivet is agentOS's native framework. The `agentOS()` function returns an ordinary [Rivet Actor](https://rivet.dev/docs/actors), so each agentOS VM is a directly addressable actor with its own durable filesystem, state, and session history—no adapter layer required.

[View the complete example →](https://github.com/rivet-dev/agentos/tree/main/examples/quickstart-app)

## Quickstart

```sh
mkdir my-agent && cd my-agent
npm init -y
npm pkg set type=module
npm add @rivet-dev/agentos @agentos-software/pi
npm add --save-dev tsx typescript
```

Create `server.ts`. The `agentOS()` call returns a normal Rivet Actor definition, and `setup()` registers it with the Rivet runtime:

Create `client.ts` to address an actor by name, subscribe to its events, open a session, and send a prompt:

Start the actor, then run the client in a second terminal with the provider key required by your model:

```sh
# Terminal 1
npx tsx server.ts

# Terminal 2
ANTHROPIC_API_KEY=... npx tsx client.ts
```

Because an agentOS VM is a Rivet Actor, you can compose it with Rivet's orchestration primitives:

- **[Durable workflows](/docs/workflows)** — Run multi-step agent tasks that retry and resume after crashes.
- **[Multiplayer agents](/docs/multiplayer)** — Stream agent output to multiple connected clients in real time.
- **[Agent-to-agent systems](/docs/agent-to-agent)** — Connect isolated agents through typed bindings and durable pipelines.
- **[Scheduled agents](/docs/cron)** — Run commands and agent sessions on timers or cron schedules.
- **[Human-in-the-loop flows](/docs/approvals)** — Pause for tool approval and resume from durable event history.

Deploy to one of the supported platforms:

## Learn more

See the [agentOS quickstart](/docs/quickstart) for VM configuration and the [Rivet Actor documentation](https://rivet.dev/docs/actors) for state, actions, events, queues, workflows, and deployment.