import coreutils from "@agentos-software/coreutils";
import {
	type AgentOsConformanceAction,
	type AgentOsConformanceBackend,
	type AgentOsConformanceEvent,
	CONFORMANCE_ACP_ADAPTER,
	CONFORMANCE_AGENT_NAME,
	defineAgentOsConformanceSuite,
} from "@rivet-dev/agentos-test-harness/agent-os-conformance";
import { createProjectedAgentPackage } from "@rivet-dev/agentos-test-harness/projected-agent-package";
import { AgentOs } from "../src/index.js";

class EventBus {
	readonly handlers = new Map<string, Set<(payload: any) => void>>();

	on(event: string, handler: (payload: any) => void): () => void {
		let handlers = this.handlers.get(event);
		if (!handlers) {
			handlers = new Set();
			this.handlers.set(event, handlers);
		}
		handlers.add(handler);
		return () => handlers?.delete(handler);
	}

	emit(event: string, payload: unknown): void {
		for (const handler of this.handlers.get(event) ?? []) handler(payload);
	}
}

async function createCoreBackend(): Promise<AgentOsConformanceBackend> {
	const events = new EventBus();
	const agentPackage = createProjectedAgentPackage({
		name: CONFORMANCE_AGENT_NAME,
		adapterScript: CONFORMANCE_ACP_ADAPTER,
	});
	const mounts = [
		{
			path: "/conformance-mount",
			plugin: {
				id: "host_dir" as const,
				config: {
					hostPath: agentPackage.packageDir,
					readOnly: true,
				},
			},
			readOnly: true,
		},
	];
	const vm = await AgentOs.create({
		defaultSoftware: false,
		software: [coreutils, agentPackage.software],
		mounts,
		onAgentExit: (event) => events.emit("agentExit", event),
	});
	vm.onCronEvent((event) => events.emit("cronEvent", event));

	function trackSession(sessionId: string): void {
		vm.onSessionEvent(sessionId, (event) => events.emit("sessionEvent", event));
	}

	const call = async <T>(
		action: AgentOsConformanceAction,
		...args: unknown[]
	): Promise<T> => {
		switch (action) {
			case "filesystem.remove":
				return (await vm.filesystem.remove(
					...(args as Parameters<AgentOs["filesystem"]["remove"]>),
				)) as T;
			case "process.spawn": {
				const [command, processArgs, spawnOptions] = args as [
					string,
					string[],
					Record<string, unknown> | undefined,
				];
				const process = await vm.process.spawn(
					command,
					processArgs,
					spawnOptions,
				);
				vm.onProcessOutput(process.pid, (event) =>
					events.emit("processOutput", event),
				);
				vm.onProcessExit(process.pid, (event) =>
					events.emit("processExit", event),
				);
				return process as T;
			}
			case "terminal.open": {
				const shell = vm.terminal.open(
					args[0] as Parameters<AgentOs["terminal"]["open"]>[0],
				);
				vm.onShellData(shell.shellId, (event) =>
					events.emit("shellData", event),
				);
				vm.onShellStderr(shell.shellId, (event) =>
					events.emit("shellStderr", event),
				);
				vm.onShellExit(shell.shellId, (event) =>
					events.emit("shellExit", event),
				);
				return shell as T;
			}
			case "network.httpRequest":
				return (await vm.network.httpRequest(
					...(args as Parameters<AgentOs["network"]["httpRequest"]>),
				)) as T;
			case "cron.schedule": {
				const job = vm.cron.schedule(
					args[0] as Parameters<AgentOs["cron"]["schedule"]>[0],
				);
				return { id: job.id } as T;
			}
			case "cron.list":
				return vm.cron.list() as T;
			case "filesystem.listMounts":
				return (await vm.filesystem.listMounts()) as T;
			case "software.list":
				return (await vm.software.list()) as T;
			case "sessions.open": {
				const [input] = args as Parameters<AgentOs["sessions"]["open"]>;
				await vm.sessions.open(
					...(args as Parameters<AgentOs["sessions"]["open"]>),
				);
				trackSession(input.sessionId ?? "main");
				return undefined as T;
			}
			default: {
				const path = action.split(".");
				let owner: any = vm;
				for (const segment of path.slice(0, -1)) owner = owner[segment];
				const method = owner[path.at(-1)!];
				if (typeof method !== "function") {
					throw new Error(`Core backend does not implement ${action}`);
				}
				return (await method.apply(owner, args)) as T;
			}
		}
	};

	return {
		call,
		on: (event: AgentOsConformanceEvent, handler: (payload: any) => void) =>
			events.on(event, handler),
		async dispose() {
			await vm.dispose();
			agentPackage.cleanup();
		},
	};
}

defineAgentOsConformanceSuite({
	name: "AgentOS Core actor-surface conformance",
	createBackend: createCoreBackend,
});
