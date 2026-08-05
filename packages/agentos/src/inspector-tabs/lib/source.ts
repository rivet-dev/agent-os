// Real data source for the inspector tabs. Each query calls a REAL agentOS
// action via the gateway (actor-client) and transforms the result into the
// display type the component expects. The actor actions are thin wrappers over
// the core `AgentOs` API, so core types are the wire types (see lib/types.ts).

import type {
	HistoryPage,
	PermissionResponseResult,
	PromptResult,
	SessionPage,
} from "@rivet-dev/agentos-core";
import { keepPreviousData, queryOptions } from "@tanstack/react-query";
import { callAction, isInspectorActionError } from "./actor-client";
import type {
	FileContent,
	FsEntry,
	MountInfo,
	PendingPermissionDisplay,
	ProcessInfo,
	ProcessTreeNode,
	ReaddirEntry,
	RuntimeHealth,
	SessionInfo,
	SessionStreamEntry,
	ShellInfo,
	ShellReplayMode,
	ShellSnapshot,
	SignedPreviewUrl,
	SoftwareBundle,
	SoftwareInfo,
	TranscriptEvent,
	VirtualStat,
} from "./types";
import { sessionIsLive } from "./types";

const k = (actorId: string, ...rest: string[]) => [
	"agent-os",
	actorId,
	...rest,
];

// ── Software ──────────────────────────────────────────────────────────
function softwareInfoToBundle(info: SoftwareInfo): SoftwareBundle {
	const pkg = info.packageName;
	const scopeIdx = pkg.lastIndexOf("@");
	let name: string;
	if (scopeIdx > 0) name = pkg.slice(scopeIdx).split("/").slice(0, 2).join("/");
	else name = pkg.split("/").filter(Boolean).pop() ?? pkg;
	// Classify off the raw package (which keeps its `@scope/`), not the derived
	// display `name` (which strips the scope for bare scoped packages).
	const source: SoftwareBundle["source"] =
		pkg.startsWith("@rivet-dev/") || pkg.startsWith("@agentos-software/")
			? "rivet-dev"
			: "user";
	return {
		name,
		slug: (pkg.split("/").filter(Boolean).pop() ?? pkg).toLowerCase(),
		version: "—",
		source,
		binaries: info.commands ?? [],
	};
}

// ── Filesystem helpers ────────────────────────────────────────────────
function joinPath(dir: string, name: string): string {
	return dir === "/" ? `/${name}` : `${dir}/${name}`;
}

export function decodeActionBytes(output: unknown): Uint8Array {
	// rivetkit's json decoder may already hand back a real Uint8Array.
	if (output instanceof Uint8Array) return output;
	// JSON encoding wraps Uint8Array as ["$Uint8Array", base64].
	if (
		Array.isArray(output) &&
		output[0] === "$Uint8Array" &&
		typeof output[1] === "string"
	) {
		const bin = atob(output[1]);
		const bytes = new Uint8Array(bin.length);
		for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
		return bytes;
	}
	if (Array.isArray(output)) return Uint8Array.from(output as number[]);
	if (typeof output === "string") {
		try {
			const bin = atob(output);
			const bytes = new Uint8Array(bin.length);
			for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
			return bytes;
		} catch {
			return new TextEncoder().encode(output);
		}
	}
	return new Uint8Array();
}

/** Preview limit for the file viewer; larger files load only on request. */
const MAX_PREVIEW_BYTES = 4 * 1024 * 1024;

function bytesToDisplay(bytes: Uint8Array): string | null {
	// Heuristic binary check: NUL byte in the first 8 KiB.
	const probe = bytes.subarray(0, 8192);
	if (probe.includes(0)) return null;
	return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

// ── Transcript mapper (defensive: unknown entries → "raw") ────────────
/** Ordering key for a stream entry: the durable `sequence`, or
 * `afterSequence + 0.5` for ephemeral streaming deltas so they sort after the
 * durable entry they follow. */
export function entrySeq(entry: SessionStreamEntry): number {
	return entry.durability === "durable"
		? entry.sequence
		: entry.afterSequence + 0.5;
}

// Map one flat public session-event entry (durable history row or live
// broadcast) to a display event. Shared by the `readHistory` backfill and the
// live `sessionEvent` stream — both carry the same `SessionStreamEntry` union.
export function mapSessionEvent(entry: SessionStreamEntry): TranscriptEvent {
	const seq = entrySeq(entry);
	const e = entry as SessionStreamEntry & Record<string, unknown>;
	switch (entry.type) {
		case "user_message_chunk":
		case "agent_message_chunk":
		case "agent_thought_chunk": {
			const content = e.content as { type?: string; text?: string } | undefined;
			const text =
				content?.type === "text" && typeof content.text === "string"
					? content.text
					: "";
			return {
				kind:
					entry.type === "user_message_chunk"
						? "user"
						: entry.type === "agent_message_chunk"
							? "assistant"
							: "thinking",
				seq,
				text,
			};
		}
		case "tool_call":
		case "tool_call_update": {
			// ACP tool content: text blocks become output; diff entries are
			// summarized by path (full diff rendering stays behind "raw").
			const outputParts: string[] = [];
			if (Array.isArray(e.content)) {
				for (const c of e.content as Record<string, unknown>[]) {
					if (!c || typeof c !== "object") continue;
					if (c.type === "content") {
						const inner = c.content as
							| { type?: string; text?: string }
							| undefined;
						if (inner?.type === "text" && typeof inner.text === "string") {
							outputParts.push(inner.text);
						}
					} else if (c.type === "diff" && typeof c.path === "string") {
						outputParts.push(`[edit] ${c.path}`);
					}
				}
			}
			const locations = Array.isArray(e.locations)
				? (e.locations as { path?: string }[])
						.map((l) => l?.path)
						.filter((p): p is string => typeof p === "string")
				: undefined;
			return {
				kind: "tool",
				seq,
				toolCallId: typeof e.toolCallId === "string" ? e.toolCallId : undefined,
				tool: (e.title as string) ?? (e.toolCallId as string) ?? "tool",
				status: e.status as string | undefined,
				input: e.rawInput,
				output: outputParts.length > 0 ? outputParts.join("\n") : undefined,
				locations: locations && locations.length > 0 ? locations : undefined,
			};
		}
		case "plan": {
			const entries = Array.isArray(e.entries)
				? (e.entries as Record<string, unknown>[]).map((p) => ({
						content:
							typeof p?.content === "string"
								? p.content
								: JSON.stringify(p?.content ?? ""),
						status: typeof p?.status === "string" ? p.status : undefined,
					}))
				: [];
			return { kind: "plan", seq, entries };
		}
		case "current_mode_update":
			return {
				kind: "notice",
				seq,
				text: `Mode changed to ${String(e.currentModeId ?? "unknown")}`,
			};
		case "available_commands_update": {
			const count = Array.isArray(e.availableCommands)
				? e.availableCommands.length
				: 0;
			return {
				kind: "notice",
				seq,
				text: `${count} agent command${count === 1 ? "" : "s"} available`,
			};
		}
		case "permission_request": {
			const toolCall = e.toolCall as { title?: string } | undefined;
			return {
				kind: "permission",
				seq,
				text: `Permission requested${toolCall?.title ? `: ${toolCall.title}` : ""}`,
			};
		}
		case "permission_response":
			return {
				kind: "notice",
				seq,
				text:
					e.status === "accepted"
						? "Permission request answered"
						: `Permission request closed (${String(e.reason ?? "not pending")})`,
			};
		default:
			return { kind: "raw", seq, label: entry.type ?? "event", json: entry };
	}
}

/** Flatten `listSessions` into the pending-permission backfill: every request
 * a "waiting" session is blocked on. Durable, so requests raised while no
 * inspector was open still surface. */
export function pendingPermissionsOf(
	sessions: SessionInfo[],
): PendingPermissionDisplay[] {
	return sessions.flatMap((session) =>
		session.state.status === "waiting"
			? session.state.requests.map((request) => ({
					...request,
					sessionId: session.sessionId,
				}))
			: [],
	);
}

// ── Query options ─────────────────────────────────────────────────────
export const agentOsSource = {
	softwareQueryOptions: (actorId: string) =>
		queryOptions({
			queryKey: k(actorId, "software"),
			queryFn: async () =>
				(await callAction<SoftwareInfo[]>("listSoftware", [])).map(
					softwareInfoToBundle,
				),
		}),

	processesQueryOptions: (actorId: string) =>
		queryOptions({
			queryKey: k(actorId, "processes"),
			queryFn: () => callAction<ProcessInfo[]>("listProcesses", []),
			// Keep the table current while the tab is open; processExit broadcasts
			// also invalidate it immediately.
			refetchInterval: 5_000,
		}),

	// Full kernel process forest (every process, not just SDK-spawned).
	processTreeQueryOptions: (actorId: string) =>
		queryOptions({
			queryKey: k(actorId, "process-tree"),
			queryFn: () =>
				callAction<ProcessTreeNode[]>("processTree", [], { timeoutMs: 10_000 }),
			refetchInterval: 5_000,
		}),

	// Lazy per-directory listing via ONE `readdirEntries` call: the sidecar
	// returns every child with its type in a single round-trip (no `readdir` +
	// per-entry `stat`, which wedged the actor on large/virtual dirs). Recursive
	// from root still times out, so the tree fetches one level at a time on expand.
	listDirQueryOptions: (actorId: string, path: string, enabled = true) =>
		queryOptions({
			queryKey: k(actorId, "dir", path),
			enabled,
			// `readdirEntries` returns `null` when `path` is not a listable
			// directory (does not exist / is a file); surface that as `null` so
			// callers can show "not found", distinct from `[]` (empty dir).
			queryFn: async (): Promise<FsEntry[] | null> => {
				const raw = await callAction<ReaddirEntry[] | null>(
					"readdirEntries",
					[path],
					{
						timeoutMs: 10_000,
					},
				);
				if (raw === null) return null;
				const entries = raw
					.filter((e) => e.name !== "." && e.name !== "..")
					.map((e): FsEntry => {
						const p = joinPath(path, e.name);
						// Symlinks are reported lstat-style (not followed) → shown as a
						// leaf, like the old per-entry path did. Virtual fs (/proc, …) is
						// flagged so the tree never auto-expands it.
						return {
							name: e.name,
							path: p,
							dir: e.isDirectory,
							symlink: e.isSymbolicLink,
						};
					});
				return entries.sort(
					(a, b) =>
						Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name),
				);
			},
		}),

	fileContentQueryOptions: (
		actorId: string,
		path: string | null,
		force = false,
	) =>
		queryOptions({
			queryKey: k(actorId, "file", path ?? "", force ? "force" : "guarded"),
			enabled: !!path,
			// Selecting another file keeps the previous one on screen until the
			// new content lands, instead of flashing the whole viewer.
			placeholderData: keepPreviousData,
			queryFn: async (): Promise<FileContent> => {
				const p = path as string;
				// Stat first: reading a huge file drags megabytes through the
				// gateway just to preview it. Past the limit, skip the read until
				// the viewer's explicit "Load anyway".
				const stat = await callAction<VirtualStat>("stat", [p]);
				// Device/fifo/socket nodes (/dev/stdout, …) are streams: reading
				// them fails or hangs, so never try. Regular files and symlinks
				// (followed by readFile) proceed.
				const fileType = (stat.mode ?? 0) & 0o170000;
				if (
					fileType === 0o020000 || // character device
					fileType === 0o060000 || // block device
					fileType === 0o010000 || // fifo
					fileType === 0o140000 // socket
				) {
					return {
						path: p,
						sizeBytes: stat.size,
						mtimeMs: stat.mtimeMs,
						text: null,
						bytes: null,
						oversize: false,
						special: true,
					};
				}
				if (!force && stat.size > MAX_PREVIEW_BYTES) {
					return {
						path: p,
						sizeBytes: stat.size,
						mtimeMs: stat.mtimeMs,
						text: null,
						bytes: null,
						oversize: true,
					};
				}
				const bytes = decodeActionBytes(
					await callAction("readFile", [p], { timeoutMs: 30_000 }),
				);
				return {
					path: p,
					sizeBytes: stat.size,
					mtimeMs: stat.mtimeMs,
					text: bytesToDisplay(bytes),
					bytes,
					oversize: false,
				};
			},
		}),

	mountsQueryOptions: (actorId: string) =>
		queryOptions({
			queryKey: k(actorId, "mounts"),
			queryFn: () => callAction<MountInfo[]>("listMounts", []),
		}),

	// Durable session records with liveness (`state.status`) built in — one
	// action covers what previously took a persisted list + a live list.
	sessionsQueryOptions: (actorId: string) =>
		queryOptions({
			queryKey: k(actorId, "sessions"),
			queryFn: async () =>
				(await callAction<SessionPage>("listSessions", [])).sessions,
			// Poll so newly created sessions and running/waiting status dots stay
			// current.
			refetchInterval: 10_000,
		}),

	// One-off persisted backfill via durable history. Live events after this
	// snapshot arrive on the `sessionEvent` broadcast (same flat entry union),
	// so this does not poll.
	transcriptQueryOptions: (actorId: string, sessionId: string | null) =>
		queryOptions({
			queryKey: k(actorId, "transcript", sessionId ?? ""),
			enabled: !!sessionId,
			queryFn: async () =>
				(
					await callAction<HistoryPage>("readHistory", [{ sessionId }])
				).events.map(mapSessionEvent),
		}),

	// ── Composer actions (transcript tab) ─────────────────────────────────
	// Imperative (not queries): the composer drives the agent. Streamed output
	// arrives on the existing `sessionEvent` subscription; `prompt` resolves
	// when the turn completes.
	sendPrompt: (sessionId: string, text: string) =>
		callAction<PromptResult>("prompt", [
			{ sessionId, content: [{ type: "text", text }] },
		]),
	// `openSession` resolves with no value; the caller supplies the id (or one
	// is generated here) and uses it afterward.
	createSession: async (
		agent: string,
		options: { env?: Record<string, string> },
	): Promise<string> => {
		const sessionId = crypto.randomUUID();
		await callAction("openSession", [{ sessionId, agent, env: options.env }]);
		return sessionId;
	},

	// ── Permission approvals (global banner, permission-prompts.tsx) ─────
	// Answers a pending permission request with one of ITS OWN ACP options
	// (render the request's `options`; don't assume a fixed once/always/reject
	// set). Another viewer may answer first or the prompt may end — that comes
	// back as `{status: "not_pending", reason}`, not an error.
	respondPermission: (sessionId: string, requestId: string, optionId: string) =>
		callAction<PermissionResponseResult>("respondPermission", [
			{ sessionId, requestId, optionId },
		]),

	// ── Process control (processes tab) ───────────────────────────────────
	killProcess: (pid: number) => callAction("killProcess", [pid]),
	stopProcess: (pid: number) => callAction("stopProcess", [pid]),

	// ── Terminal (terminal tab) ────────────────────────────────────────────
	// PTY output arrives on the `shellData` broadcast, not from these calls.
	openShell: (cols: number, rows: number) =>
		callAction<{ shellId: string }>("openShell", [{ cols, rows }], {
			timeoutMs: 30_000,
		}),
	writeShell: (shellId: string, data: string) =>
		callAction("writeShell", [shellId, data]),
	resizeShell: (shellId: string, cols: number, rows: number) =>
		callAction("resizeShell", [shellId, cols, rows]),
	closeShell: (shellId: string) => callAction("closeShell", [shellId]),
	/** Server-rendered repaint for a shell that is already running. `null` when
	 * the runtime predates the action or the shell has no emulator, which the
	 * caller reads as "attach live, repaint nothing". */
	shellSnapshot: async (
		shellId: string,
		mode: ShellReplayMode = "screen",
	): Promise<ShellSnapshot | null> => {
		try {
			return await callAction<ShellSnapshot | null>(
				"shellSnapshot",
				[shellId, mode],
				{
					timeoutMs: 8_000,
				},
			);
		} catch (error) {
			if (isInspectorActionError(error) && error.layer === "contract")
				return null;
			throw error;
		}
	},

	// ── Filesystem mutations (filesystem tab) ──────────────────────────────
	writeFile: (path: string, content: Uint8Array | string) =>
		callAction("writeFile", [path, content], { timeoutMs: 30_000 }),
	mkdir: (path: string) => callAction("mkdir", [path]),
	moveEntry: (from: string, to: string) => callAction("move", [from, to]),
	deleteFile: (path: string, options: { recursive?: boolean }) =>
		callAction("remove", [path, options]),

	// ── Session management (transcript tab) ────────────────────────────────
	// Close ends the live agent process; the persisted transcript stays.
	closeSession: (sessionId: string) =>
		callAction("unloadSession", [{ sessionId }]),

	// ── Signed preview URLs (system tab) ───────────────────────────────────
	createSignedPreviewUrl: (port: number, ttlSeconds: number) =>
		callAction<SignedPreviewUrl>("createPreviewUrl", [port, ttlSeconds]),
	expireSignedPreviewUrl: (token: string) =>
		callAction("expirePreviewUrl", [token]),
};

// ── Runtime health (observe-only actions) ─────────────────────────────
// `health` reads VM liveness and post-mortem buffers without booting the VM.
// Feature detection stays: vendored tab bundles can run against an OLDER
// published runtime without the action, which rejects at the contract layer
// (see lib/health.ts's isMissingHealthAction) — the status badges hide
// themselves and the composer disables its Stop button.

/** Shells currently open on the actor. Observe-only: never boots the VM, and a
 * sleeping VM correctly reports none. This is the source of truth for the
 * terminal tab's shell strip — the tab must not remember ids itself, or a
 * shell that outlives its page becomes unreachable while still holding the VM
 * awake. Feature-detected like `health` so vendored bundles running against an
 * older runtime degrade instead of erroring. */
export const shellsQueryOptions = (actorId: string) =>
	queryOptions({
		queryKey: k(actorId, "shells"),
		queryFn: async (): Promise<ShellInfo[] | null> => {
			try {
				return await callAction<ShellInfo[]>("listShells", [], {
					timeoutMs: 8_000,
				});
			} catch (error) {
				if (isInspectorActionError(error) && error.layer === "contract")
					return null;
				throw error;
			}
		},
		refetchInterval: 10_000,
		retry: false,
	});

export const healthQueryOptions = (actorId: string) =>
	queryOptions({
		queryKey: k(actorId, "runtime-health"),
		queryFn: () =>
			callAction<RuntimeHealth>("health", [], { timeoutMs: 8_000 }),
		refetchInterval: 5_000,
		// Contract-missing is permanent for this actor: never retry, and stop
		// polling (react-query keeps refetching errored queries otherwise).
		retry: false,
		refetchOnMount: false,
	});

/** Pending permission requests — the one-off backfill for the permission
 * banner (permission-prompts.tsx), derived from the durable session records,
 * so requests raised while no inspector iframe was open still render. No
 * refetchInterval: after the mount fetch, updates arrive as `sessionEvent`
 * entries (`permission_request` / `permission_response`). */
export const pendingPermissionsQueryOptions = (actorId: string) =>
	queryOptions({
		queryKey: k(actorId, "pending-permissions"),
		queryFn: async (): Promise<PendingPermissionDisplay[] | null> => {
			try {
				const page = await callAction<SessionPage>("listSessions", []);
				return pendingPermissionsOf(page.sessions);
			} catch (error) {
				if (isInspectorActionError(error) && error.layer === "contract")
					return null;
				throw error;
			}
		},
		retry: false,
	});

/** Session ids currently live (loaded in the VM), derived from the same
 * durable records the sidebar renders. */
export function liveSessionIds(sessions: SessionInfo[]): Set<string> {
	return new Set(sessions.filter(sessionIsLive).map((s) => s.sessionId));
}

/** Best-effort prompt cancellation; resolves false when unsupported. */
export async function cancelPrompt(sessionId: string): Promise<boolean> {
	try {
		await callAction("cancelPrompt", [{ sessionId }]);
		return true;
	} catch (error) {
		if (isInspectorActionError(error) && error.layer === "contract")
			return false;
		throw error;
	}
}
