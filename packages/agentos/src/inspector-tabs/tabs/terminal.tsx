// Interactive PTY shells in the VM, rendered with Ghostty's VT parser (WASM)
// behind an xterm.js-shaped API. Input is raw passthrough: the kernel's line
// discipline owns echo, editing, and signal generation, so a local-echo layer
// would double-print and break raw-mode apps (vim, reedline).
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FitAddon, init as initGhostty, Terminal } from "ghostty-web";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
	ActionErrorNote,
	AgentOsEmpty,
	IconButton,
	PlusIcon,
	UnsupportedAction,
} from "../common";
import { cn } from "../lib/cn";
import { useAgentOsActor } from "../lib/rivet";
import { agentOsSource, decodeActionBytes, shellsQueryOptions } from "../lib/source";
import type { ShellDataPayload, ShellExitPayload, ShellInfo } from "../lib/types";
import { VmBootGate } from "../vm-boot-gate";
import { VmStatusBadges } from "../vm-status-badges";
import React from "react";

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
const SCROLLBACK_LINES = 5_000;
const MAX_SHELLS = 8;
// Output that arrives between `openShell` resolving and the pane mounting is
// held here. Bounded: a shell that floods before its pane exists drops the
// oldest bytes rather than growing without limit.
const MAX_PENDING_BYTES = 256 * 1024;
// Unsent PTY input held while an earlier write is in flight.
const MAX_PENDING_INPUT_CHARS = 1024 * 1024;

/** Ghostty's WASM module is process-wide and loads once. `init()` is
 * idempotent, but keeping one promise avoids racing instantiations. */
let ghosttyReady: Promise<void> | undefined;
function loadGhostty(): Promise<void> {
	ghosttyReady ??= initGhostty();
	return ghosttyReady;
}

function useGhostty(): { ready: boolean; error: unknown } {
	const [state, setState] = useState<{ ready: boolean; error: unknown }>({
		ready: false,
		error: null,
	});
	useEffect(() => {
		let cancelled = false;
		loadGhostty().then(
			() => !cancelled && setState({ ready: true, error: null }),
			(error) => !cancelled && setState({ ready: false, error }),
		);
		return () => {
			cancelled = true;
		};
	}, []);
	return state;
}

// ── Output router ──────────────────────────────────────────────────────────
/** Routes `shellData` broadcasts to whichever pane owns that shell, buffering
 * for shells whose pane has not mounted yet. Lives in a ref (not state): PTY
 * output must never drive a React render. */
interface PendingChunk {
	seq: number;
	bytes: Uint8Array;
}

class OutputRouter {
	private writers = new Map<string, (bytes: Uint8Array) => void>();
	private pending = new Map<string, PendingChunk[]>();

	push(shellId: string, seq: number, bytes: Uint8Array): void {
		const writer = this.writers.get(shellId);
		if (writer) {
			writer(bytes);
			return;
		}
		const buffered = this.pending.get(shellId) ?? [];
		buffered.push({ seq, bytes });
		let total = buffered.reduce((sum, chunk) => sum + chunk.bytes.length, 0);
		while (total > MAX_PENDING_BYTES && buffered.length > 1) {
			total -= buffered.shift()?.bytes.length ?? 0;
		}
		this.pending.set(shellId, buffered);
	}

	/** Attaches a writer and flushes what it missed. `sinceSeq` drops chunks a
	 * server snapshot already painted; the actor stamps every broadcast from the
	 * same counter the snapshot reports, so the two can neither gap nor overlap. */
	attach(
		shellId: string,
		writer: (bytes: Uint8Array) => void,
		sinceSeq = 0,
	): () => void {
		this.writers.set(shellId, writer);
		const buffered = this.pending.get(shellId);
		if (buffered) {
			this.pending.delete(shellId);
			for (const chunk of buffered) {
				if (chunk.seq > sinceSeq) writer(chunk.bytes);
			}
		}
		return () => {
			if (this.writers.get(shellId) === writer) this.writers.delete(shellId);
		};
	}

	forget(shellId: string): void {
		this.writers.delete(shellId);
		this.pending.delete(shellId);
	}
}

// ── Input queue ────────────────────────────────────────────────────────────
/** Serializes PTY input per shell. Each keystroke would otherwise be its own
 * in-flight request, and concurrent requests reach the PTY in completion
 * order, not typing order — which transposes characters. Queued bytes also
 * coalesce into one write, so a burst (or a paste) costs a single round-trip. */
class InputQueue {
	private queues = new Map<string, { pending: string; flushing: boolean }>();

	constructor(
		private readonly send: (shellId: string, data: string) => Promise<unknown>,
		private readonly onError: (error: unknown) => void,
	) {}

	write(shellId: string, data: string): void {
		const queue = this.queues.get(shellId) ?? { pending: "", flushing: false };
		if (queue.pending.length + data.length > MAX_PENDING_INPUT_CHARS) {
			this.onError(
				new Error(
					`Pending terminal input exceeds ${MAX_PENDING_INPUT_CHARS} characters; raise MAX_PENDING_INPUT_CHARS in tabs/terminal.tsx`,
				),
			);
			return;
		}
		queue.pending += data;
		this.queues.set(shellId, queue);
		if (!queue.flushing) void this.flush(shellId, queue);
	}

	private async flush(
		shellId: string,
		queue: { pending: string; flushing: boolean },
	): Promise<void> {
		queue.flushing = true;
		try {
			while (queue.pending) {
				const chunk = queue.pending;
				queue.pending = "";
				await this.send(shellId, chunk);
			}
		} catch (error) {
			// Drop the rest: replaying input after a failed write would deliver it
			// out of order, which is what this queue exists to prevent.
			queue.pending = "";
			this.onError(error);
		} finally {
			queue.flushing = false;
		}
	}

	forget(shellId: string): void {
		this.queues.delete(shellId);
	}
}

// ── Terminal pane ──────────────────────────────────────────────────────────
function terminalTheme(): Record<string, string> {
	const dark = document.documentElement.classList.contains("dark");
	return dark
		? { background: "#0b0e14", foreground: "#c7d0e0", cursor: "#c7d0e0" }
		: { background: "#ffffff", foreground: "#1f2328", cursor: "#1f2328" };
}

// The kernel answers a cursor-position query (ESC[6n) itself with a synthetic
// report, because a converged PTY may have no emulator on the master side
// (crates/kernel/src/pty.rs). With a real emulator attached, Ghostty answers
// too — and the second report lands in the guest's input stream as garbage.
// Drop ours; the kernel's already went through.
const CURSOR_POSITION_REPORT = /\x1b\[\d+;\d+R/g;

function TerminalPane({
	shellId,
	active,
	router,
	onInput,
	onResize,
	onError,
}: {
	shellId: string;
	active: boolean;
	router: OutputRouter;
	onInput: (data: string) => void;
	onResize: (cols: number, rows: number) => void;
	onError: (error: unknown) => void;
}) {
	const containerRef = useRef<HTMLDivElement | null>(null);
	const termRef = useRef<Terminal | null>(null);
	const fitRef = useRef<FitAddon | null>(null);

	// Handlers change identity every render; the terminal is built once per
	// shell, so read them through refs instead of rebuilding it.
	const onInputRef = useRef(onInput);
	const onResizeRef = useRef(onResize);
	const onErrorRef = useRef(onError);
	onInputRef.current = onInput;
	onResizeRef.current = onResize;
	onErrorRef.current = onError;

	useEffect(() => {
		const container = containerRef.current;
		if (!container) return;

		const term = new Terminal({
			cursorBlink: true,
			fontFamily:
				'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
			fontSize: 13,
			scrollback: SCROLLBACK_LINES,
			theme: terminalTheme(),
		});
		const fit = new FitAddon();
		term.loadAddon(fit);
		term.open(container);
		termRef.current = term;
		fitRef.current = fit;
		fit.fit();
		fit.observeResize();

		term.onData((data) => {
			const filtered = data.replace(CURSOR_POSITION_REPORT, "");
			if (filtered) onInputRef.current(filtered);
		});
		term.onResize(({ cols, rows }) => onResizeRef.current(cols, rows));

		// Repaint from the server before taking live output. This pane is built
		// from scratch on every tab switch — the dashboard reboots the iframe —
		// and a full-screen program (vim, less) never repaints unprompted, so
		// without a snapshot a reattached shell shows a blank screen. Output that
		// arrives while the snapshot is in flight stays buffered in the router
		// and is replayed after it, minus whatever the snapshot already covered.
		let detach: (() => void) | null = null;
		let disposed = false;
		const writer = (bytes: Uint8Array) => term.write(bytes);
		void agentOsSource.shellSnapshot(shellId, "screen").then(
			(snapshot) => {
				if (disposed) return;
				if (snapshot?.data) term.write(snapshot.data);
				detach = router.attach(shellId, writer, snapshot?.seq ?? 0);
			},
			(error) => {
				if (disposed) return;
				// Attach anyway: live output is still worth showing, but the failed
				// repaint means the pane starts blank, so say so.
				detach = router.attach(shellId, writer);
				onErrorRef.current(error);
			},
		);

		return () => {
			disposed = true;
			detach?.();
			fit.dispose();
			term.dispose();
			termRef.current = null;
			fitRef.current = null;
		};
	}, [shellId, router]);

	// Hidden panes have no layout box, so their fit is a no-op while inactive;
	// re-fit and take focus when this one comes forward.
	useEffect(() => {
		if (!active) return;
		fitRef.current?.fit();
		termRef.current?.focus();
	}, [active]);

	return (
		<div
			ref={containerRef}
			className="agentos-terminal-pane absolute inset-0"
			style={{ visibility: active ? "visible" : "hidden" }}
		/>
	);
}

// ── Tab ────────────────────────────────────────────────────────────────────
interface ShellTab {
	shellId: string;
	title: string;
}

export function TerminalTabConnected({ actorId }: { actorId: string }) {
	// Opening a shell boots the VM and then pins it awake for as long as the
	// shell lives, so never do it just because someone clicked the tab.
	return (
		<VmBootGate
			actorId={actorId}
			note="VM not booted."
			actionLabel="Boot the VM and open a terminal"
		>
			<TerminalTab actorId={actorId} />
		</VmBootGate>
	);
}

function TerminalTab({ actorId }: { actorId: string }) {
	const ghostty = useGhostty();
	const routerRef = useRef(new OutputRouter());
	const router = routerRef.current;

	const queryClient = useQueryClient();
	// The actor owns the shell list; the tab never remembers ids. A shell
	// outlives the page that opened it and holds the VM awake, so it has to
	// stay discoverable from a fresh page.
	const shellsQuery = useQuery(shellsQueryOptions(actorId));
	const [error, setError] = useState<unknown>(null);

	const shells = useMemo<ShellTab[]>(
		() =>
			(shellsQuery.data ?? []).map((shell, i) => ({
				shellId: shell.shellId,
				title: `shell ${i + 1}`,
			})),
		[shellsQuery.data],
	);

	const [selectedId, setSelectedId] = useState<string | null>(null);
	// Derived, not synced: a selection that no longer exists falls back to the
	// newest shell during render rather than through an effect.
	const activeId =
		selectedId && shells.some((s) => s.shellId === selectedId)
			? selectedId
			: (shells[shells.length - 1]?.shellId ?? null);

	const refreshShells = useCallback(
		() =>
			queryClient.invalidateQueries({
				queryKey: shellsQueryOptions(actorId).queryKey,
			}),
		[actorId, queryClient],
	);

	const dropShell = useCallback(
		(shellId: string) => {
			router.forget(shellId);
			inputRef.current?.forget(shellId);
			queryClient.setQueryData(
				shellsQueryOptions(actorId).queryKey,
				(prev: ShellInfo[] | null | undefined) =>
					prev ? prev.filter((s) => s.shellId !== shellId) : prev,
			);
			void refreshShells();
		},
		[actorId, queryClient, refreshShells, router],
	);

	const actor = useAgentOsActor();
	const useAgentEvent = actor.useEvent as (
		name: string,
		handler: (payload: unknown) => void,
	) => void;
	// Only `shellData` renders. `shellStderr` carries the same bytes as a
	// diagnostic tap — subscribing to both double-prints stderr.
	useAgentEvent("shellData", (raw) => {
		const payload = raw as ShellDataPayload | undefined;
		if (!payload || typeof payload.shellId !== "string") return;
		const bytes = decodeActionBytes(payload.data);
		if (bytes.length > 0) router.push(payload.shellId, payload.seq ?? 0, bytes);
	});
	useAgentEvent("shellExit", (raw) => {
		const payload = raw as ShellExitPayload | undefined;
		if (!payload || typeof payload.shellId !== "string") return;
		dropShell(payload.shellId);
	});

	const openShell = useMutation({
		mutationFn: () => agentOsSource.openShell(DEFAULT_COLS, DEFAULT_ROWS),
		onSuccess: ({ shellId }) => {
			setError(null);
			// Show the pane immediately instead of waiting for the next poll;
			// output that lands before it mounts is held by the router.
			queryClient.setQueryData(
				shellsQueryOptions(actorId).queryKey,
				(prev: ShellInfo[] | null | undefined) =>
					prev ? [...prev, { shellId, openedAt: Date.now() }] : prev,
			);
			setSelectedId(shellId);
			void refreshShells();
		},
		onError: setError,
	});

	const closeShell = useMutation({
		mutationFn: (shellId: string) => agentOsSource.closeShell(shellId),
		onMutate: dropShell,
		onError: setError,
	});

	const inputRef = useRef<InputQueue | null>(null);
	inputRef.current ??= new InputQueue(agentOsSource.writeShell, setError);
	const input = inputRef.current;

	const write = useCallback(
		(shellId: string, data: string) => input.write(shellId, data),
		[input],
	);
	const resize = useCallback((shellId: string, cols: number, rows: number) => {
		agentOsSource.resizeShell(shellId, cols, rows).catch(setError);
	}, []);

	if (ghostty.error) {
		return (
			<AgentOsEmpty>
				<div className="flex max-w-sm flex-col items-center gap-2">
					<span>Terminal renderer failed to load.</span>
					<ActionErrorNote error={ghostty.error} />
				</div>
			</AgentOsEmpty>
		);
	}
	// `null` = this runtime predates the `listShells` action, so shells cannot
	// be enumerated and the tab has no safe way to track them.
	if (shellsQuery.data === null) return <UnsupportedAction action="listShells" />;

	const atLimit = shells.length >= MAX_SHELLS;
	return (
		<div className="flex h-full min-h-0 flex-col">
			<div className="flex items-center gap-1 border-b px-2 py-1.5">
				{shells.map((shell) => (
					<div
						key={shell.shellId}
						onClick={() => setSelectedId(shell.shellId)}
						className={cn(
							"flex cursor-pointer items-center gap-1.5 rounded px-2 py-1 text-xs transition-colors",
							shell.shellId === activeId
								? "bg-muted text-foreground"
								: "text-muted-foreground hover:bg-muted/50",
						)}
					>
						<span className="font-mono">{shell.title}</span>
						<button
							type="button"
							title="Close shell"
							onClick={(e) => {
								e.stopPropagation();
								closeShell.mutate(shell.shellId);
							}}
							className="text-muted-foreground/60 transition-colors hover:text-foreground"
						>
							×
						</button>
					</div>
				))}
				<IconButton
					title={atLimit ? `Limit of ${MAX_SHELLS} shells reached` : "New shell"}
					onClick={() => openShell.mutate()}
					disabled={!ghostty.ready || openShell.isPending || atLimit}
				>
					<PlusIcon />
				</IconButton>
				<div className="ml-auto">
					<VmStatusBadges actorId={actorId} />
				</div>
			</div>

			{error ? <ActionErrorNote error={error} /> : null}

			<div className="relative min-h-0 flex-1 bg-background">
				{/* Constructing a Terminal before `init()` resolves throws; a
				    reattached shell list can arrive first. Output that lands
				    meanwhile is held by the router and flushed on mount. */}
				{!ghostty.ready ? (
					<AgentOsEmpty>Loading terminal renderer…</AgentOsEmpty>
				) : shells.length === 0 ? (
					<AgentOsEmpty>No shells open — click + to start one.</AgentOsEmpty>
				) : (
					shells.map((shell) => (
						<TerminalPane
							key={shell.shellId}
							shellId={shell.shellId}
							active={shell.shellId === activeId}
							router={router}
							onInput={(data) => write(shell.shellId, data)}
							onResize={(cols, rows) => resize(shell.shellId, cols, rows)}
							onError={setError}
						/>
					))
				)}
			</div>
		</div>
	);
}
