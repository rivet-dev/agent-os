import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { AgentOs } from "../../src/agent-os.js";

type OpenCodeProviderConfig = {
	name?: string;
	env?: string[];
	npm?: string;
	api?: string;
	options?: Record<string, unknown>;
	models?: Record<string, unknown>;
};

type CreateVmOpenCodeHomeOptions = {
	permission?: Record<string, string>;
	model?: string;
	providers?: Record<string, OpenCodeProviderConfig>;
};

const openCodeConfigs = new WeakMap<AgentOs, string>();

async function mkdirpVm(vm: AgentOs, targetPath: string): Promise<void> {
	const parts = targetPath.split("/").filter(Boolean);
	let current = "";
	for (const part of parts) {
		current += `/${part}`;
		try {
			await vm.mkdir(current);
		} catch {
			// Directory already exists.
		}
	}
}

export function resolveOpenCodeAdapterBinPath(hostProjectDir: string): string {
	const hostPkgJson = join(
		hostProjectDir,
		"node_modules/@agentos-software/opencode/package.json",
	);
	const pkg = JSON.parse(readFileSync(hostPkgJson, "utf-8"));

	let binEntry: string;
	if (typeof pkg.bin === "string") {
		binEntry = pkg.bin;
	} else if (typeof pkg.bin === "object" && pkg.bin !== null) {
		binEntry = Object.values(pkg.bin)[0] as string;
	} else {
		throw new Error("No bin entry in @agentos-software/opencode package.json");
	}

	return `/root/node_modules/@agentos-software/opencode/${binEntry}`;
}

export async function createVmOpenCodeHome(
	vm: AgentOs,
	mockUrl: string,
	options: CreateVmOpenCodeHomeOptions = {},
): Promise<string> {
	const homeDir = `/tmp/opencode-home-${randomUUID()}`;
	const configPath = `${homeDir}/.config/opencode/config.json`;
	await mkdirpVm(vm, `${homeDir}/.config/opencode`);
	const providers = options.providers ?? {
		anthropic: {
			options: {
				baseURL: `${mockUrl}/v1`,
			},
		},
	};
	const config = JSON.stringify(
		{
			$schema: "https://opencode.ai/config.json",
			autoupdate: false,
			share: "disabled",
			snapshot: false,
			model: options.model ?? "anthropic/claude-sonnet-4-6",
			...(options.permission ? { permission: options.permission } : {}),
			provider: providers,
		},
		null,
		2,
	);
	await vm.writeFile(configPath, config);
	openCodeConfigs.set(vm, config);
	return homeDir;
}

export async function createVmWorkspace(vm: AgentOs): Promise<string> {
	const workspaceDir = `/tmp/opencode-workspace-${randomUUID()}`;
	await mkdirpVm(vm, workspaceDir);
	const config = openCodeConfigs.get(vm);
	if (config) {
		// A project config is independent of process-global HOME resolution and
		// matches how OpenCode selects providers/models for the session cwd.
		await vm.writeFile(`${workspaceDir}/opencode.json`, config);
	}
	return workspaceDir;
}

export async function readVmText(vm: AgentOs, path: string): Promise<string> {
	return new TextDecoder().decode(await vm.readFile(path));
}
