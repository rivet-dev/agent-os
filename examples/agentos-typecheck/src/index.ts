/**
 * Type-checking example for `@rivet-dev/agentos`.
 *
 * This file exercises the public actor package surface. It is not meant to
 * run: the actor delegates VM operations to the AgentOS runtime and sidecar.
 */

import pi from "@agentos-software/pi";
import {
	type AgentOSConfigInput,
	type AgentOsEvents,
	agentOS,
	type NodeModulesMountConfig,
	nodeModulesMount,
	type PromptResult,
	type SerializableCronJobInfo,
	type SerializableCronJobOptions,
	type SessionInfo,
	setup,
} from "@rivet-dev/agentos";
import { createClient } from "@rivet-dev/agentos/client";

const mount: NodeModulesMountConfig = nodeModulesMount(
	"/abs/host/node_modules",
	{
		readOnly: true,
	},
);

const config: AgentOSConfigInput = {
	software: [pi],
	mounts: [mount],
	allowedNodeBuiltins: ["path", "fs"],
	loopbackExemptPorts: [3000],
	preview: {
		defaultExpiresInSeconds: 3600,
		maxExpiresInSeconds: 86_400,
	},
	onSessionEvent: async (c, sessionId, event) => {
		console.log(c.actorId, sessionId, event.type);
	},
};

const vm = agentOS(config);
const inputVm = agentOS<
	undefined,
	undefined,
	undefined,
	undefined,
	{ workspace: string }
>({
	onCreate: (_c, input) => {
		console.log(input.workspace);
	},
});
const registry = setup({ use: { vm, inputVm } });
const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});

type PublicDomainTypes = PromptResult | SerializableCronJobInfo | SessionInfo;
type SessionStreamEntry = AgentOsEvents["sessionEvent"];

function acceptPublicDomainType(value: PublicDomainTypes): PublicDomainTypes {
	return value;
}

function acceptSessionStreamEntry(event: SessionStreamEntry): SessionStreamEntry {
	return event;
}

function acceptEvent<K extends keyof AgentOsEvents>(
	name: K,
	payload: AgentOsEvents[K],
): AgentOsEvents[K] {
	console.log(name);
	return payload;
}

const cron: SerializableCronJobOptions = {
	schedule: "*/5 * * * *",
	action: { type: "exec", command: "echo", args: ["tick"] },
	overlap: "skip",
};

async function main(): Promise<void> {
	const handle = client.vm.getOrCreate("my-agent");
	void client.vm.get("my-agent").createPreviewUrl;
	void client.vm.getForId("actor-id").createPreviewUrl;
	const connection = handle.connect();
	connection.on("sessionEvent", (event) => acceptSessionStreamEntry(event));
	connection.on("sessionEvent", (event) => {
		if (event.sessionId === "session-1") acceptSessionStreamEntry(event);
	});
	const createdHandle = await client.inputVm.create("input-agent", {
		input: { workspace: "/work" },
	});
	void createdHandle.createPreviewUrl;

	const sessionId = "session-1";
	await handle.sessions.open({
		sessionId,
		agent: "pi",
		cwd: "/work",
		permissionPolicy: "ask",
	});
	const session = await handle.sessions.get({ sessionId });
	await handle.sessions.prompt({
		sessionId: "session-1",
		content: [{ type: "text", text: "List the files in /work." }],
	});
	await handle.cron.schedule(cron);
	await handle.createPreviewUrl(3000, 300);

	acceptEvent("sessionEvent", {
		durability: "ephemeral",
		type: "agent_message_chunk",
		sessionId: "session-1",
		afterSequence: 0,
		content: { type: "text", text: "Working…" },
	});
	acceptEvent("sessionEvent", {
		durability: "durable",
		type: "permission_request",
		sessionId: "session-1",
		sequence: 1,
		timestamp: new Date().toISOString(),
		requestId: "request-1",
		toolCall: { toolCallId: "tool-1", title: "Write a file" },
		options: [
			{
				optionId: "allow_once",
				name: "Allow once",
				kind: "allow_once",
			},
		],
	});

	acceptPublicDomainType(session);
}

export { main };
