import { spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = fileURLToPath(new URL(".", import.meta.url));
const pnpmStoreDir = resolve(scriptDir, "..", "..", "node_modules", ".pnpm");
const vitestPackageDir = readdirSync(pnpmStoreDir).find((entry) =>
	entry.startsWith("vitest@"),
);

if (!vitestPackageDir) {
	throw new Error(`Could not find vitest in ${pnpmStoreDir}`);
}

const vitestCli = resolve(
	pnpmStoreDir,
	vitestPackageDir,
	"node_modules",
	"vitest",
	"vitest.mjs",
);

const result = spawnSync(process.execPath, [vitestCli, "run", ...process.argv.slice(2)], {
	stdio: "inherit",
});

if (result.error) {
	throw result.error;
}

process.exit(result.status ?? 1);
