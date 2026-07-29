#!/usr/bin/env node

import { chmodSync, copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve as resolvePath } from "node:path";

const require = createRequire(import.meta.url);
const sdkPath = require.resolve("@anthropic-ai/claude-agent-sdk");
const sdkRoot = dirname(sdkPath);
const sdkPackage = JSON.parse(
	readFileSync(resolvePath(sdkRoot, "package.json"), "utf8"),
);
const cliPath = resolvePath(sdkRoot, "cli.js");
const distDir = resolvePath(import.meta.dirname, "..", "dist");
const outputPath = resolvePath(distDir, "claude-cli.mjs");
const manifestPath = resolvePath(distDir, "claude-cli-upstream.json");

// Claude Agent SDK 0.2.112 / Claude Code 2.1.112 is the final release that
// ships its CLI as JavaScript. Later SDKs ship only closed platform-native
// executables, which cannot run inside an AgentOS VM. Stage it byte-for-byte;
// Node compatibility belongs in AgentOS runtime core, not in this bundle.
mkdirSync(distDir, { recursive: true });
copyFileSync(cliPath, outputPath);
chmodSync(outputPath, 0o755);
writeFileSync(
	manifestPath,
	`${JSON.stringify(
		{
			entry: "./claude-cli.mjs",
			sdkVersion: sdkPackage.version,
			claudeCodeVersion: sdkPackage.claudeCodeVersion,
			patches: [],
		},
		null,
		2,
	)}\n`,
	"utf8",
);

process.stdout.write(
	`Staged unmodified Claude Code ${sdkPackage.claudeCodeVersion} from Agent SDK ${sdkPackage.version}\n`,
);
