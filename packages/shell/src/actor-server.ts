// Runtime server for the shell's `--actor` mode — the shell-CLI equivalent of
// `packages/agentos/tests/fixtures/actor-runtime-server.mjs`. Boots the
// agentOS actor registry on the native runtime and serves it against a local
// rivet engine (spawned by the native registry via RIVET_RUN_ENGINE_PORT /
// RIVET_ENGINE_BINARY). Spawned as a child by `actor-vm.ts` with the shell's
// VM options passed through AGENTOS_SHELL_ACTOR_OPTIONS.

import { agentOS, setup } from "@rivet-dev/agentos";
import { allowAll } from "@rivet-dev/agentos-core/internal/runtime-compat";

const options = JSON.parse(process.env.AGENTOS_SHELL_ACTOR_OPTIONS ?? "{}");

const vm = agentOS({ permissions: allowAll, ...options });
const registry = setup({
	use: { vm },
	endpoint: process.env.AGENTOS_SHELL_ENDPOINT,
	namespace: process.env.RIVET_NAMESPACE ?? "default",
	token: process.env.RIVET_TOKEN ?? "dev",
	envoy: { poolName: process.env.AGENTOS_SHELL_POOL_NAME ?? "agentos-shell" },
	runtime: "native",
} as never);

registry.start();
