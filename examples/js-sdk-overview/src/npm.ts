import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create();

try {
	await runtime.javascript.npm.install({ frozen: true }); // lockfile-exact

	const build = await runtime.javascript.npm.runScript("build");
	console.log(build.outcome); // "succeeded"

	await runtime.javascript.npm.runPackage("prettier", {
		args: ["--check", "."],
	});
} finally {
	await runtime.dispose();
}
