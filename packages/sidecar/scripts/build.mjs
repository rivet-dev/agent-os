#!/usr/bin/env node
// Build wrapper for the agentos-sidecar binary. Cargo owns incremental caching;
// the resolver finds the result under target/{release,debug}.
import { execFileSync } from "node:child_process";

const release = process.argv.includes("--release");

if (process.env.AGENTOS_SKIP_NATIVE_META_BUILD === "1") {
	console.log(
		"Skipping agentos-sidecar cargo build; publish CI already staged platform sidecar artifacts.",
	);
	process.exit(0);
}

execFileSync(
	"cargo",
	["build", "-p", "agentos-sidecar", ...(release ? ["--release"] : [])],
	{ stdio: "inherit" },
);
