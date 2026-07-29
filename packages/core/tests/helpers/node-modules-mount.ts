import { nodeModulesMount } from "../../src/index.js";
import { ensureFlatNodeModules } from "./fixture-node-modules.js";

/**
 * Test helper: build the `mounts` entry that exposes a host `node_modules` tree
 * at `/root/node_modules` in the VM. The resolver reads the mounted tree
 * through the kernel VFS.
 *
 * The host `node_modules` must be a FLAT install: the agentos `host_dir`
 * mount resolves strictly beneath the mount root and refuses symlinks that
 * escape it, so pnpm's default symlinked `<pkg>/node_modules` (whose entries
 * point into the workspace-root `.pnpm` store) cannot be mounted directly.
 * `ensureFlatNodeModules` deploys a flat, self-contained tree for the workspace
 * package at `cwd` (cached + shared across workers) and we mount that.
 */
export function moduleAccessMounts(cwd: string) {
	return [nodeModulesMount(ensureFlatNodeModules(cwd))];
}
