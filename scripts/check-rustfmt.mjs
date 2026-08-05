import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const defaultRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function parseArgs(argv) {
	let root = defaultRoot;
	for (let index = 0; index < argv.length; index += 1) {
		const arg = argv[index];
		if (arg === "--root") {
			const value = argv[++index];
			if (!value) throw new Error("--root requires a path");
			root = value;
			continue;
		}
		if (arg.startsWith("--root=")) {
			root = arg.slice("--root=".length);
			continue;
		}
		throw new Error(`unknown argument: ${arg}`);
	}
	return { root: resolve(root) };
}

export function main(argv = process.argv.slice(2)) {
	const { root } = parseArgs(argv);
	if (!existsSync(resolve(root, "Cargo.toml"))) {
		throw new Error(`Cargo.toml not found under ${root}`);
	}
	execFileSync("cargo", ["fmt", "--all", "--check"], {
		cwd: root,
		stdio: "inherit",
	});
	console.log("Rust formatting ok (all active workspace packages)");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	main();
}
