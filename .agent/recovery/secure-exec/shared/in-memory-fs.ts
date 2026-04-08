/**
 * Factory for creating an in-memory VirtualFileSystem backed by ChunkedVFS.
 *
 * Replaces the old monolithic InMemoryFileSystem with
 * ChunkedVFS(InMemoryMetadataStore + InMemoryBlockStore).
 */

import type { VirtualFileSystem } from "../kernel/vfs.js";
import { KernelError, O_CREAT, O_EXCL, O_TRUNC } from "../kernel/types.js";
import { createChunkedVfs } from "../vfs/chunked-vfs.js";
import { InMemoryMetadataStore } from "../vfs/memory-metadata.js";
import { InMemoryBlockStore } from "../vfs/memory-block-store.js";

/**
 * Create an in-memory VirtualFileSystem using the chunked storage architecture.
 *
 * The returned VFS stores all data in memory via InMemoryMetadataStore and
 * InMemoryBlockStore, composed through ChunkedVFS. It also includes a
 * synchronous `prepareOpenSync` method used by the kernel for O_CREAT/O_EXCL/O_TRUNC
 * handling during fdOpen.
 */
export function createInMemoryFileSystem(): VirtualFileSystem {
	const metadata = new InMemoryMetadataStore();
	const blocks = new InMemoryBlockStore();
	const vfs = createChunkedVfs({ metadata, blocks });

	// The kernel's fdOpen calls prepareOpenSync synchronously for O_CREAT,
	// O_EXCL, and O_TRUNC flags. Since InMemoryMetadataStore is backed by
	// synchronous Maps, we use its synchronous accessor methods directly.
	function prepareOpenSync(path: string, flags: number): boolean {
		const hasCreate = (flags & O_CREAT) !== 0;
		const hasExcl = (flags & O_EXCL) !== 0;
		const hasTrunc = (flags & O_TRUNC) !== 0;

		// Check if path exists via synchronous resolution.
		let resolvedIno: number | undefined;
		try {
			resolvedIno = metadata.resolvePathSync(path);
		} catch {
			// ENOENT is expected when the file doesn't exist yet.
		}

		const exists = resolvedIno !== undefined;

		if (hasCreate && hasExcl && exists) {
			throw new KernelError("EEXIST", `file already exists, open '${path}'`);
		}

		let created = false;
		if (!exists && hasCreate) {
			// Create parent directories and the file synchronously.
			const parts = path.replace(/\/+/g, "/").replace(/\/$/, "").split("/").filter(Boolean);
			let parentIno = 1; // root

			for (let i = 0; i < parts.length - 1; i++) {
				const childIno = metadata.lookupSync(parentIno, parts[i]);
				if (childIno === null) {
					const newIno = metadata.createInodeSync({
						type: "directory",
						mode: 0o755,
						uid: 0,
						gid: 0,
					});
					metadata.updateInodeSync(newIno, { nlink: 2 });
					metadata.createDentrySync(parentIno, parts[i], newIno, "directory");
					// Increment parent nlink for subdirectory
					const parentMeta = metadata.getInodeSync(parentIno);
					if (parentMeta) {
						metadata.updateInodeSync(parentIno, { nlink: parentMeta.nlink + 1 });
					}
					parentIno = newIno;
				} else {
					parentIno = childIno;
				}
			}

			// Create the file inode.
			const fileName = parts[parts.length - 1];
			if (fileName) {
				const fileIno = metadata.createInodeSync({
					type: "file",
					mode: 0o644,
					uid: 0,
					gid: 0,
				});
				metadata.updateInodeSync(fileIno, {
					nlink: 1,
					size: 0,
					storageMode: "inline",
					inlineContent: new Uint8Array(0),
				});
				try {
					metadata.createDentrySync(parentIno, fileName, fileIno, "file");
					created = true;
				} catch {
					// EEXIST from race condition, ignore.
				}
			}
		}

		if (hasTrunc && resolvedIno !== undefined) {
			// Check that the target is a file, not a directory.
			const meta = metadata.getInodeSync(resolvedIno);
			if (meta && meta.type === "directory") {
				throw new KernelError("EISDIR", `illegal operation on a directory, open '${path}'`);
			}
			// Truncate file to 0 bytes.
			metadata.updateInodeSync(resolvedIno, {
				size: 0,
				storageMode: "inline",
				inlineContent: new Uint8Array(0),
			});
			// Delete any existing chunks synchronously.
			const keys = metadata.deleteAllChunksSync(resolvedIno);
			if (keys.length > 0) {
				// Fire-and-forget async block deletion. Blocks are in memory so this resolves immediately.
				void blocks.deleteMany(keys);
			}
		}

		return created;
	}

	return Object.assign(vfs, { prepareOpenSync });
}
