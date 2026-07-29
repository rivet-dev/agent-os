# `@rivet-dev/agentos-eve`

Use agentOS as the sandbox for Vercel Eve. Choose a Rivet-backed agentOS actor
or a standalone agentOS Core VM without coupling either hosting model to Eve.

Requires Node.js 24 or newer.

## Rivet actor

```sh
pnpm add eve @rivet-dev/agentos @rivet-dev/agentos-eve
```

Register the VM as a normal agentOS actor. Its configuration owns software,
permissions, limits, sandbox mounting, and persistence:

```ts
// actors.ts
import { agentOS, setup } from "@rivet-dev/agentos";

const vm = agentOS({
	// Configure software, permissions, limits, and mounts here.
});

export const registry = setup({
	use: { vm },
});
```

Select the actor by its registry key:

```ts
// agent/sandbox.ts
import { agentOSBackend } from "@rivet-dev/agentos-eve";
import { defineSandbox } from "eve/sandbox";
import { registry } from "../actors";

export default defineSandbox({
	backend: agentOSBackend({ actor: "vm", registry }),
});
```

Relative paths and command working directories resolve from `/workspace`.
Configure its persistence on the actor—for example with actor durable storage
or a mounted filesystem. The adapter never copies or interprets workspace data.

Each Eve session maps to a stable actor key. `shutdown()` stops processes opened
through Eve and disconnects the client, but does not destroy the actor, so the
session can reattach after actor sleep or process restart.

## Advanced: standalone Core

Install Core instead of the actor package when Rivet orchestration is not
needed:

```sh
pnpm add eve @rivet-dev/agentos-core @rivet-dev/agentos-eve
```

The factory creates one VM per Eve session. All VM configuration and filesystem
persistence remain caller-owned:

```ts
import { AgentOs } from "@rivet-dev/agentos-core";
import { agentOSCoreBackend } from "@rivet-dev/agentos-eve";
import { defineSandbox } from "eve/sandbox";

export default defineSandbox({
	backend: agentOSCoreBackend({
		create: ({ sessionKey }) =>
			AgentOs.create({
				mounts: [
					{
						path: "/workspace",
						plugin: {
							id: "host_dir",
							config: {
								hostPath: `/var/lib/eve/${encodeURIComponent(sessionKey)}`,
							},
						},
						readOnly: false,
					},
				],
			}),
	}),
});
```

Standalone Core has no Rivet orchestration or automatic durable storage.
`shutdown()` stops Eve processes and disposes the caller-created VM.

Network permissions belong to `agentOS(...)` or the Core `create()` factory;
Eve's runtime `setNetworkPolicy()` operation is unsupported.
