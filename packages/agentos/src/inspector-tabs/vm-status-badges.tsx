// Compact VM status for each tab's own top bar — the successor to the
// full-width status strip row. Renders nothing while the VM is simply
// healthy; otherwise a small state chip (not booted / shut down / sidecar
// trouble) and an amber warnings pill whose dropdown holds limit warnings,
// agent exits, and the adapter stderr tail. Data: the observe-only
// `health` action poll (never wakes a sleeping VM) with instant flips from
// the `vmBooted`/`vmShutdown` broadcasts; actors without the action render
// nothing, preserving stock behavior.
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { CopyButton, relativeTime, StatusDot } from "./common";
import { cn } from "./lib/cn";
import { isMissingHealthAction } from "./lib/health";
import { useAgentOsActor } from "./lib/rivet";
import { healthQueryOptions } from "./lib/source";
import type { RuntimeHealth, VmShutdownPayload } from "./lib/types";
import { ChevronRight } from "./common";
import React from "react";

type LiveVmState =
	| { kind: "booted" }
	| { kind: "shutdown"; reason?: string }
	| null;

// `null` = nothing worth a chip: the VM is healthy or still being checked.
function stateChip(
	health: RuntimeHealth | undefined,
	live: LiveVmState,
): { tone: "muted" | "red"; label: string } | null {
	if (live?.kind === "shutdown") {
		const reason = live.reason ?? "unknown";
		return reason === "error"
			? { tone: "red", label: "VM shut down after an error" }
			: { tone: "muted", label: `VM shut down (${reason})` };
	}
	if (!health) return null;
	if (health.sidecar && health.sidecar.state !== "ready") {
		return { tone: "red", label: `sidecar ${health.sidecar.state}` };
	}
	if (!health.booted && live?.kind !== "booted") {
		return { tone: "muted", label: "VM not booted" };
	}
	return null;
}

export function VmStatusBadges({
	actorId,
	align = "right",
}: {
	actorId: string;
	/** Which edge the warnings dropdown anchors to. */
	align?: "left" | "right";
}) {
	const health = useQuery(healthQueryOptions(actorId));
	const [live, setLive] = useState<LiveVmState>(null);
	const [panelOpen, setPanelOpen] = useState(false);

	// Live lifecycle flips between polls. Broadcast names aren't in the typed
	// event schema — same cast pattern as transcript.tsx.
	const actor = useAgentOsActor();
	const useAgentEvent = actor.useEvent as (
		name: string,
		handler: (payload: unknown) => void,
	) => void;
	useAgentEvent("vmBooted", () => {
		setLive({ kind: "booted" });
		void health.refetch();
	});
	useAgentEvent("vmShutdown", (payload) => {
		setLive({ kind: "shutdown", reason: (payload as VmShutdownPayload | undefined)?.reason });
		void health.refetch();
	});

	// Actor doesn't provide the health action → nothing to show.
	if (health.error && isMissingHealthAction(health.error)) return null;

	const data = health.data;
	const chip = health.error
		? ({ tone: "muted", label: "health unavailable" } as const)
		: stateChip(data, live);
	const warningCount = (data?.warnings.length ?? 0) + (data?.agentExits.length ?? 0);
	if (!chip && warningCount === 0) return null;

	return (
		<span className="inline-flex shrink-0 items-center gap-1.5 text-xs">
			{chip ? (
				// Pill, matching the warnings button beside it — bare text next to a
				// bordered control reads as a rendering fault, not a status.
				<span
					className={cn(
						"inline-flex items-center gap-1.5 rounded border px-2 py-0.5 text-[11px]",
						chip.tone === "red"
							? "border-destructive/40 text-destructive"
							: "border-foreground/15 text-muted-foreground",
					)}
				>
					<StatusDot color={chip.tone === "red" ? "red" : "muted"} />
					{chip.label}
				</span>
			) : null}
			{warningCount > 0 ? (
				<span className="relative">
					<button
						type="button"
						onClick={() => setPanelOpen((v) => !v)}
						className="inline-flex items-center gap-1 rounded border border-amber-500/40 px-2 py-0.5 text-[11px] text-amber-500 transition-colors hover:bg-amber-500/10"
					>
						{warningCount} warning{warningCount === 1 ? "" : "s"}
						<ChevronRight
							className={`size-2.5 transition-transform ${panelOpen ? "rotate-90" : ""}`}
						/>
					</button>
					{panelOpen && data ? (
						<div
							className={`absolute top-full z-20 mt-1 max-h-64 w-80 overflow-y-auto rounded-md border bg-background p-2 text-[11px] leading-relaxed shadow-lg ${
								align === "right" ? "right-0" : "left-0"
							}`}
						>
							{data.warnings.length > 0 ? (
								<div className="mb-2">
									<div className="mb-1 flex items-center gap-1 font-medium text-muted-foreground">
										Limit warnings
										<CopyButton
											value={data.warnings
												.map(
													(w) =>
														`${new Date(w.ts).toISOString()} ${w.limit} ${w.observed}/${w.capacity} · ${w.fillPercent}%`,
												)
												.join("\n")}
										/>
									</div>
									{data.warnings.map((w, i) => (
										<div key={`w-${i}`} className="flex items-center gap-2 font-mono">
											<span>
												{w.limit} {w.observed}/{w.capacity} · {w.fillPercent}%
											</span>
											<span className="ml-auto text-muted-foreground">{relativeTime(w.ts)}</span>
										</div>
									))}
								</div>
							) : null}
							{data.agentExits.length > 0 ? (
								<div className="mb-2">
									<div className="mb-1 flex items-center gap-1 font-medium text-muted-foreground">
										Agent exits
										<CopyButton
											value={data.agentExits
												.map(
													(e) =>
														`${new Date(e.ts).toISOString()} ${e.agentType} session=${e.sessionId} exit=${e.exitCode ?? "?"} restart=${e.restart} (${e.restartCount})`,
												)
												.join("\n")}
										/>
									</div>
									{data.agentExits.map((e, i) => (
										<div key={`e-${i}`} className="flex items-center gap-2 font-mono">
											<span>
												{e.agentType} exit {e.exitCode ?? "?"} · {e.restart} ({e.restartCount})
											</span>
											<span className="ml-auto text-muted-foreground">{relativeTime(e.ts)}</span>
										</div>
									))}
								</div>
							) : null}
							{data.stderrTail.length > 0 ? (
								<div>
									<div className="mb-1 flex items-center gap-1 font-medium text-muted-foreground">
										Adapter stderr (tail)
										<CopyButton value={data.stderrTail.map((l) => l.line).join("\n")} />
									</div>
									<pre className="whitespace-pre-wrap break-words font-mono text-muted-foreground">
										{data.stderrTail.map((l) => l.line).join("\n")}
									</pre>
								</div>
							) : null}
						</div>
					) : null}
				</span>
			) : null}
		</span>
	);
}
