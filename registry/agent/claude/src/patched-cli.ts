import { readFileSync } from "node:fs";
import { dirname, resolve as resolvePath } from "node:path";

type PatchedArtifactOptions = {
	packageDir: string;
	sdkPath: string;
};

type PatchedManifest = {
	entry?: string;
};

function resolveManifestEntry(
	packageDir: string,
	manifestName: string,
): string | undefined {
	const manifestPath = resolvePath(packageDir, "dist", manifestName);
	try {
		const manifest = JSON.parse(readFileSync(manifestPath, "utf-8")) as PatchedManifest;
		if (typeof manifest.entry === "string" && manifest.entry.length > 0) {
			return resolvePath(packageDir, "dist", manifest.entry.replace(/^\.\//, ""));
		}
	} catch {
	}
	return undefined;
}

export function resolveClaudeCliPath({
	packageDir,
	sdkPath,
}: PatchedArtifactOptions): string {
	return (
		resolveManifestEntry(packageDir, "claude-cli-patched.json") ??
		resolvePath(dirname(sdkPath), "cli.js")
	);
}

export function resolveClaudeSdkPath({
	packageDir,
	sdkPath,
}: PatchedArtifactOptions): string {
	return (
		resolveManifestEntry(packageDir, "claude-sdk-patched.json") ?? sdkPath
	);
}
