import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { transform } from "@bare-ts/tools";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageDir = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(packageDir, "../..");
const schemaPath = path.join(
	repoRoot,
	"crates/acp-protocol/protocol/agent_os_acp_v1.bare",
);
const outputPath = path.join(
	packageDir,
	"src/sidecar/agentos-acp-protocol.ts",
);

const schema = await readFile(schemaPath, "utf8");
let output = transform(schema, { pedantic: false, generator: "ts" });
output = postProcess(output);

await mkdir(path.dirname(outputPath), { recursive: true });
await writeFile(outputPath, output);

function postProcess(code) {
	code = code.replace(/@bare-ts\/lib/g, "@rivetkit/bare-ts");
	code = code.replace(/^import assert from "assert"\n?/m, "");
	code = code.replace(/^import assert from "node:assert"\n?/m, "");

	if (code.includes("@bare-ts/lib")) {
		throw new Error("failed to replace @bare-ts/lib import");
	}
	if (code.includes('import assert from "')) {
		throw new Error("failed to remove generated assert import");
	}

	let header =
		"// @generated - run pnpm --dir packages/core build:agentos-acp-protocol\n";
	if (/\bassert\(/.test(code)) {
		header += `function assert(condition: boolean, message?: string): asserts condition {
\tif (!condition) throw new Error(message ?? "Assertion failed");
}
`;
	}

	return `${header}${code}`;
}
