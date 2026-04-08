import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { AgentOs } from "../../src/agent-os.js";

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

export function resolveOpenCodeAdapterBinPath(moduleAccessCwd: string): string {
	const hostPkgJson = join(
		moduleAccessCwd,
		"node_modules/@rivet-dev/agent-os-opencode/package.json",
	);
	const pkg = JSON.parse(readFileSync(hostPkgJson, "utf-8"));

	let binEntry: string;
	if (typeof pkg.bin === "string") {
		binEntry = pkg.bin;
	} else if (typeof pkg.bin === "object" && pkg.bin !== null) {
		binEntry = Object.values(pkg.bin)[0] as string;
	} else {
		throw new Error(
			"No bin entry in @rivet-dev/agent-os-opencode package.json",
		);
	}

	return `/root/node_modules/@rivet-dev/agent-os-opencode/${binEntry}`;
}

export async function createVmOpenCodeHome(
	vm: AgentOs,
	mockUrl: string,
	permission?: Record<string, string>,
): Promise<string> {
	const homeDir = `/tmp/opencode-home-${randomUUID()}`;
	const configPath = `${homeDir}/.config/opencode/opencode.json`;
	await mkdirpVm(vm, `${homeDir}/.config/opencode`);
	await vm.writeFile(
		configPath,
		JSON.stringify(
			{
				$schema: "https://opencode.ai/config.json",
				autoupdate: false,
				share: "disabled",
				snapshot: false,
				model: "anthropic/claude-sonnet-4-20250514",
				...(permission ? { permission } : {}),
				provider: {
					anthropic: {
						options: {
							baseURL: `${mockUrl}/v1`,
						},
					},
				},
			},
			null,
			2,
		),
	);
	return homeDir;
}

export async function createVmWorkspace(vm: AgentOs): Promise<string> {
	const workspaceDir = `/tmp/opencode-workspace-${randomUUID()}`;
	await mkdirpVm(vm, workspaceDir);
	return workspaceDir;
}

export async function readVmText(vm: AgentOs, path: string): Promise<string> {
	return new TextDecoder().decode(await vm.readFile(path));
}
