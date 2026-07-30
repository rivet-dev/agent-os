import {
	chmodSync,
	mkdirSync,
	mkdtempSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export interface ProjectedSoftwarePackageRef {
	packagePath: string;
}

export interface ProjectedAgentPackageOptions {
	name: string;
	adapterScript: string;
	acpEntrypoint?: string;
	env?: Record<string, string>;
	launchArgs?: string[];
}

export interface ProjectedAgentPackage {
	packageDir: string;
	software: ProjectedSoftwarePackageRef;
	binPath: string;
	cleanup(): void;
}

/** Creates a transition-directory agent package for deterministic ACP tests. */
export function createProjectedAgentPackage(
	options: ProjectedAgentPackageOptions,
): ProjectedAgentPackage {
	const root = mkdtempSync(join(tmpdir(), "agentos-projected-agent-"));
	const packageDir = join(root, "pkg");
	const binDir = join(packageDir, "bin");
	const acpEntrypoint =
		options.acpEntrypoint ??
		`${options.name.replace(/[^a-zA-Z0-9_-]/g, "-")}-acp`;

	mkdirSync(binDir, { recursive: true });
	writeFileSync(
		join(packageDir, "package.json"),
		JSON.stringify({ name: options.name, version: "1.0.0" }, null, 2),
	);
	writeFileSync(
		join(packageDir, "agentos-package.json"),
		JSON.stringify(
			{
				name: options.name,
				version: "1.0.0",
				agent: {
					acpEntrypoint,
					...(options.env ? { env: options.env } : {}),
					...(options.launchArgs ? { launchArgs: options.launchArgs } : {}),
				},
			},
			null,
			2,
		),
	);

	const binPath = join(binDir, acpEntrypoint);
	writeFileSync(binPath, `#!/usr/bin/env node\n${options.adapterScript}\n`);
	chmodSync(binPath, 0o755);

	return {
		packageDir,
		software: { packagePath: packageDir },
		binPath,
		cleanup: () => rmSync(root, { recursive: true, force: true }),
	};
}
