/**
 * Symlink vendor assets from node_modules into vendor/ so the dev server
 * can serve them as static files without a CDN proxy.
 */
import { mkdir, readlink, symlink, unlink } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const playgroundDir = resolve(fileURLToPath(new URL("..", import.meta.url)));
const vendorDir = resolve(playgroundDir, "vendor");
const nodeModules = resolve(playgroundDir, "node_modules");

const LINKS: Array<{ name: string; target: string }> = [
	{ name: "monaco", target: resolve(nodeModules, "monaco-editor/min") },
	{
		name: "typescript.js",
		target: resolve(nodeModules, "typescript/lib/typescript.js"),
	},
];

async function ensureSymlink(linkPath: string, target: string): Promise<void> {
	try {
		const existing = await readlink(linkPath);
		if (existing === target) return;
		await unlink(linkPath);
	} catch {
		/* link doesn't exist yet */
	}
	await symlink(target, linkPath);
}

async function main(): Promise<void> {
	await mkdir(vendorDir, { recursive: true });
	await Promise.all(
		LINKS.map(({ name, target }) =>
			ensureSymlink(resolve(vendorDir, name), target),
		),
	);
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
