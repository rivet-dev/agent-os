// Kernel process table for the System tab: one dense table sized to content,
// with detail on demand — clicking a row expands it inline (fields, stop/kill,
// live output tail) instead of a permanently open side pane.
import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { ActionErrorNote, ChevronRight, relativeTime, StatusDot } from "../common";
import { cn } from "../lib/cn";
import { useArmedConfirm } from "../lib/hooks";
import { useAgentOsActor } from "../lib/rivet";
import { agentOsSource, decodeActionBytes } from "../lib/source";
import type { KernelProcessInfo, ProcessExitPayload, ProcessOutputPayload, ProcessTreeNode } from "../lib/types";
import React from "react";

/** Format an epoch-ms spawn time for display; `—` when absent. */
function formatStartedAt(startedAt: number | undefined): string {
	if (!startedAt) return "—";
	return new Date(startedAt).toLocaleTimeString();
}

/** Tree → rows with depth for indentation. Running processes sort before
 * exited at every level; ties break newest-first so fresh activity is on top. */
interface ProcessRow extends KernelProcessInfo {
	depth: number;
}

function flattenTree(nodes: ProcessTreeNode[], depth = 0, out: ProcessRow[] = []): ProcessRow[] {
	const sorted = [...nodes].sort(
		(a, b) =>
			Number(b.status === "running") - Number(a.status === "running") ||
			b.startTime - a.startTime,
	);
	for (const node of sorted) {
		const { children, ...info } = node;
		out.push({ ...info, depth });
		flattenTree(children, depth + 1, out);
	}
	return out;
}

// ── Live output tail ───────────────────────────────────────────────────────
// Bounded per-pid buffers fed by the `processOutput` broadcast. Only pids
// spawned through the SDK have output pumps; everything else shows nothing.
const MAX_TRACKED_PIDS = 32;
const MAX_BUFFER_CHARS = 64_000;

class OutputBuffers {
	private buffers = new Map<number, string>();

	append(pid: number, text: string): void {
		const prev = this.buffers.get(pid);
		if (prev === undefined && this.buffers.size >= MAX_TRACKED_PIDS) {
			// Bounded: drop the oldest-tracked pid's buffer.
			const oldest = this.buffers.keys().next().value;
			if (oldest !== undefined) this.buffers.delete(oldest);
		}
		const next = (prev ?? "") + text;
		this.buffers.set(pid, next.length > MAX_BUFFER_CHARS ? next.slice(-MAX_BUFFER_CHARS) : next);
	}

	get(pid: number): string | undefined {
		return this.buffers.get(pid);
	}
}

/** Label-over-value cell for the expanded detail grid — compact, no divider
 * rows, the layout VM dashboards use for inspect summaries. */
function Field({ label, value }: { label: string; value: string }) {
	return (
		<div className="min-w-0">
			<div className="text-muted-foreground/70">{label}</div>
			<div className="truncate font-mono" title={value}>
				{value}
			</div>
		</div>
	);
}

function ExpandedDetail({
	p,
	outputTail,
	onStop,
	onKill,
	actionError,
}: {
	p: ProcessRow;
	outputTail?: string;
	onStop: () => void;
	onKill: () => void;
	actionError: unknown;
}) {
	// Arming is per-process: expanding another row must not inherit it.
	const { armed, confirm } = useArmedConfirm<"stop" | "kill">({ resetKey: p.pid });
	return (
		<div className="flex flex-col gap-3 px-4 py-3 text-xs">
			<div className="grid grid-cols-2 gap-x-8 gap-y-2 sm:grid-cols-4">
				<Field label="ppid" value={p.ppid ? String(p.ppid) : "—"} />
				<Field label="cwd" value={p.cwd || "—"} />
				<Field label="driver" value={p.driver || "—"} />
				<Field label="exit code" value={p.exitCode == null ? "—" : String(p.exitCode)} />
				<Field label="args" value={p.args.join(" ") || "—"} />
				<Field label="started" value={formatStartedAt(p.startTime)} />
				<Field label="exited" value={p.exitTime == null ? "—" : relativeTime(p.exitTime)} />
				<Field label="group / session" value={`${p.pgid} / ${p.sid}`} />
			</div>
			{p.status === "running" ? (
				<div className="flex items-center gap-2">
					<button
						type="button"
						onClick={() => confirm("stop", onStop)}
						className="rounded border px-2 py-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
					>
						{armed === "stop" ? "Confirm stop?" : "Stop"}
					</button>
					<button
						type="button"
						onClick={() => confirm("kill", onKill)}
						className="rounded border border-destructive/40 px-2 py-0.5 text-destructive transition-colors hover:bg-destructive/10"
					>
						{armed === "kill" ? "Confirm kill?" : "Kill"}
					</button>
				</div>
			) : null}
			{actionError ? <ActionErrorNote error={actionError} className="p-0" /> : null}
			{outputTail ? (
				<div>
					<div className="mb-1 text-muted-foreground/70">Output (live tail)</div>
					<pre className="max-h-48 overflow-y-auto whitespace-pre-wrap break-words rounded bg-muted/50 p-2 font-mono text-[11px] leading-relaxed">
						{outputTail}
					</pre>
				</div>
			) : (
				<div className="text-muted-foreground/60">
					No live output. Only processes spawned through the SDK stream stdout/stderr here.
				</div>
			)}
		</div>
	);
}

/** Running/total counts for the System overview; same query key as the table
 * so React Query serves both from one fetch. */
export function useProcessCounts(actorId: string): { running: number; total: number } {
	const { data: tree } = useSuspenseQuery(agentOsSource.processTreeQueryOptions(actorId));
	const rows = flattenTree(tree);
	return { running: rows.filter((p) => p.status === "running").length, total: rows.length };
}

export function ProcessTable({ actorId }: { actorId: string }) {
	const { data: tree } = useSuspenseQuery(agentOsSource.processTreeQueryOptions(actorId));
	const queryClient = useQueryClient();
	const [expandedPid, setExpandedPid] = useState<number | null>(null);
	const [actionError, setActionError] = useState<unknown>(null);

	const rows = flattenTree(tree);

	// Live output tail for SDK-spawned pids; exit refreshes the table.
	const buffersRef = useRef(new OutputBuffers());
	const [, setOutputVersion] = useState(0);
	const actor = useAgentOsActor();
	const useAgentEvent = actor.useEvent as (
		name: string,
		handler: (payload: unknown) => void,
	) => void;
	useAgentEvent("processOutput", (raw) => {
		const payload = raw as ProcessOutputPayload | undefined;
		if (!payload || typeof payload.pid !== "number") return;
		const text = new TextDecoder("utf-8", { fatal: false }).decode(
			decodeActionBytes(payload.data),
		);
		if (!text) return;
		buffersRef.current.append(payload.pid, text);
		setOutputVersion((v) => v + 1);
	});
	useAgentEvent("processExit", (raw) => {
		const payload = raw as ProcessExitPayload | undefined;
		if (!payload || typeof payload.pid !== "number") return;
		buffersRef.current.append(payload.pid, `\n[exited ${payload.exitCode}]`);
		setOutputVersion((v) => v + 1);
		void queryClient.invalidateQueries({
			queryKey: agentOsSource.processTreeQueryOptions(actorId).queryKey,
		});
	});

	const invalidate = () =>
		queryClient.invalidateQueries({
			queryKey: agentOsSource.processTreeQueryOptions(actorId).queryKey,
		});
	const runControl = async (action: () => Promise<unknown>) => {
		setActionError(null);
		try {
			await action();
			await invalidate();
		} catch (error) {
			setActionError(error);
		}
	};

	if (rows.length === 0) {
		return <div className="px-4 py-3 text-sm text-muted-foreground">No processes in the VM.</div>;
	}
	return (
		<div className="max-h-96 overflow-y-auto">
			<table className="w-full text-sm">
				<thead className="sticky top-0 bg-secondary text-[11px] text-muted-foreground">
					<tr className="border-b">
						<th className="w-8 px-3 py-2" aria-label="Expand" />
						<th className="w-16 px-2 py-2 text-left font-medium">PID</th>
						<th className="px-2 py-2 text-left font-medium">Command</th>
						<th className="w-28 px-2 py-2 text-left font-medium">Started</th>
						<th className="w-28 px-2 py-2 text-left font-medium">Status</th>
					</tr>
				</thead>
				<tbody>
					{rows.map((p) => (
						<React.Fragment key={p.pid}>
							<tr
								onClick={() => setExpandedPid((cur) => (cur === p.pid ? null : p.pid))}
								className={cn(
									"cursor-pointer border-b border-foreground/[0.06] hover:bg-muted/50",
									expandedPid === p.pid && "bg-muted/40",
									p.status === "exited" && "opacity-50",
								)}
							>
								<td className="px-3 py-1.5">
									<ChevronRight
										className={cn(
											"size-3 text-muted-foreground/60 transition-transform",
											expandedPid === p.pid && "rotate-90",
										)}
									/>
								</td>
								<td className="px-2 py-1.5 font-mono">{p.pid}</td>
								<td className="px-2 py-1.5 font-mono text-xs">
									<span style={{ paddingLeft: `${p.depth * 14}px` }}>
										{p.depth > 0 ? <span className="text-muted-foreground/40">└ </span> : null}
										{p.command}
										{p.args.length > 0 ? (
											<span className="text-muted-foreground/60"> {p.args.join(" ")}</span>
										) : null}
									</span>
								</td>
								<td className="px-2 py-1.5 font-mono text-xs text-muted-foreground">
									{formatStartedAt(p.startTime)}
								</td>
								<td className="px-2 py-1.5">
									<span className="inline-flex items-center gap-1.5 text-xs">
										<StatusDot color={p.status === "running" ? "green" : "muted"} />
										{p.status}
									</span>
								</td>
							</tr>
							{expandedPid === p.pid ? (
								<tr className="border-b border-foreground/[0.06] bg-muted/20">
									<td colSpan={5}>
										<ExpandedDetail
											p={p}
											outputTail={buffersRef.current.get(p.pid)}
											onStop={() => void runControl(() => agentOsSource.stopProcess(p.pid))}
											onKill={() => void runControl(() => agentOsSource.killProcess(p.pid))}
											actionError={actionError}
										/>
									</td>
								</tr>
							) : null}
						</React.Fragment>
					))}
				</tbody>
			</table>
		</div>
	);
}
