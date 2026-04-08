/**
 * PTY manager.
 *
 * Allocates pseudo-terminal master/slave pairs with bidirectional data flow.
 * Writing to master → readable from slave (input direction).
 * Writing to slave → readable from master (output direction).
 * Follows the same FileDescription/refCount pattern as PipeManager.
 */

import type { FileDescription, Termios, KernelLogger } from "./types.js";
import {
	FILETYPE_CHARACTER_DEVICE,
	O_RDWR,
	KernelError,
	defaultTermios,
	noopKernelLogger,
} from "./types.js";
import type { ProcessFDTable } from "./fd-table.js";

export interface LineDisciplineConfig {
	/** Canonical mode: buffer input until newline, handle backspace. */
	canonical: boolean;
	/** Echo input bytes back through output (master reads them). */
	echo: boolean;
	/** Enable signal generation from control chars (^C, ^Z, ^\). */
	isig: boolean;
}

export interface PtyEnd {
	description: FileDescription;
	filetype: typeof FILETYPE_CHARACTER_DEVICE;
}

interface PtyState {
	id: number;
	path: string; // /dev/pts/N
	masterDescription: FileDescription;
	slaveDescription: FileDescription;
	/** Data written to master, readable from slave (input direction) */
	inputBuffer: Uint8Array[];
	/** Data written to slave, readable from master (output direction) */
	outputBuffer: Uint8Array[];
	closed: { master: boolean; slave: boolean };
	/** Resolves waiting for input data (slave reads) */
	inputWaiters: Array<(data: Uint8Array | null) => void>;
	/** Resolves waiting for output data (master reads) */
	outputWaiters: Array<(data: Uint8Array | null) => void>;
	/** Terminal attributes (controls line discipline behavior) */
	termios: Termios;
	/** Canonical mode line editing buffer */
	lineBuffer: number[];
	/** Foreground process group for signal delivery */
	foregroundPgid: number;
	/** Session leader's pgid — used to intercept SIGINT at the PTY level */
	sessionLeaderPgid: number;
}

/** Maximum buffered bytes per PTY direction before writes are rejected (EAGAIN). */
export const MAX_PTY_BUFFER_BYTES = 65_536; // 64 KB

/** Maximum canonical-mode line buffer size (POSIX MAX_CANON). */
export const MAX_CANON = 4096;

export class PtyManager {
	private ptys: Map<number, PtyState> = new Map();
	/** Map description ID → pty ID and which end */
	private descToPty: Map<number, { ptyId: number; end: "master" | "slave" }> = new Map();
	/**
	 * Signal delivery callback: (pgid, signal, excludeLeaders) → number of
	 * processes signaled. When excludeLeaders is true, session leaders are
	 * skipped (WasmVM workers can't handle graceful signals).
	 */
	private onSignal: ((pgid: number, signal: number, excludeLeaders: boolean) => number) | null;
	private nextPtyId = 0;
	private nextPtyDescId = 200_000; // High range to avoid FD/pipe ID collisions
	private log: KernelLogger;

	constructor(
		onSignal?: (pgid: number, signal: number, excludeLeaders: boolean) => number,
		logger?: KernelLogger,
	) {
		this.onSignal = onSignal ?? null;
		this.log = logger ?? noopKernelLogger;
	}

	/**
	 * Allocate a PTY pair. Returns two FileDescriptions:
	 * one for the master and one for the slave.
	 */
	createPty(): { master: PtyEnd; slave: PtyEnd; path: string } {
		const id = this.nextPtyId++;
		const path = `/dev/pts/${id}`;

		const masterDesc: FileDescription = {
			id: this.nextPtyDescId++,
			path: `pty:${id}:master`,
			cursor: 0n,
			flags: O_RDWR,
			refCount: 0, // openWith() will bump
		};

		const slaveDesc: FileDescription = {
			id: this.nextPtyDescId++,
			path: path,
			cursor: 0n,
			flags: O_RDWR,
			refCount: 0, // openWith() will bump
		};

		const state: PtyState = {
			id,
			path,
			masterDescription: masterDesc,
			slaveDescription: slaveDesc,
			inputBuffer: [],
			outputBuffer: [],
			closed: { master: false, slave: false },
			inputWaiters: [],
			outputWaiters: [],
			termios: defaultTermios(),
			lineBuffer: [],
			foregroundPgid: 0,
			sessionLeaderPgid: 0,
		};

		this.ptys.set(id, state);
		this.descToPty.set(masterDesc.id, { ptyId: id, end: "master" });
		this.descToPty.set(slaveDesc.id, { ptyId: id, end: "slave" });
		this.log.debug({ ptyId: id, path, masterDescId: masterDesc.id, slaveDescId: slaveDesc.id }, "PTY created");

		return {
			master: { description: masterDesc, filetype: FILETYPE_CHARACTER_DEVICE },
			slave: { description: slaveDesc, filetype: FILETYPE_CHARACTER_DEVICE },
			path,
		};
	}

	/**
	 * Write data to a PTY end.
	 * Master write → slave can read (input direction).
	 * Slave write → master can read (output direction).
	 */
	write(descriptionId: number, data: Uint8Array): number {
		const ref = this.descToPty.get(descriptionId);
		if (!ref) throw new KernelError("EBADF", "not a PTY end");

		const state = this.ptys.get(ref.ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");

		if (ref.end === "master") {
			// Master write → input direction, processed through line discipline
			if (state.closed.master) throw new KernelError("EIO", "master closed");
			if (state.closed.slave) throw new KernelError("EIO", "slave closed");
			return this.processInput(state, data);
		} else {
			// Slave write → output buffer (master reads)
			// ONLCR: convert \n to \r\n (standard POSIX terminal output processing)
			if (state.closed.slave) throw new KernelError("EIO", "slave closed");
			if (state.closed.master) throw new KernelError("EIO", "master closed");

			const processed = this.processOutput(state, data);
			if (state.outputWaiters.length > 0) {
				const waiter = state.outputWaiters.shift()!;
				waiter(processed);
			} else {
				// Enforce buffer limit to prevent unbounded memory growth
				if (this.bufferBytes(state.outputBuffer) + processed.length > MAX_PTY_BUFFER_BYTES) {
					throw new KernelError("EAGAIN", "PTY output buffer full");
				}
				state.outputBuffer.push(new Uint8Array(processed));
			}
		}

		return data.length;
	}

	/**
	 * Read data from a PTY end.
	 * Master read → data written by slave (output direction).
	 * Slave read → data written by master (input direction).
	 * Returns null on hangup (other end closed).
	 */
	read(descriptionId: number, length: number): Promise<Uint8Array | null> {
		const ref = this.descToPty.get(descriptionId);
		if (!ref) throw new KernelError("EBADF", "not a PTY end");

		const state = this.ptys.get(ref.ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");

		if (ref.end === "master") {
			// Master reads from output buffer (data written by slave)
			if (state.closed.master) throw new KernelError("EIO", "master closed");

			if (state.outputBuffer.length > 0) {
				return Promise.resolve(this.drainBuffer(state.outputBuffer, length));
			}
			// Slave closed → EIO (terminal hangup)
			if (state.closed.slave) {
				return Promise.resolve(null);
			}
			return new Promise((resolve) => {
				state.outputWaiters.push(resolve);
			});
		} else {
			// Slave reads from input buffer (data written by master)
			if (state.closed.slave) throw new KernelError("EIO", "slave closed");

			if (state.inputBuffer.length > 0) {
				return Promise.resolve(this.drainBuffer(state.inputBuffer, length));
			}
			// Master closed → EIO (terminal hangup)
			if (state.closed.master) {
				return Promise.resolve(null);
			}
			return new Promise((resolve) => {
				state.inputWaiters.push(resolve);
			});
		}
	}

	/** Close one end of a PTY. */
	close(descriptionId: number): void {
		const ref = this.descToPty.get(descriptionId);
		if (!ref) return;

		const state = this.ptys.get(ref.ptyId);
		if (!state) return;

		if (ref.end === "master") {
			state.closed.master = true;
			this.log.debug({ ptyId: ref.ptyId, fgPgid: state.foregroundPgid }, "PTY master closed");

			// SIGHUP: when master closes, send SIGHUP to foreground process group
			if (state.foregroundPgid > 0 && this.onSignal) {
				this.log.debug({ ptyId: ref.ptyId, pgid: state.foregroundPgid, signal: 1 }, "PTY SIGHUP delivery");
				try {
					this.onSignal(state.foregroundPgid, 1 /* SIGHUP */, false);
				} catch {
					// Signal delivery failure must not break PTY cleanup
				}
			}

			// Notify blocked slave readers with null (EIO / hangup)
			for (const waiter of state.inputWaiters) {
				waiter(null);
			}
			state.inputWaiters.length = 0;
			// Resolve any pending master reads (same-end close → EOF)
			for (const waiter of state.outputWaiters) {
				waiter(null);
			}
			state.outputWaiters.length = 0;
		} else {
			state.closed.slave = true;
			// Notify blocked master readers with null (EIO / hangup)
			for (const waiter of state.outputWaiters) {
				waiter(null);
			}
			state.outputWaiters.length = 0;
			// Resolve any pending slave reads (same-end close → EOF)
			for (const waiter of state.inputWaiters) {
				waiter(null);
			}
			state.inputWaiters.length = 0;
		}

		this.descToPty.delete(descriptionId);

		// Clean up when both ends closed
		if (state.closed.master && state.closed.slave) {
			this.ptys.delete(ref.ptyId);
		}
	}

	/** Check if a description ID belongs to a PTY. */
	isPty(descriptionId: number): boolean {
		return this.descToPty.has(descriptionId);
	}

	/** Check if a description ID is a PTY slave (terminal). */
	isSlave(descriptionId: number): boolean {
		const ref = this.descToPty.get(descriptionId);
		return ref?.end === "slave";
	}

	/**
	 * Allocate PTY FDs in the given FD table.
	 * Returns master/slave FD numbers and the /dev/pts/N path.
	 */
	createPtyFDs(fdTable: ProcessFDTable): { masterFd: number; slaveFd: number; path: string } {
		const { master, slave, path } = this.createPty();
		const masterFd = fdTable.openWith(master.description, master.filetype);
		const slaveFd = fdTable.openWith(slave.description, slave.filetype);
		return { masterFd, slaveFd, path };
	}

	/** Set line discipline options for the PTY containing this description. */
	setDiscipline(
		descriptionId: number,
		config: Partial<LineDisciplineConfig>,
	): void {
		const ptyId = this.getPtyId(descriptionId);
		const state = this.ptys.get(ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");

		if (config.canonical !== undefined) state.termios.icanon = config.canonical;
		if (config.echo !== undefined) state.termios.echo = config.echo;
		if (config.isig !== undefined) state.termios.isig = config.isig;
	}

	/** Set the foreground process group for signal delivery on this PTY. */
	setForegroundPgid(descriptionId: number, pgid: number): void {
		const ptyId = this.getPtyId(descriptionId);
		const state = this.ptys.get(ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");
		this.log.trace({ ptyId, pgid, prev: state.foregroundPgid }, "PTY set foreground pgid");
		state.foregroundPgid = pgid;
	}

	/** Set the session leader pgid for SIGINT interception on this PTY. */
	setSessionLeader(descriptionId: number, pgid: number): void {
		const ptyId = this.getPtyId(descriptionId);
		const state = this.ptys.get(ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");
		this.log.trace({ ptyId, pgid }, "PTY set session leader");
		state.sessionLeaderPgid = pgid;
	}

	/** Get terminal attributes for the PTY containing this description. */
	getTermios(descriptionId: number): Termios {
		const ptyId = this.getPtyId(descriptionId);
		const state = this.ptys.get(ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");
		return {
			icrnl: state.termios.icrnl,
			opost: state.termios.opost,
			onlcr: state.termios.onlcr,
			icanon: state.termios.icanon,
			echo: state.termios.echo,
			isig: state.termios.isig,
			cc: { ...state.termios.cc },
		};
	}

	/** Set terminal attributes for the PTY containing this description. */
	setTermios(descriptionId: number, termios: Partial<Termios>): void {
		const ptyId = this.getPtyId(descriptionId);
		const state = this.ptys.get(ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");
		this.log.trace({ ptyId, termios }, "PTY setTermios");

		if (termios.icrnl !== undefined) state.termios.icrnl = termios.icrnl;
		if (termios.opost !== undefined) state.termios.opost = termios.opost;
		if (termios.onlcr !== undefined) state.termios.onlcr = termios.onlcr;
		if (termios.icanon !== undefined) state.termios.icanon = termios.icanon;
		if (termios.echo !== undefined) state.termios.echo = termios.echo;
		if (termios.isig !== undefined) state.termios.isig = termios.isig;
		if (termios.cc) Object.assign(state.termios.cc, termios.cc);
	}

	/** Get the foreground process group for the PTY containing this description. */
	getForegroundPgid(descriptionId: number): number {
		const ptyId = this.getPtyId(descriptionId);
		const state = this.ptys.get(ptyId);
		if (!state) throw new KernelError("EBADF", "PTY not found");
		return state.foregroundPgid;
	}

	/** Get the PTY ID from a description ID. */
	private getPtyId(descriptionId: number): number {
		const ref = this.descToPty.get(descriptionId);
		if (!ref) throw new KernelError("EBADF", "not a PTY end");
		return ref.ptyId;
	}

	// -------------------------------------------------------------------
	// Output processing (ONLCR)
	// -------------------------------------------------------------------

	/** Convert lone \n to \r\n in output data (POSIX ONLCR). Skipped when opost/onlcr disabled. */
	private processOutput(state: PtyState, data: Uint8Array): Uint8Array {
		// Skip output processing when opost or onlcr is off
		if (!state.termios.opost || !state.termios.onlcr) return data;

		// Fast path: no newlines → return as-is
		if (!data.includes(0x0a)) return data;

		// Count lone LFs (not preceded by CR) to size the result buffer
		let extraCRs = 0;
		for (let i = 0; i < data.length; i++) {
			if (data[i] === 0x0a && (i === 0 || data[i - 1] !== 0x0d)) {
				extraCRs++;
			}
		}
		if (extraCRs === 0) return data;

		const result = new Uint8Array(data.length + extraCRs);
		let j = 0;
		for (let i = 0; i < data.length; i++) {
			if (data[i] === 0x0a && (i === 0 || data[i - 1] !== 0x0d)) {
				result[j++] = 0x0d; // CR
			}
			result[j++] = data[i];
		}
		return result;
	}

	// -------------------------------------------------------------------
	// Line discipline input processing
	// -------------------------------------------------------------------

	/**
	 * Process input data through line discipline.
	 * Master writes go through here before reaching the slave's input buffer.
	 */
	private processInput(state: PtyState, data: Uint8Array): number {
		const { termios } = state;

		// Raw-mode input still applies ICRNL, but it must deliver atomically so
		// oversized writes fail without partially filling the input buffer.
		if (!termios.icanon && !termios.echo && !termios.isig) {
			this.deliverInput(state, this.applyInputTranslations(termios.icrnl, data));
			return data.length;
		}

		// Process byte by byte through discipline
		for (let byte of data) {
			// ICRNL: convert CR (0x0d) to NL (0x0a) before all other processing
			if (termios.icrnl && byte === 0x0d) byte = 0x0a;
			// Signal character handling (requires isig)
			if (termios.isig) {
				const signal = this.signalForByte(state, byte);
				if (signal !== null) {
					this.log.debug({ ptyId: state.id, signal, fgPgid: state.foregroundPgid, sessionLeader: state.sessionLeaderPgid }, "PTY signal char detected");
					if (termios.icanon) state.lineBuffer.length = 0;

					// Session-leader SIGINT interception: echo ^C, protect
					// the shell, and inject a newline to trigger a fresh prompt
					// when no children are running.
					if (
						signal === 2 &&
						state.sessionLeaderPgid > 0 &&
						state.foregroundPgid === state.sessionLeaderPgid
					) {
						// Echo ^C + newline so the user sees the interruption
						if (termios.echo) {
							this.echoOutput(state, new Uint8Array([0x5e, 0x43, 0x0d, 0x0a]));
						}

						// Kill children in the group (session leader is skipped).
						// Returns count of non-leader processes signaled.
						let childrenKilled = 0;
						if (state.foregroundPgid > 0) {
							try {
								childrenKilled = this.onSignal?.(state.foregroundPgid, signal, true) ?? 0;
							} catch {
								// Signal delivery failure must not break line discipline
							}
						}
						this.log.debug({ ptyId: state.id, childrenKilled, pgid: state.foregroundPgid }, "PTY session-leader SIGINT interception");

						// No children running → shell is at the prompt blocking on
						// fdRead. Inject a newline to unblock it and trigger a
						// fresh prompt.
						if (childrenKilled === 0) {
							this.deliverInput(state, new Uint8Array([0x0a]));
						}
						continue;
					}

					// Echo ^Z for SIGTSTP
					if (signal === 20 && termios.echo) {
						this.echoOutput(state, new Uint8Array([0x5e, 0x5a, 0x0d, 0x0a]));
					}
					// Echo ^\ for SIGQUIT
					if (signal === 3 && termios.echo) {
						this.echoOutput(state, new Uint8Array([0x5e, 0x5c, 0x0d, 0x0a]));
					}
					// Normal signal delivery (non-SIGINT or non-session-leader)
					if (state.foregroundPgid > 0) {
						this.log.debug({ ptyId: state.id, signal, pgid: state.foregroundPgid }, "PTY signal delivery to foreground group");
						try {
							this.onSignal?.(state.foregroundPgid, signal, false);
						} catch {
							// Signal delivery failure must not break line discipline
						}
					}
					continue;
				}
			}

			if (termios.icanon) {
				// EOF char: flush or signal EOF
				if (byte === termios.cc.veof) {
					if (state.lineBuffer.length === 0) {
						this.deliverInput(state, new Uint8Array(0));
					} else {
						this.deliverInput(state, new Uint8Array(state.lineBuffer));
						state.lineBuffer.length = 0;
					}
					continue;
				}

				// Erase char: erase last char
				if (byte === termios.cc.verase || byte === 0x08) {
					if (state.lineBuffer.length > 0) {
						state.lineBuffer.pop();
						if (termios.echo) {
							this.echoOutput(state, new Uint8Array([0x08, 0x20, 0x08]));
						}
					}
					continue;
				}

				// Newline: flush line (echo CR+LF for correct cursor positioning)
				if (byte === 0x0a) {
					state.lineBuffer.push(0x0a);
					if (termios.echo) this.echoOutput(state, new Uint8Array([0x0d, 0x0a]));
					this.deliverInput(state, new Uint8Array(state.lineBuffer));
					state.lineBuffer.length = 0;
					continue;
				}

				// Regular char: buffer (capped at MAX_CANON to prevent unbounded growth)
				if (state.lineBuffer.length >= MAX_CANON) continue;
				state.lineBuffer.push(byte);
				if (termios.echo) this.echoOutput(state, new Uint8Array([byte]));
			} else {
				// Raw mode: deliver immediately
				if (termios.echo) this.echoOutput(state, new Uint8Array([byte]));
				this.deliverInput(state, new Uint8Array([byte]));
			}
		}

		return data.length;
	}

	private applyInputTranslations(icrnl: boolean, data: Uint8Array): Uint8Array {
		if (!icrnl || !data.includes(0x0d)) return data;

		const translated = new Uint8Array(data.length);
		for (let i = 0; i < data.length; i++) {
			translated[i] = data[i] === 0x0d ? 0x0a : data[i];
		}
		return translated;
	}

	/** Deliver input data to slave (input buffer / waiters). */
	private deliverInput(state: PtyState, data: Uint8Array): void {
		if (state.inputWaiters.length > 0) {
			const waiter = state.inputWaiters.shift()!;
			waiter(data);
		} else {
			// Enforce buffer limit to prevent unbounded memory growth
			if (this.bufferBytes(state.inputBuffer) + data.length > MAX_PTY_BUFFER_BYTES) {
				throw new KernelError("EAGAIN", "PTY input buffer full");
			}
			state.inputBuffer.push(new Uint8Array(data));
		}
	}

	/** Echo data to output (master reads it back for display). Throws EAGAIN when buffer is full. */
	private echoOutput(state: PtyState, data: Uint8Array): void {
		if (state.outputWaiters.length > 0) {
			const waiter = state.outputWaiters.shift()!;
			waiter(data);
		} else {
			if (this.bufferBytes(state.outputBuffer) + data.length > MAX_PTY_BUFFER_BYTES) {
				throw new KernelError("EAGAIN", "PTY output buffer full (echo backpressure)");
			}
			state.outputBuffer.push(new Uint8Array(data));
		}
	}

	/** Map control byte to signal number using termios cc, or null if not a signal char. */
	private signalForByte(state: PtyState, byte: number): number | null {
		const { cc } = state.termios;
		if (byte === cc.vintr) return 2;   // SIGINT
		if (byte === cc.vsusp) return 20;  // SIGTSTP
		if (byte === cc.vquit) return 3;   // SIGQUIT
		return null;
	}

	private bufferBytes(buffer: Uint8Array[]): number {
		let size = 0;
		for (const chunk of buffer) size += chunk.length;
		return size;
	}

	private drainBuffer(buffer: Uint8Array[], length: number): Uint8Array {
		const chunks: Uint8Array[] = [];
		let remaining = length;

		while (remaining > 0 && buffer.length > 0) {
			const chunk = buffer[0];
			if (chunk.length <= remaining) {
				chunks.push(chunk);
				remaining -= chunk.length;
				buffer.shift();
			} else {
				chunks.push(chunk.subarray(0, remaining));
				buffer[0] = chunk.subarray(remaining);
				remaining = 0;
			}
		}

		if (chunks.length === 1) return chunks[0];

		const total = chunks.reduce((sum, c) => sum + c.length, 0);
		const result = new Uint8Array(total);
		let offset = 0;
		for (const chunk of chunks) {
			result.set(chunk, offset);
			offset += chunk.length;
		}
		return result;
	}
}
