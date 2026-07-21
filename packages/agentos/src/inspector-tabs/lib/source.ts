// Real data source for the inspector tabs — the deliberate replacement for the
// mockup's `createLiveAgentOsSource`. Each query calls a REAL agentOS action via
// the gateway (actor-client) and transforms the result into the display type the
// ported component expects. Action names/shapes are the actual ones, not the
// mockup's aspirational `agentOs*` names.

import type { SessionStreamEntry } from "@rivet-dev/agentos-core";
import { queryOptions } from "@tanstack/react-query";
import { callAction } from "./actor-client";
import type {
	FileContent,
	FsEntry,
	MountInfo,
	ProcessInfo,
	ReaddirEntry,
	SessionInfo,
	SoftwareBundle,
	SoftwareInfo,
	TranscriptEvent,
	VirtualStat,
} from "./types";

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
		version: "—",
		source,
		binaries: info.commands ?? [],
	};
}

// ── Filesystem helpers ────────────────────────────────────────────────
function joinPath(dir: string, name: string): string {
	return dir === "/" ? `/${name}` : `${dir}/${name}`;
}

function decodeActionBytes(output: unknown): Uint8Array {
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

function bytesToDisplay(bytes: Uint8Array): string | null {
	// Heuristic binary check: NUL byte in the first 8 KiB.
	const probe = bytes.subarray(0, 8192);
	if (probe.includes(0)) return null;
	return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

// ── Transcript mapper (defensive: unknown ACP updates → "raw") ─────────
// Map the flat public session-event union to a display event.
export function mapSessionEvent(event: SessionStreamEntry): TranscriptEvent {
	const text =
		event.type === "user_message_chunk" ||
		event.type === "agent_message_chunk" ||
		event.type === "agent_thought_chunk"
			? event.content.type === "text"
				? event.content.text
				: ""
			: "";
	switch (event.type) {
		case "user_message_chunk":
			return { kind: "user", text };
		case "agent_message_chunk":
			return { kind: "assistant", text };
		case "agent_thought_chunk":
			return { kind: "thinking", text };
		case "tool_call":
		case "tool_call_update":
			return {
				kind: "tool",
				tool: event.title ?? event.toolCallId ?? "tool",
				status: event.status ?? undefined,
			};
		default:
			return { kind: "raw", label: event.type, json: event };
	}
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
						};
					});
				return entries.sort(
					(a, b) =>
						Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name),
				);
			},
		}),

	fileContentQueryOptions: (actorId: string, path: string | null) =>
		queryOptions({
			queryKey: k(actorId, "file", path ?? ""),
			enabled: !!path,
			queryFn: async (): Promise<FileContent> => {
				const p = path as string;
				const [bytes, stat] = await Promise.all([
					callAction("readFile", [p]).then(decodeActionBytes),
					callAction<VirtualStat>("stat", [p]),
				]);
				return {
					path: p,
					sizeBytes: stat.size,
					mtimeMs: stat.mtimeMs,
					text: bytesToDisplay(bytes),
				};
			},
		}),

	mountsQueryOptions: (actorId: string) =>
		queryOptions({
			queryKey: k(actorId, "mounts"),
			queryFn: () => callAction<MountInfo[]>("listMounts", []),
		}),

	sessionsQueryOptions: (actorId: string) =>
		queryOptions({
			queryKey: k(actorId, "sessions"),
			queryFn: () => callAction<SessionInfo[]>("listSessions", []),
			// Poll so newly created and closed sessions stay current.
			refetchInterval: 10_000,
		}),
};
