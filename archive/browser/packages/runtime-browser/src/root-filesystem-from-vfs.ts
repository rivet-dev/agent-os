// Migration shim: snapshot a legacy caller-provided VirtualFileSystem into a
// kernel `RootFilesystemConfig` (bootstrap entries) so the converged driver can
// seed a kernel-owned VM from filesystem content that was previously handed in
// as a live TS VFS object. This bridges the legacy `options.system.filesystem`
// model to the converged kernel-owns-fs model without rewriting every caller at
// once; new callers should provide a `CreateVmConfig.rootFilesystem` directly.

import type { RootFilesystemConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import { encodeBase64 } from "./converged-base64.js";
import type { VirtualFileSystem } from "./runtime.js";

type RootFilesystemEntry = RootFilesystemConfig["bootstrapEntries"][number];

// Kernel-owned pseudo-filesystems must not be materialized as bootstrap entries.
const SKIP_ROOTS = ["/dev", "/proc", "/sys"];

export interface RootFilesystemSnapshotOptions {
	root?: string;
	mode?: RootFilesystemConfig["mode"];
	disableDefaultBaseLayer?: boolean;
}

/** Walk `vfs` and produce kernel bootstrap entries for its contents. */
export async function collectRootFilesystemEntries(
	vfs: VirtualFileSystem,
	root = "/",
): Promise<RootFilesystemEntry[]> {
	const entries: RootFilesystemEntry[] = [];
	await walk(vfs, normalizeDir(root), entries);
	return entries;
}

/** A full `RootFilesystemConfig` seeded from `vfs`. */
export async function rootFilesystemConfigFromVfs(
	vfs: VirtualFileSystem,
	options: RootFilesystemSnapshotOptions = {},
): Promise<RootFilesystemConfig> {
	return {
		mode: options.mode ?? "ephemeral",
		disableDefaultBaseLayer: options.disableDefaultBaseLayer ?? false,
		lowers: [],
		bootstrapEntries: await collectRootFilesystemEntries(
			vfs,
			options.root ?? "/",
		),
	};
}

async function walk(
	vfs: VirtualFileSystem,
	dir: string,
	entries: RootFilesystemEntry[],
): Promise<void> {
	let children: Awaited<ReturnType<VirtualFileSystem["readDirWithTypes"]>>;
	try {
		children = await vfs.readDirWithTypes(dir);
	} catch {
		return;
	}
	for (const child of children) {
		if (child.name === "." || child.name === "..") {
			continue;
		}
		const path = joinPath(dir, child.name);
		if (SKIP_ROOTS.includes(path)) {
			continue;
		}
		if (child.isSymbolicLink) {
			const target = await vfs.readlink(path).catch(() => null);
			if (target !== null) {
				entries.push({ path, kind: "symlink", target, executable: false });
			}
			continue;
		}
		if (child.isDirectory) {
			entries.push({ path, kind: "directory", executable: true });
			await walk(vfs, path, entries);
			continue;
		}
		entries.push(await fileEntry(vfs, path));
	}
}

async function fileEntry(
	vfs: VirtualFileSystem,
	path: string,
): Promise<RootFilesystemEntry> {
	const bytes = await vfs.readFile(path);
	const executable = await isExecutable(vfs, path);
	const text = tryDecodeUtf8(bytes);
	if (text !== null) {
		return { path, kind: "file", content: text, encoding: "utf8", executable };
	}
	return {
		path,
		kind: "file",
		content: encodeBase64(bytes),
		encoding: "base64",
		executable,
	};
}

async function isExecutable(
	vfs: VirtualFileSystem,
	path: string,
): Promise<boolean> {
	try {
		return ((await vfs.stat(path)).mode & 0o111) !== 0;
	} catch {
		return false;
	}
}

function tryDecodeUtf8(bytes: Uint8Array): string | null {
	try {
		return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
	} catch {
		return null;
	}
}

function normalizeDir(dir: string): string {
	if (dir.length > 1 && dir.endsWith("/")) {
		return dir.slice(0, -1);
	}
	return dir;
}

function joinPath(parent: string, child: string): string {
	return parent === "/" ? `/${child}` : `${parent}/${child}`;
}
