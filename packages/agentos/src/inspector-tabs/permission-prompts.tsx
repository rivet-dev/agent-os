// Global permission-approval banner, mounted in main.tsx above every tab (each
// tab is its own iframe, so "global" means "in every iframe's chrome"). An
// agent blocked on a permission request otherwise looks frozen: the session
// sits in durable "waiting" until someone answers. Cards come from two sources
// merged on `sessionId:requestId`: a backfill fetch (durable "waiting" sessions
// via `listSessions`, so requests raised while no inspector iframe was open
// still surface) plus live `sessionEvent` entries of type `permission_request`.
// A `permission_response` entry drops the card another viewer already answered.
import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { ChevronRight } from "./common";
import { isInspectorActionError } from "./lib/actor-client";
import { useAgentOsActor } from "./lib/rivet";
import { agentOsSource, pendingPermissionsQueryOptions } from "./lib/source";
import type { PendingPermissionDisplay, SessionStreamEntry } from "./lib/types";
import React from "react";

// Bounded queue: drop the oldest card beyond this (an unbounded queue would
// just stack dead cards; resolutions arrive as `permission_response` entries).
// The same bound caps the per-request status map.
const MAX_CARDS = 32;

interface PermissionRequest {
	sessionId: string;
	requestId: string;
	/** The request's OWN ACP options — one button per entry (never a fixed set). */
	options: PendingPermissionDisplay["options"];
	toolCall: PendingPermissionDisplay["toolCall"];
}

/** Per-request status, kept apart from the request itself so the rendered list
 * can be derived from (backfill + live) instead of copied into state. */
type CardStatus =
	| { state: "pending" }
	| { state: "busy" }
	/** `PermissionTerminalReason` when the request resolved out from under us. */
	| { state: "resolved"; reason?: string }
	| { state: "failed"; error: string }
	/** Answered here, answered elsewhere, or dismissed: no card. */
	| { state: "hidden" };

const PENDING: CardStatus = { state: "pending" };

function cardKey(card: Pick<PermissionRequest, "sessionId" | "requestId">): string {
	return `${card.sessionId}:${card.requestId}`;
}

/** Insert into a bounded, insertion-ordered map; oldest key drops first. */
function withStatus(
	prev: ReadonlyMap<string, CardStatus>,
	key: string,
	status: CardStatus,
): Map<string, CardStatus> {
	const next = new Map(prev);
	next.delete(key);
	next.set(key, status);
	while (next.size > MAX_CARDS) {
		const oldest = next.keys().next().value;
		if (oldest === undefined) break;
		next.delete(oldest);
	}
	return next;
}

/** Detail payload for the collapsible block: the tool call's raw input,
 * hidden when absent or an empty object. */
function toolCallDetail(toolCall: PermissionRequest["toolCall"]): unknown {
	const raw = toolCall?.rawInput;
	if (raw == null) return undefined;
	if (typeof raw === "object" && !Array.isArray(raw) && Object.keys(raw).length === 0) {
		return undefined;
	}
	return raw;
}

export function PermissionPrompts({ actorId }: { actorId: string }) {
	// Requests seen live on this iframe's subscription.
	const [liveRequests, setLiveRequests] = useState<PermissionRequest[]>([]);
	const [statuses, setStatuses] = useState<ReadonlyMap<string, CardStatus>>(() => new Map());

	// Backfill of requests raised before this iframe subscribed. `data === null`
	// means the runtime predates the durable-sessions contract (contract-layer
	// error) — stay live-only silently.
	const backfill = useQuery(pendingPermissionsQueryOptions(actorId));
	const backfillRows = backfill.data;

	// Live cards win the dedupe: a `sessionEvent` that raced the fetch already
	// carries the same request (identity is sessionId:requestId). Backfilled
	// requests predate anything received live, so they stay first.
	const cards = useMemo(() => {
		const liveKeys = new Set(liveRequests.map(cardKey));
		const merged: PermissionRequest[] = [
			...(backfillRows ?? [])
				.filter((row) => !liveKeys.has(cardKey(row)))
				.map((row) => ({
					sessionId: row.sessionId,
					requestId: row.requestId,
					options: row.options,
					toolCall: row.toolCall,
				})),
			...liveRequests,
		];
		return merged
			.map((request) => ({ request, status: statuses.get(cardKey(request)) ?? PENDING }))
			.filter((card) => card.status.state !== "hidden")
			.slice(-MAX_CARDS);
	}, [backfillRows, liveRequests, statuses]);

	// Live stream: durable `sessionEvent` entries carry both new requests and
	// their resolutions — same cast pattern as transcript.tsx (the handler
	// treats payloads as unknown and narrows on `entry.type`).
	const actor = useAgentOsActor();
	const useAgentEvent = actor.useEvent as (
		name: string,
		handler: (payload: unknown) => void,
	) => void;
	useAgentEvent("sessionEvent", (payload) => {
		const entry = payload as SessionStreamEntry | undefined;
		if (!entry) return;
		if (entry.type === "permission_request") {
			if (!entry.sessionId || !entry.requestId) return;
			const next: PermissionRequest = {
				sessionId: entry.sessionId,
				requestId: entry.requestId,
				options: entry.options,
				toolCall: entry.toolCall,
			};
			const key = cardKey(next);
			setLiveRequests((prev) => [...prev.filter((c) => cardKey(c) !== key), next].slice(-MAX_CARDS));
			// A re-raised request is pending again, whatever its last outcome was.
			setStatuses((prev) => withStatus(prev, key, PENDING));
			return;
		}
		// Another viewer (or a headless client) answered: its card is stale here,
		// so drop it outright — an untouched card needs no "handled elsewhere"
		// residue. A busy card stays: its own in-flight respond reports the
		// outcome (`{status: "not_pending"}` → quiet resolved state).
		if (entry.type === "permission_response") {
			if (!entry.sessionId || !entry.requestId) return;
			const key = cardKey(entry);
			setStatuses((prev) =>
				prev.get(key)?.state === "busy" ? prev : withStatus(prev, key, { state: "hidden" }),
			);
		}
	});

	const respond = async (request: PermissionRequest, optionId: string) => {
		const key = cardKey(request);
		const patch = (status: CardStatus) => setStatuses((prev) => withStatus(prev, key, status));
		patch({ state: "busy" });
		try {
			const result = await agentOsSource.respondPermission(
				request.sessionId,
				request.requestId,
				optionId,
			);
			if (result.status === "accepted") {
				patch({ state: "hidden" });
				return;
			}
			// `not_pending`: another viewer answered first or the prompt ended —
			// quiet outcome, not an error card.
			patch({ state: "resolved", reason: result.reason });
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			const hint = isInspectorActionError(error) ? ` — ${error.hint}` : "";
			patch({ state: "failed", error: `${message}${hint}` });
		}
	};

	const dismiss = (request: PermissionRequest) =>
		setStatuses((prev) => withStatus(prev, cardKey(request), { state: "hidden" }));

	if (cards.length === 0) return null;

	return (
		<div className="shrink-0 border-b">
			{cards.map(({ request, status }) => {
				const detail = toolCallDetail(request.toolCall);
				return (
					<div
						key={cardKey(request)}
						className="flex flex-col gap-1.5 border-b border-amber-500/20 bg-amber-500/5 px-3 py-2 text-xs last:border-b-0"
					>
						<div className="flex items-start gap-2">
							<div className="min-w-0 flex-1">
								<div className="text-amber-500">
									Agent requests permission
									<span className="ml-2 font-mono text-[10px] text-muted-foreground/70">
										session …{request.sessionId.slice(-10)}
									</span>
								</div>
								<div className="mt-0.5 text-foreground/90">
									{request.toolCall?.title ?? request.requestId}
								</div>
								{detail !== undefined ? (
									<details className="group mt-1 text-muted-foreground/70">
										<summary className="flex cursor-pointer list-none items-center gap-1 [&::-webkit-details-marker]:hidden">
											<ChevronRight className="size-3 transition-transform group-open:rotate-90" />
											Details
										</summary>
										<pre className="mt-1 max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded bg-muted/50 p-2 font-mono text-[11px] leading-relaxed">
											{JSON.stringify(detail, null, 2)}
										</pre>
									</details>
								) : null}
							</div>
							{status.state === "pending" || status.state === "busy" ? (
								<div className="flex shrink-0 items-center gap-1.5">
									{request.options.map((option) => (
										<button
											key={option.optionId}
											type="button"
											disabled={status.state === "busy"}
											onClick={() => void respond(request, option.optionId)}
											className={
												option.kind === "reject_once" || option.kind === "reject_always"
													? "rounded-md border border-destructive/50 px-2.5 py-1 text-destructive transition-colors hover:bg-destructive/10 disabled:opacity-40"
													: "rounded-md bg-primary px-2.5 py-1 text-primary-foreground transition-opacity hover:opacity-90 disabled:opacity-40"
											}
										>
											{option.name}
										</button>
									))}
								</div>
							) : (
								<div className="flex shrink-0 items-center gap-2">
									<span className="text-muted-foreground">
										{status.state === "resolved"
											? `No longer pending (${(status.reason ?? "already resolved").replace(/_/g, " ")})`
											: status.state === "failed"
												? status.error
												: "Reply failed"}
									</span>
									<button
										type="button"
										onClick={() => dismiss(request)}
										className="rounded border px-2 py-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
									>
										Dismiss
									</button>
								</div>
							)}
						</div>
					</div>
				);
			})}
		</div>
	);
}
