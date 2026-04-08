/**
 * Advisory file lock manager (flock semantics).
 *
 * Locks are per-path (inode proxy). Multiple FDs sharing the same
 * FileDescription (via dup) share the same lock. Locks are released
 * when the description's refCount drops to zero (all FDs closed).
 */

import { KernelError } from "./types.js";
import { WaitQueue } from "./wait.js";

// flock operation flags (POSIX)
export const LOCK_SH = 1;
export const LOCK_EX = 2;
export const LOCK_UN = 8;
export const LOCK_NB = 4;

interface LockEntry {
	descriptionId: number;
	type: "sh" | "ex";
}

interface PathLockState {
	holders: LockEntry[];
	waiters: WaitQueue;
}

export class FileLockManager {
	/** path -> lock state */
	private locks = new Map<string, PathLockState>();
	/** descriptionId -> path (for cleanup) */
	private descToPath = new Map<number, string>();

	/**
	 * Acquire, upgrade/downgrade, or release a lock.
	 *
	 * @param path      Resolved file path (inode proxy)
	 * @param descId    FileDescription id (shared across dup'd FDs)
	 * @param operation LOCK_SH | LOCK_EX | LOCK_UN, optionally | LOCK_NB
	 */
	async flock(path: string, descId: number, operation: number): Promise<void> {
		const op = operation & ~LOCK_NB;
		const nonBlocking = (operation & LOCK_NB) !== 0;

		if (op === LOCK_UN) {
			this.unlock(path, descId);
			return;
		}

		while (true) {
			const state = this.getOrCreate(path);
			if (this.tryAcquire(path, state, descId, op)) {
				return;
			}

			if (nonBlocking) {
				throw new KernelError("EAGAIN", "resource temporarily unavailable");
			}

			// Wait indefinitely until an unlock wakes this waiter.
			const handle = state.waiters.enqueue();
			try {
				await handle.wait();
			} finally {
				state.waiters.remove(handle);
				this.cleanupState(path, state);
			}
		}
	}

	/** Release the lock held by a specific description on a path. */
	private unlock(path: string, descId: number): void {
		const state = this.locks.get(path);
		if (!state) return;

		const idx = state.holders.findIndex(h => h.descriptionId === descId);
		if (idx >= 0) {
			state.holders.splice(idx, 1);
			this.descToPath.delete(descId);
			state.waiters.wakeOne();
		}
		this.cleanupState(path, state);
	}

	/** Release all locks held by a specific description (called on FD close when refCount drops to 0). */
	releaseByDescription(descId: number): void {
		const path = this.descToPath.get(descId);
		if (path === undefined) return;
		this.unlock(path, descId);
	}

	/** Check if a description holds any lock. */
	hasLock(descId: number): boolean {
		return this.descToPath.has(descId);
	}

	private getOrCreate(path: string): PathLockState {
		let state = this.locks.get(path);
		if (!state) {
			state = { holders: [], waiters: new WaitQueue() };
			this.locks.set(path, state);
		}
		return state;
	}

	private tryAcquire(path: string, state: PathLockState, descId: number, op: number): boolean {
		const existingIdx = state.holders.findIndex(h => h.descriptionId === descId);

		if (op === LOCK_SH) {
			const conflict = state.holders.some(
				h => h.type === "ex" && h.descriptionId !== descId,
			);
			if (conflict) {
				return false;
			}

			if (existingIdx >= 0) {
				state.holders[existingIdx].type = "sh";
			} else {
				state.holders.push({ descriptionId: descId, type: "sh" });
				this.descToPath.set(descId, path);
			}
			return true;
		}

		if (op === LOCK_EX) {
			const conflict = state.holders.some(h => h.descriptionId !== descId);
			if (conflict) {
				return false;
			}

			if (existingIdx >= 0) {
				state.holders[existingIdx].type = "ex";
			} else {
				state.holders.push({ descriptionId: descId, type: "ex" });
				this.descToPath.set(descId, path);
			}
			return true;
		}

		throw new KernelError("EINVAL", `unsupported flock operation ${op}`);
	}

	private cleanupState(path: string, state: PathLockState): void {
		if (state.holders.length === 0 && state.waiters.pending === 0) {
			this.locks.delete(path);
		}
	}
}
