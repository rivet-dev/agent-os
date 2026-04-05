import { createInMemoryFileSystem } from "./runtime-compat.js";
import type { ToolKit } from "./host-tools.js";

const NETWORK_ERROR_JSON =
	'{"ok":false,"error":"INTERNAL_ERROR","message":"Could not reach host tools server"}';
const MISSING_PORT_JSON =
	'{"ok":false,"error":"INTERNAL_ERROR","message":"AGENTOS_TOOLS_PORT not set. Host tools not available."}';

function buildCommonRuntime(toolkitName?: string): string {
	const toolkitDecl = toolkitName
		? `const TOOLKIT = ${JSON.stringify(toolkitName)};\n`
		: "";

return `const fs = require("node:fs/promises");
${toolkitDecl}const PORT = process.env.AGENTOS_TOOLS_PORT;
const BASE = PORT ? \`http://127.0.0.1:\${PORT}\` : "";

function getCliArgs() {
	const args = process.argv.slice(2);
	if (args[0] && args[0] === process.argv[1]) {
		return args.slice(1);
	}
	return args;
}

async function rpcRequest(path, init) {
	try {
		const res = await fetch(\`\${BASE}\${path}\`, init);
		process.stdout.write(await res.text());
		return 0;
	} catch {
		process.stdout.write(${JSON.stringify(NETWORK_ERROR_JSON)});
		return 1;
	}
}

function validationError(message) {
	process.stdout.write(
		JSON.stringify({ ok: false, error: "VALIDATION_ERROR", message }),
	);
	return 1;
}

async function runTool(toolkit, tool, args) {
	let payload;

	if (args[0] === "--json") {
		if (args.length < 2) return validationError("Flag --json requires a value");
		try {
			payload = { toolkit, tool, input: JSON.parse(args[1]) };
		} catch (err) {
			return validationError(\`Invalid JSON for --json: \${err instanceof Error ? err.message : String(err)}\`);
		}
	} else if (args[0] === "--json-file") {
		if (args.length < 2) {
			return validationError("Flag --json-file requires a value");
		}
		try {
			payload = {
				toolkit,
				tool,
				input: JSON.parse(await fs.readFile(args[1], "utf8")),
			};
		} catch (err) {
			return validationError(\`Invalid JSON file: \${err instanceof Error ? err.message : String(err)}\`);
		}
	} else {
		payload = { toolkit, tool, argv: args };
	}

	return rpcRequest("/call", {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify(payload),
	});
}
`;
}

/**
 * Generate a Node.js CLI shim for a toolkit entrypoint.
 * Invoked as: node /usr/local/bin/agentos-{name} <tool> ...
 */
export function generateToolkitShim(toolkitName: string): string {
	return `#!/usr/bin/env node
${buildCommonRuntime(toolkitName)}

(async () => {
	if (!PORT) {
		process.stdout.write(${JSON.stringify(MISSING_PORT_JSON)});
		process.exit(1);
	}

	const args = getCliArgs();
	const tool = args[0];
	const rest = args.slice(1);

	if (!tool || tool === "--help" || tool === "-h") {
		return rpcRequest(\`/describe/\${TOOLKIT}\`);
	}

	if (rest[0] === "--help" || rest[0] === "-h") {
		return rpcRequest(\`/describe/\${TOOLKIT}/\${tool}\`);
	}

	return runTool(TOOLKIT, tool, rest);
})().catch((err) => {
	process.stderr.write(String(err) + "\\n");
	return 1;
}).then((code) => {
	process.exitCode = code ?? 0;
});
`;
}

/**
 * Generate the master Node.js host tools CLI.
 * Invoked as:
 * - node /usr/local/bin/agentos list-tools [toolkit]
 * - node /usr/local/bin/agentos <toolkit> [tool] ...
 */
export function generateMasterShim(): string {
	return `#!/usr/bin/env node
${buildCommonRuntime()}

function printUsage() {
	process.stdout.write("Usage: agentos <command>\\n\\n");
	process.stdout.write("Commands:\\n");
	process.stdout.write("  list-tools [toolkit]   List available toolkits and tools\\n");
	process.stdout.write("  <toolkit> --help       Describe one toolkit\\n");
	process.stdout.write("  <toolkit> <tool> ...   Run a host tool\\n");
}

(async () => {
	if (!PORT) {
		process.stdout.write(${JSON.stringify(MISSING_PORT_JSON)});
		process.exit(1);
	}

	const args = getCliArgs();
	const cmd = args[0];

	if (!cmd || cmd === "--help" || cmd === "-h") {
		printUsage();
		return 0;
	}

	if (cmd === "list-tools") {
		const toolkit = args[1];
		const path = toolkit ? \`/list/\${toolkit}\` : "/list";
		return rpcRequest(path);
	}

	const toolkit = cmd;
	const tool = args[1];
	const rest = args.slice(2);

	if (!tool || tool === "--help" || tool === "-h") {
		return rpcRequest(\`/describe/\${toolkit}\`);
	}

	if (rest[0] === "--help" || rest[0] === "-h") {
		return rpcRequest(\`/describe/\${toolkit}/\${tool}\`);
	}

	return runTool(toolkit, tool, rest);
})().catch((err) => {
	process.stderr.write(String(err) + "\\n");
	return 1;
}).then((code) => {
	process.exitCode = code ?? 0;
});
`;
}

/**
 * Create a pre-populated InMemoryFileSystem with all CLI shims.
 * These are JS entrypoints invoked via `node /usr/local/bin/...`.
 */
export async function createShimFilesystem(
	toolkits: ToolKit[],
): Promise<ReturnType<typeof createInMemoryFileSystem>> {
	const fs = createInMemoryFileSystem();

	await fs.writeFile("agentos", generateMasterShim());
	await fs.chmod("agentos", 0o100755);

	for (const tk of toolkits) {
		const filename = `agentos-${tk.name}`;
		await fs.writeFile(filename, generateToolkitShim(tk.name));
		await fs.chmod(filename, 0o100755);
	}

	return fs;
}
