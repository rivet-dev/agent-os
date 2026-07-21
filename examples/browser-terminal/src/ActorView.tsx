import { useCallback, useEffect, useRef, useState } from "react";
import { ACTOR_NAME, useActor } from "./rivet";
import { loadShellIds, saveShellIds } from "./store";
import { TerminalPane } from "./TerminalPane";

interface Tab {
	shellId: string;
	title: string;
}

interface ShellDataPayload {
	shellId: string;
	data: unknown;
}
interface ShellExitPayload {
	shellId: string;
}

interface TerminalActions {
	open(options?: { cols?: number; rows?: number }): Promise<{ shellId: string }>;
	write(shellId: string, data: string): Promise<void>;
	resize(shellId: string, cols: number, rows: number): Promise<void>;
	close(shellId: string): Promise<void>;
}

function terminalActions(connection: unknown): TerminalActions {
	return (connection as { terminal: TerminalActions }).terminal;
}

function toBytes(data: unknown): Uint8Array {
	if (data instanceof Uint8Array) return data;
	if (data instanceof ArrayBuffer) return new Uint8Array(data);
	if (Array.isArray(data)) return new Uint8Array(data as number[]);
	if (typeof data === "string") {
		const bin = atob(data);
		const bytes = new Uint8Array(bin.length);
		for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
		return bytes;
	}
	return new Uint8Array();
}

export function ActorView({ actorId }: { actorId: string }) {
	const agent = useActor({ name: ACTOR_NAME, key: actorId });
	const conn = agent.connection;

	const [tabs, setTabs] = useState<Tab[]>([]);
	const [active, setActive] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const initedRef = useRef(false);

	const writers = useRef<Map<string, (bytes: Uint8Array) => void>>(new Map());
	const pending = useRef<Map<string, Uint8Array[]>>(new Map());

	const dispatchData = useCallback((shellId: string, bytes: Uint8Array) => {
		const writer = writers.current.get(shellId);
		if (writer) {
			writer(bytes);
			return;
		}
		const buf = pending.current.get(shellId) ?? [];
		buf.push(bytes);
		pending.current.set(shellId, buf);
	}, []);

	const dropTab = useCallback(
		(shellId: string) => {
			writers.current.delete(shellId);
			pending.current.delete(shellId);
			setTabs((prev) => {
				const next = prev.filter((t) => t.shellId !== shellId);
				saveShellIds(
					actorId,
					next.map((t) => t.shellId),
				);
				setActive((cur) =>
					cur === shellId ? (next[next.length - 1]?.shellId ?? null) : cur,
				);
				return next;
			});
		},
		[actorId],
	);

	const subscribe = useCallback(
		(shellId: string) => (onData: (bytes: Uint8Array) => void) => {
			writers.current.set(shellId, onData);
			const buf = pending.current.get(shellId);
			if (buf) {
				for (const b of buf) onData(b);
				pending.current.delete(shellId);
			}
			return () => {
				writers.current.delete(shellId);
			};
		},
		[],
	);

	useEffect(() => {
		if (!conn) return;
		const events = conn as unknown as {
			on(name: string, cb: (p: never) => void): () => void;
		};
		const offData = events.on("shellData", (p: ShellDataPayload) =>
			dispatchData(p.shellId, toBytes(p.data)),
		);
		const offExit = events.on("shellExit", (p: ShellExitPayload) =>
			dropTab(p.shellId),
		);
		return () => {
			offData();
			offExit();
		};
	}, [conn, dispatchData, dropTab]);

	useEffect(() => {
		if (!conn || initedRef.current) return;
		initedRef.current = true;
		const ids = loadShellIds(actorId);
		if (ids.length === 0) return;
		Promise.all(
			ids.map(async (shellId) => {
				try {
					await terminalActions(conn).resize(shellId, 80, 24);
					return shellId;
				} catch {
					return null;
				}
			}),
		)
			.then((probed) => {
				const live = probed.filter((id): id is string => id !== null);
				saveShellIds(actorId, live);
				if (live.length === 0) return;
				setTabs(live.map((id, i) => ({ shellId: id, title: `shell ${i + 1}` })));
				setActive(live[0]);
			})
			.catch((e: unknown) => setError(String(e)));
	}, [conn, actorId]);

	useEffect(() => {
		initedRef.current = false;
		writers.current.clear();
		pending.current.clear();
		setTabs([]);
		setActive(null);
		setError(null);
	}, [actorId]);

	const openTerminal = useCallback(async () => {
		if (!conn) return;
		setBusy(true);
		setError(null);
		try {
			const { shellId } = await terminalActions(conn).open({
				cols: 80,
				rows: 24,
			});
			setTabs((prev) => {
				const next = [
					...prev,
					{ shellId, title: `shell ${prev.length + 1}` },
				];
				saveShellIds(
					actorId,
					next.map((t) => t.shellId),
				);
				return next;
			});
			setActive(shellId);
		} catch (e) {
			setError(String(e));
		} finally {
			setBusy(false);
		}
	}, [conn, actorId]);

	const closeTab = useCallback(
		async (shellId: string) => {
			dropTab(shellId);
			try {
				if (conn) await terminalActions(conn).close(shellId);
			} catch (closeError) {
				console.warn("Failed to close terminal", closeError);
			}
		},
		[conn, dropTab],
	);

	return (
		<div className="actor-view">
			<div className="tabbar">
				{tabs.map((t) => (
					<div
						key={t.shellId}
						className={`tab ${t.shellId === active ? "tab-active" : ""}`}
						onClick={() => setActive(t.shellId)}
					>
						<span className="tab-title">{t.title}</span>
						<button
							type="button"
							className="tab-close"
							title="Close terminal"
							onClick={(e) => {
								e.stopPropagation();
								void closeTab(t.shellId);
							}}
						>
							×
						</button>
					</div>
				))}
				<button
					type="button"
					className="tab-new"
					disabled={!conn || busy}
					onClick={() => void openTerminal()}
					title="New terminal"
				>
					+
				</button>
				<span className="conn-status">
					{conn ? "connected" : "connecting…"}
				</span>
			</div>

			{error && <div className="error-banner">{error}</div>}

			<div className="terminals">
				{tabs.length === 0 && (
					<div className="empty-hint">
						{conn
							? "No terminals yet — click + to open one."
							: "Connecting to the VM…"}
					</div>
				)}
				{tabs.map((t) => (
					<TerminalPane
						key={t.shellId}
						shellId={t.shellId}
						active={t.shellId === active}
						onInput={(text) =>
							conn ? terminalActions(conn).write(t.shellId, text) : undefined
						}
						onResize={(cols, rows) =>
							conn
								? terminalActions(conn).resize(t.shellId, cols, rows)
								: undefined
						}
						subscribe={subscribe(t.shellId)}
					/>
				))}
			</div>
		</div>
	);
}
