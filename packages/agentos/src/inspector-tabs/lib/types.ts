// Display + raw types for the inspector tabs. Raw types match the agentOS
// action return shapes; the source adapter (source.ts) transforms raw → display.
// Session/permission shapes come straight from the core public API — the actor
// actions are thin wrappers over `AgentOs`, so core types ARE the wire types.
import type {
	AgentExitEvent,
	PendingPermissionRequest,
	SessionInfo,
} from "@rivet-dev/agentos-core";

export type {
	AgentExitEvent,
	DurableSessionEventEntry,
	PendingPermissionRequest,
	SessionInfo,
	SessionStreamEntry,
} from "@rivet-dev/agentos-core";
export type {
	RuntimeAgentExit,
	RuntimeHealth,
	RuntimeLimitWarning,
} from "../../types";

// ── Software ──────────────────────────────────────────────────────────
export interface SoftwareBundle {
	name: string;
	/** Package basename ("coreutils", "claude-code") — keys the logo lookup. */
	slug: string;
	version: string;
	source: "rivet-dev" | "user";
	binaries: string[]; // command names the package ships (from SoftwareInfo.commands)
}
export interface SoftwareInfo {
	packageName: string;
	/** Command names projected from this package. */
	commands: string[];
}

// ── Processes ─────────────────────────────────────────────────────────
/** Actual `listProcesses` serialized shape (a subset of the kernel struct;
 * ppid/cwd/cpu/mem/signal/stdout are NOT exposed — PARTIAL). Only processes
 * started via the SDK `spawn` are listed; `startedAt` is the spawn time. */
export interface ProcessInfo {
	pid: number;
	command: string;
	args: string[];
	running: boolean;
	exitCode: number | null;
	/** Epoch milliseconds when the process was spawned. */
	startedAt: number;
}
/** Raw `allProcesses`/`processTree` node fields — the full kernel process
 * table (every process, not just SDK-spawned). Mirrors the Rust wire shape;
 * `startTime`/`exitTime` are epoch milliseconds. */
export interface KernelProcessInfo {
	pid: number;
	ppid: number;
	pgid: number;
	sid: number;
	driver: string;
	command: string;
	args: string[];
	cwd: string;
	status: "running" | "exited";
	exitCode: number | null;
	startTime: number;
	exitTime: number | null;
}
/** Raw `processTree` node: kernel info + children. */
export interface ProcessTreeNode extends KernelProcessInfo {
	children: ProcessTreeNode[];
}
/** Live `processOutput` broadcast payload mirror. `data` arrives
 * Uint8Array-shaped but encoding-dependent — normalize with
 * `decodeActionBytes`. Only SDK-`spawn`ed pids have output pumps. */
export interface ProcessOutputPayload {
	pid: number;
	stream: "stdout" | "stderr";
	data: unknown;
}
/** Live `processExit` broadcast payload mirror. */
export interface ProcessExitPayload {
	pid: number;
	exitCode: number;
}
/** Live `shellData` broadcast payload mirror — ordered PTY output for one
 * shell. `data` is encoding-dependent; normalize with `decodeActionBytes`.
 * `shellStderr` carries the same shape but is a diagnostic tap: rendering it
 * alongside `shellData` double-prints every stderr byte. */
export interface ShellDataPayload {
	shellId: string;
	data: unknown;
	/** Orders this chunk against `ShellSnapshot.seq`; 0 when the runtime has no
	 * replay for the shell. */
	seq?: number;
}
/** Live `shellExit` broadcast payload mirror. */
export interface ShellExitPayload {
	shellId: string;
	exitCode: number;
}

/** Raw `createPreviewUrl` result. `path` is relative to the gateway
 * origin serving this iframe. */
export interface SignedPreviewUrl {
	path: string;
	token: string;
	port: number;
	expiresAt: number;
}

// ── Filesystem ────────────────────────────────────────────────────────
/** Raw `readdirRecursive` entry. */
export interface DirEntry {
	path: string;
	type: "file" | "directory" | "symlink";
	size: number;
}
/** One directory entry, fetched lazily per-level (readdir + stat). Recursive
 * `readdirRecursive("/")` times out on a real VM fs, so the tree loads on
 * demand: each expanded dir fetches just its own children. */
export interface FsEntry {
	name: string;
	path: string;
	dir: boolean;
	size?: number;
	/** Reported lstat-style (not followed); rendered with a link marker. */
	symlink?: boolean;
	/** Virtual/system fs (/proc, /sys, …) — shown but not stat-ed or expanded,
	 * because touching it wedges the VM sidecar. */
	virtual?: boolean;
}
/** Raw `readdirEntries` entry — one typed child in a single round-trip. No
 * `size` (the fast path skips the per-entry `stat`); the file viewer stats on open. */
export interface ReaddirEntry {
	name: string;
	isDirectory: boolean;
	isSymbolicLink: boolean;
}
/** Raw `stat` shape (subset we use). */
export interface VirtualStat {
	/** POSIX mode bits (file type in the top nibble). */
	mode: number;
	size: number;
	mtimeMs: number;
	isDirectory: boolean;
	isSymbolicLink: boolean;
}
export interface FileContent {
	path: string;
	sizeBytes: number;
	mtimeMs: number;
	text: string | null; // null = binary or not loaded
	/** Raw bytes for download / image preview; null when skipped (oversize). */
	bytes: Uint8Array | null;
	/** True when the file exceeded the preview limit and was not read. */
	oversize: boolean;
	/** True for device/fifo/socket nodes — streams with no readable contents. */
	special?: boolean;
}

// ── Mounts ────────────────────────────────────────────────────────────
/** Raw `listMounts` entry — echoes the actor's declarative mount config.
 * The kernel has no runtime mount table to enumerate. */
export interface MountInfo {
	path: string;
	kind: string;
	readOnly: boolean;
	config?: unknown | null;
}

// ── Sessions / transcript ─────────────────────────────────────────────
/** Mapped, displayable transcript event (defensive; unknown → "raw"). Carries
 * the source `seq` for stable keys/ordering: the durable `sequence` for
 * persisted entries, or `afterSequence + 0.5` for ephemeral streaming deltas
 * (they sort after the durable entry they follow). Tool events keep
 * `toolCallId` so the render pipeline can merge a call and its status updates
 * into one card. */
export type TranscriptEvent = { seq: number } & (
	| { kind: "user" | "assistant" | "thinking"; text: string }
	| {
			kind: "tool";
			tool: string;
			toolCallId?: string;
			status?: string;
			input?: unknown;
			output?: string;
			locations?: string[];
	  }
	| { kind: "plan"; entries: { content: string; status?: string }[] }
	| { kind: "notice"; text: string }
	| { kind: "permission"; text: string }
	| { kind: "raw"; label: string; json: unknown }
	| { kind: "error"; text: string }
);

/** One pending permission request flattened for display: the session's
 * `state.requests` entry (core `PendingPermissionRequest`) plus the session it
 * belongs to. Backfilled from `listSessions` (sessions with
 * `state.status === "waiting"`); live updates arrive as `sessionEvent`
 * entries with `type === "permission_request"` / `"permission_response"`. */
export interface PendingPermissionDisplay extends PendingPermissionRequest {
	sessionId: string;
}

/** Crash-row payload: core's `AgentExitEvent`, broadcast as `agentExit`. */
export type AgentCrashedPayload = AgentExitEvent;

/** Session liveness derived from core `SessionInfo.state`. */
export function sessionIsLive(session: SessionInfo): boolean {
	return (
		session.state.status === "running" || session.state.status === "waiting"
	);
}

/** Live `vmShutdown` broadcast payload mirror. */
export interface VmShutdownPayload {
	reason?: "sleep" | "destroy" | "error" | string;
}

/** One live shell from the observe-only `listShells` action. */
export interface ShellInfo {
	shellId: string;
	openedAt: number;
}

/** How much history a `shellSnapshot` repaints. */
export type ShellReplayMode = "none" | "screen" | "scrollback";

/** Server-rendered repaint for a running shell; `data` is ANSI to write
 * verbatim into a fresh terminal. */
export interface ShellSnapshot {
	shellId: string;
	mode: ShellReplayMode;
	seq: number;
	cols: number;
	rows: number;
	data: string;
}
