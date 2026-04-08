/**
 * Process table.
 *
 * Universal process tracking across all runtimes. Owns PID allocation,
 * parent-child relationships, waitpid, and signal routing. A WasmVM
 * shell can waitpid on a Node child process.
 */

import type { DriverProcess, ProcessContext, ProcessEntry, ProcessInfo, SignalHandler, ProcessSignalState, KernelLogger } from "./types.js";
import { KernelError, SIGCHLD, SIGALRM, SIGCONT, SIGSTOP, SIGTSTP, SIGKILL, SIGWINCH, WNOHANG, SA_RESTART, SA_RESETHAND, SIG_BLOCK, SIG_UNBLOCK, SIG_SETMASK, noopKernelLogger } from "./types.js";
import { WaitQueue } from "./wait.js";
import { encodeExitStatus, encodeSignalStatus } from "./wstatus.js";

const ZOMBIE_TTL_MS = 60_000;

export class ProcessTable {
	private entries: Map<number, ProcessEntry> = new Map();
	private nextPid = 1;
	private waiters: Map<number, Array<(info: { pid: number; status: number; termSignal: number }) => void>> = new Map();
	private zombieTimers: Map<number, ReturnType<typeof setTimeout>> = new Map();
	/** Pending alarm timers per PID: { timer, scheduledAt (ms epoch) }. */
	private alarmTimers: Map<number, { timer: ReturnType<typeof setTimeout>; scheduledAt: number; seconds: number }> = new Map();
	private log: KernelLogger;

	/** Called when a process exits, before waiters are notified. */
	onProcessExit: ((pid: number) => void) | null = null;

	/** Called when a zombie process is reaped (removed from the table). */
	onProcessReap: ((pid: number) => void) | null = null;

	constructor(logger?: KernelLogger) {
		this.log = logger ?? noopKernelLogger;
	}

	/** Atomically allocate the next PID. */
	allocatePid(): number {
		return this.nextPid++;
	}

	/** Register a process with a pre-allocated PID. */
	register(
		pid: number,
		driver: string,
		command: string,
		args: string[],
		ctx: ProcessContext,
		driverProcess: DriverProcess,
	): ProcessEntry {
		// Inherit pgid/sid/umask from parent, or default to own pid / 0o022
		const parent = ctx.ppid ? this.entries.get(ctx.ppid) : undefined;
		const pgid = parent?.pgid ?? pid;
		const sid = parent?.sid ?? pid;
		const umask = parent?.umask ?? 0o022;

		const entry: ProcessEntry = {
			pid,
			ppid: ctx.ppid,
			pgid,
			sid,
			driver,
			command,
			args,
			status: "running",
			exitCode: null,
			exitReason: null,
			termSignal: 0,
			startTime: Date.now(),
			exitTime: null,
			env: { ...ctx.env },
			cwd: ctx.cwd,
			umask,
			activeHandles: new Map(),
			handleLimit: 0,
			signalState: {
				handlers: new Map(),
				blockedSignals: new Set(),
				pendingSignals: new Set(),
				signalWaiters: new WaitQueue(),
				deliverySeq: 0,
				lastDeliveredSignal: null,
				lastDeliveredFlags: 0,
			},
			driverProcess,
		};
		this.entries.set(pid, entry);
		this.log.debug({ pid, ppid: ctx.ppid, pgid, sid, driver, command, args }, "process registered");

		// Wire up exit callback to mark process as exited
		driverProcess.onExit = (code: number) => {
			this.markExited(pid, code);
		};

		return entry;
	}

	get(pid: number): ProcessEntry | undefined {
		return this.entries.get(pid);
	}

	/** Count pending zombie cleanup timers (test observability). */
	get zombieTimerCount(): number {
		return this.zombieTimers.size;
	}

	/** Count running (non-exited) processes. */
	runningCount(): number {
		let count = 0;
		for (const entry of this.entries.values()) {
			if (entry.status === "running") count++;
		}
		return count;
	}

	/** Mark a process as exited with the given code. Notifies waiters. */
	markExited(pid: number, exitCode: number): void {
		const entry = this.entries.get(pid);
		if (!entry) return;
		if (entry.status === "exited") return;

		this.log.debug({
			pid, exitCode, command: entry.command,
			termSignal: entry.termSignal,
			reason: entry.termSignal > 0 ? "signal" : "normal",
		}, "process exited");
		entry.status = "exited";
		entry.exitCode = exitCode;
		entry.exitReason = entry.termSignal > 0 ? "signal" : "normal";
		entry.exitTime = Date.now();

		// Encode POSIX wstatus
		const wstatus = entry.termSignal > 0
			? encodeSignalStatus(entry.termSignal)
			: encodeExitStatus(exitCode);

		// Cancel pending alarm
		this.cancelAlarm(pid);

		// Clear all active handles
		entry.activeHandles.clear();

		// Clean up process resources (FD table, pipe ends)
		this.onProcessExit?.(pid);

		// Deliver SIGCHLD to parent via signal handler system
		if (entry.ppid > 0) {
			const parent = this.entries.get(entry.ppid);
			if (parent && parent.status === "running") {
				this.deliverSignal(parent, SIGCHLD);
			}
		}

		// Notify waiters
		const waiters = this.waiters.get(pid);
		if (waiters) {
			for (const resolve of waiters) {
				resolve({ pid, status: wstatus, termSignal: entry.termSignal });
			}
			this.waiters.delete(pid);
		}

		// Schedule zombie cleanup (tracked for cancellation on dispose)
		const timer = setTimeout(() => {
			this.zombieTimers.delete(pid);
			this.reap(pid);
		}, ZOMBIE_TTL_MS);
		this.zombieTimers.set(pid, timer);
	}

	/**
	 * Wait for a process to exit.
	 * If already exited, resolves immediately. Otherwise blocks until exit.
	 * With WNOHANG option, returns null immediately if process is still running.
	 */
	waitpid(pid: number, options?: number): Promise<{ pid: number; status: number; termSignal: number } | null> {
		const entry = this.entries.get(pid);
		if (!entry) {
			return Promise.reject(new Error(`ESRCH: no such process ${pid}`));
		}

		if (entry.status === "exited") {
			const wstatus = entry.termSignal > 0
				? encodeSignalStatus(entry.termSignal)
				: encodeExitStatus(entry.exitCode!);
			return Promise.resolve({ pid, status: wstatus, termSignal: entry.termSignal });
		}

		// WNOHANG: return null immediately if process is still running
		if (options && (options & WNOHANG)) {
			return Promise.resolve(null);
		}

		return new Promise((resolve) => {
			let waiters = this.waiters.get(pid);
			if (!waiters) {
				waiters = [];
				this.waiters.set(pid, waiters);
			}
			waiters.push(resolve);
		});
	}

	/**
	 * Send a signal to a process or process group.
	 * If pid > 0, signal a single process.
	 * If pid < 0, signal all processes in process group abs(pid).
	 */
	kill(pid: number, signal: number): void {
		// Validate signal range (POSIX: 0 = existence check, 1-64 = real signals)
		if (signal < 0 || signal > 64) {
			throw new KernelError("EINVAL", `invalid signal ${signal}`);
		}

		this.log.debug({ pid, signal }, "kill");

		if (pid < 0) {
			// Process group kill
			const pgid = -pid;
			let found = false;
			for (const entry of this.entries.values()) {
				if (entry.pgid === pgid && entry.status !== "exited") {
					found = true;
					if (signal !== 0) {
						this.deliverSignal(entry, signal);
					}
				}
			}
			if (!found) throw new KernelError("ESRCH", `no such process group ${pgid}`);
			return;
		}
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		if (entry.status === "exited") return;
		// Signal 0: existence check only — don't deliver
		if (signal === 0) return;
		this.deliverSignal(entry, signal);
	}

	/**
	 * Deliver a signal to a process, respecting handlers, blocking, and coalescing.
	 *
	 * SIGKILL and SIGSTOP cannot be caught, blocked, or ignored (POSIX).
	 * Blocked signals are queued in pendingSignals; standard signals (1-31) coalesce.
	 * If a handler is registered, it is invoked with sa_mask temporarily blocked.
	 */
	private deliverSignal(entry: ProcessEntry, signal: number): void {
		const { signalState } = entry;
		this.log.trace({ pid: entry.pid, signal, command: entry.command }, "deliver signal");

		// SIGKILL and SIGSTOP always use default action — cannot be caught/blocked/ignored
		if (signal === SIGKILL || signal === SIGSTOP) {
			this.applyDefaultAction(entry, signal);
			return;
		}

		// SIGCONT always resumes a stopped process, even if blocked or caught (POSIX)
		if (signal === SIGCONT) {
			this.cont(entry.pid);
			// If blocked, queue for handler delivery later; otherwise dispatch
			if (signalState.blockedSignals.has(signal)) {
				signalState.pendingSignals.add(signal);
				return;
			}
			this.dispatchSignal(entry, signal);
			return;
		}

		// If signal is blocked, queue it (standard signals 1-31 coalesce via Set)
		if (signalState.blockedSignals.has(signal)) {
			signalState.pendingSignals.add(signal);
			return;
		}

		this.dispatchSignal(entry, signal);
	}

	/**
	 * Dispatch a signal to a process — check handler, then apply.
	 * Called for unblocked signals and when delivering pending signals.
	 */
	private dispatchSignal(entry: ProcessEntry, signal: number): void {
		const { signalState } = entry;
		const registration = signalState.handlers.get(signal);

		if (!registration) {
			// No handler registered — apply default action
			if (signal !== SIGCHLD) {
				this.recordSignalDelivery(signalState, signal, 0);
			}
			this.applyDefaultAction(entry, signal);
			return;
		}

		const { handler, mask, flags } = registration;

		if (handler === "ignore") return;

		if (handler === "default") {
			if (signal !== SIGCHLD) {
				this.recordSignalDelivery(signalState, signal, 0);
			}
			this.applyDefaultAction(entry, signal);
			return;
		}

		this.recordSignalDelivery(signalState, signal, flags);

		// User-defined handler: temporarily block sa_mask + the signal itself during execution
		const savedBlocked = new Set(signalState.blockedSignals);
		for (const s of mask) signalState.blockedSignals.add(s);
		signalState.blockedSignals.add(signal);

		try {
			handler(signal);
		} finally {
			// Restore previous blocked set
			signalState.blockedSignals = savedBlocked;
		}

		// Reset one-shot handlers before any pending re-delivery.
		if ((flags & SA_RESETHAND) !== 0) {
			signalState.handlers.set(signal, {
				handler: "default",
				mask: new Set(),
				flags: 0,
			});
		}

		// Deliver any signals that were pending and are now unblocked
		this.deliverPendingSignals(entry);
	}

	/** Wake signal-aware waiters after a signal has been dispatched. */
	private recordSignalDelivery(signalState: ProcessSignalState, signal: number, flags: number): void {
		signalState.lastDeliveredSignal = signal;
		signalState.lastDeliveredFlags = flags;
		signalState.deliverySeq++;
		signalState.signalWaiters.wakeAll();
	}

	/** Apply the kernel default action for a signal. */
	private applyDefaultAction(entry: ProcessEntry, signal: number): void {
		if (signal === SIGTSTP || signal === SIGSTOP) {
			this.log.debug({ pid: entry.pid, signal, action: "stop" }, "signal default action");
			this.stop(entry.pid);
			entry.driverProcess.kill(signal);
		} else if (signal === SIGCONT) {
			this.log.debug({ pid: entry.pid, signal, action: "continue" }, "signal default action");
			this.cont(entry.pid);
			entry.driverProcess.kill(signal);
		} else if (signal === SIGCHLD || signal === SIGWINCH) {
			// Default action: ignore (POSIX — SIGCHLD and SIGWINCH don't terminate)
			return;
		} else {
			this.log.debug({ pid: entry.pid, signal, action: "terminate", command: entry.command }, "signal default action");
			entry.termSignal = signal;
			entry.driverProcess.kill(signal);
		}
	}

	/** Deliver pending signals that are no longer blocked (lowest signal number first). */
	private deliverPendingSignals(entry: ProcessEntry): void {
		const { signalState } = entry;
		if (signalState.pendingSignals.size === 0) return;

		// Deliver in ascending signal number order
		const pending = [...signalState.pendingSignals].sort((a, b) => a - b);
		for (const sig of pending) {
			// Check both: not blocked AND still pending (recursive delivery may have handled it)
			if (!signalState.blockedSignals.has(sig) && signalState.pendingSignals.has(sig)) {
				signalState.pendingSignals.delete(sig);
				this.dispatchSignal(entry, sig);
				if (entry.status === "exited") break;
			}
		}
	}

	/**
	 * Schedule SIGALRM delivery after `seconds`. Returns previous alarm remaining (0 if none).
	 * alarm(pid, 0) cancels any pending alarm. A new alarm replaces the previous one.
	 */
	alarm(pid: number, seconds: number): number {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);

		// Calculate remaining time from any existing alarm
		let remaining = 0;
		const existing = this.alarmTimers.get(pid);
		if (existing) {
			const elapsed = (Date.now() - existing.scheduledAt) / 1000;
			remaining = Math.max(0, Math.ceil(existing.seconds - elapsed));
			clearTimeout(existing.timer);
			this.alarmTimers.delete(pid);
		}

		if (seconds === 0) return remaining;

		// Schedule new alarm
		const scheduledAt = Date.now();
		const timer = setTimeout(() => {
			this.alarmTimers.delete(pid);
			const e = this.entries.get(pid);
			if (!e || e.status !== "running") return;

			// Deliver through signal handler system
			this.deliverSignal(e, SIGALRM);
		}, seconds * 1000);
		this.alarmTimers.set(pid, { timer, scheduledAt, seconds });

		return remaining;
	}

	// -----------------------------------------------------------------------
	// Signal handlers (sigaction / sigprocmask)
	// -----------------------------------------------------------------------

	/**
	 * Register a signal handler (POSIX sigaction).
	 * Returns the previous handler for the signal, or undefined if none was set.
	 * SIGKILL and SIGSTOP cannot be caught or ignored.
	 */
	sigaction(pid: number, signal: number, handler: SignalHandler): SignalHandler | undefined {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		if (signal < 1 || signal > 64) throw new KernelError("EINVAL", `invalid signal ${signal}`);
		if (signal === SIGKILL || signal === SIGSTOP) {
			throw new KernelError("EINVAL", `cannot catch or ignore signal ${signal}`);
		}

		const prev = entry.signalState.handlers.get(signal);
		entry.signalState.handlers.set(signal, handler);
		return prev;
	}

	/**
	 * Modify the blocked signal mask (POSIX sigprocmask).
	 * Returns the previous blocked set.
	 * SIGKILL and SIGSTOP cannot be blocked.
	 */
	sigprocmask(pid: number, how: number, set: Set<number>): Set<number> {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);

		const { signalState } = entry;
		const prevBlocked = new Set(signalState.blockedSignals);

		// Filter out uncatchable signals
		const filtered = new Set(set);
		filtered.delete(SIGKILL);
		filtered.delete(SIGSTOP);

		if (how === SIG_BLOCK) {
			for (const s of filtered) signalState.blockedSignals.add(s);
		} else if (how === SIG_UNBLOCK) {
			for (const s of filtered) signalState.blockedSignals.delete(s);
		} else if (how === SIG_SETMASK) {
			signalState.blockedSignals = filtered;
		} else {
			throw new KernelError("EINVAL", `invalid sigprocmask how: ${how}`);
		}

		// Deliver any pending signals that are now unblocked
		this.deliverPendingSignals(entry);

		return prevBlocked;
	}

	/** Get the signal state for a process (read-only view). */
	getSignalState(pid: number): ProcessSignalState {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		return entry.signalState;
	}

	/** Suspend a process (SIGTSTP/SIGSTOP). Sets status to 'stopped'. */
	stop(pid: number): void {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		if (entry.status !== "running") return;
		entry.status = "stopped";
	}

	/** Resume a stopped process (SIGCONT). Sets status back to 'running'. */
	cont(pid: number): void {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		if (entry.status !== "stopped") return;
		entry.status = "running";
	}

	/** Cancel a pending alarm for a process. */
	private cancelAlarm(pid: number): void {
		const existing = this.alarmTimers.get(pid);
		if (existing) {
			clearTimeout(existing.timer);
			this.alarmTimers.delete(pid);
		}
	}

	/** Set process group ID. Process can join existing group or create new one. */
	setpgid(pid: number, pgid: number): void {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);

		// pgid 0 means "use own PID as pgid"
		const targetPgid = pgid === 0 ? pid : pgid;

		// Can only join an existing group or create own group
		if (targetPgid !== pid) {
			let groupExists = false;
			for (const e of this.entries.values()) {
				if (e.pgid === targetPgid && e.status !== "exited") {
					// Reject cross-session group joining (POSIX)
					if (e.sid !== entry.sid) {
						throw new KernelError("EPERM", `cannot join process group in different session`);
					}
					groupExists = true;
					break;
				}
			}
			if (!groupExists) throw new KernelError("EPERM", `no such process group ${targetPgid}`);
		}

		entry.pgid = targetPgid;
	}

	/** Get process group ID. */
	getpgid(pid: number): number {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		return entry.pgid;
	}

	/** Create a new session. Process becomes session leader and process group leader. */
	setsid(pid: number): number {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);

		// Process must not already be a process group leader
		if (entry.pgid === pid) {
			throw new KernelError("EPERM", `process ${pid} is already a process group leader`);
		}

		entry.sid = pid;
		entry.pgid = pid;
		return pid;
	}

	/** Get session ID. */
	getsid(pid: number): number {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		return entry.sid;
	}

	/** Get the parent PID for a process. */
	getppid(pid: number): number {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		return entry.ppid;
	}

	/**
	 * Send a signal to a process group, skipping session leaders.
	 * Returns count of processes actually signaled.
	 * Used for PTY-originated SIGINT where the session leader (shell)
	 * cannot handle signals gracefully (WasmVM worker.terminate()).
	 */
	killGroupExcludeLeaders(pgid: number, signal: number): number {
		if (signal < 0 || signal > 64) {
			throw new KernelError("EINVAL", `invalid signal ${signal}`);
		}
		let count = 0;
		for (const entry of this.entries.values()) {
			if (entry.pgid === pgid && entry.status !== "exited") {
				if (entry.pid === entry.sid) continue; // Skip session leaders
				if (signal !== 0) {
					this.deliverSignal(entry, signal);
				}
				count++;
			}
		}
		return count;
	}

	/** Check if any running process belongs to the given process group. */
	hasProcessGroup(pgid: number): boolean {
		for (const entry of this.entries.values()) {
			if (entry.pgid === pgid && entry.status !== "exited") return true;
		}
		return false;
	}

	/** Get a read-only view of process info for all processes. */
	listProcesses(): Map<number, ProcessInfo> {
		const result = new Map<number, ProcessInfo>();
		for (const [pid, entry] of this.entries) {
			result.set(pid, {
				pid: entry.pid,
				ppid: entry.ppid,
				pgid: entry.pgid,
				sid: entry.sid,
				driver: entry.driver,
				command: entry.command,
				args: entry.args,
				cwd: entry.cwd,
				status: entry.status,
				exitCode: entry.exitCode,
				startTime: entry.startTime,
				exitTime: entry.exitTime,
			});
		}
		return result;
	}

	/** Remove a zombie process. */
	private reap(pid: number): void {
		const entry = this.entries.get(pid);
		if (entry?.status === "exited") {
			this.onProcessReap?.(pid);
			this.entries.delete(pid);
		}
	}

	// -----------------------------------------------------------------------
	// Handle tracking
	// -----------------------------------------------------------------------

	/** Register an active handle for a process. Throws EAGAIN if budget exceeded. */
	registerHandle(pid: number, id: string, description: string): void {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		if (entry.handleLimit > 0 && entry.activeHandles.size >= entry.handleLimit) {
			throw new KernelError("EAGAIN", `handle limit (${entry.handleLimit}) exceeded for process ${pid}`);
		}
		entry.activeHandles.set(id, description);
	}

	/** Unregister an active handle. Throws EBADF if handle not found. */
	unregisterHandle(pid: number, id: string): void {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		if (!entry.activeHandles.delete(id)) {
			throw new KernelError("EBADF", `no such handle ${id} for process ${pid}`);
		}
	}

	/** Set the maximum number of active handles for a process. 0 = unlimited. */
	setHandleLimit(pid: number, limit: number): void {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		entry.handleLimit = limit;
	}

	/** Get the active handles for a process (read-only copy). */
	getHandles(pid: number): Map<string, string> {
		const entry = this.entries.get(pid);
		if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
		return new Map(entry.activeHandles);
	}

	/** Terminate all running processes and clear pending timers. */
	async terminateAll(): Promise<void> {
		// Clear all zombie cleanup timers to prevent post-dispose firings
		for (const timer of this.zombieTimers.values()) {
			clearTimeout(timer);
		}
		this.zombieTimers.clear();

		// Clear all pending alarm timers
		for (const { timer } of this.alarmTimers.values()) {
			clearTimeout(timer);
		}
		this.alarmTimers.clear();

		const running = [...this.entries.values()].filter(
			(e) => e.status !== "exited",
		);
		for (const entry of running) {
			try {
				entry.driverProcess.kill(15); // SIGTERM
			} catch {
				// Best effort
			}
		}
		// Wait briefly for graceful exits
		await Promise.allSettled(
			running.map((e) =>
				Promise.race([
					e.driverProcess.wait(),
					new Promise((r) => setTimeout(r, 1000)),
				]),
			),
		);

		// Escalate to SIGKILL for processes that survived SIGTERM
		const survivors = running.filter((e) => e.status !== "exited");
		for (const entry of survivors) {
			try {
				entry.driverProcess.kill(9); // SIGKILL
			} catch {
				// Best effort
			}
		}
		if (survivors.length > 0) {
			await Promise.allSettled(
				survivors.map((e) =>
					Promise.race([
						e.driverProcess.wait(),
						new Promise((r) => setTimeout(r, 500)),
					]),
				),
			);
		}
	}
}
