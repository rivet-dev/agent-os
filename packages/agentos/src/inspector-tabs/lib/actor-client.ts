// Stateless action transport for inspector-tab iframes, over the SAME rivetkit
// client the React layer uses for `useActor` (set by <RivetProvider> via
// `setRivetClient`). Actions go through `handle.action`, which POSTs to
// `/gateway/<actorId>@<authToken>/action/<name>` with `x-rivet-encoding: json`.
//
// React Query's query functions live outside React, so they can't read the
// provider's client from context — hence the module-level shared handle. The
// provider always mounts before any tab (and its queries) render, so the client
// is set by the time `callAction` runs.
import { INSPECTOR_ACTOR_NAME } from "./registry";

// Untyped here: `callAction` dispatches by string name. The typed surface is the
// `useActor` connection (see lib/rivet.tsx), not this raw transport.
// biome-ignore lint/suspicious/noExplicitAny: untyped shared rivetkit client
type Any = any;

let client: Any;
let handle: Any;
let actorId: string | undefined;

/** Called once by <RivetProvider> with the shared rivetkit client + actor id. */
export function setRivetClient(nextClient: Any, nextActorId: string): void {
	client = nextClient;
	actorId = nextActorId;
	handle = undefined;
}

/** The tab id this iframe is rendering, parsed from its own gateway URL. */
export function tabIdFromUrl(): string | undefined {
	const parts = window.location.pathname.split("/").filter(Boolean);
	const idx = parts.indexOf("custom-tabs");
	return idx >= 0 ? parts[idx + 1] : undefined;
}

/** Which hop of the dashboard→gateway→actor→VM chain an action failure came
 * from. Drives per-layer rendering (tab-boundary.tsx / common.tsx) and the
 * retry policy (main.tsx): contract/auth failures never retry. */
export type ActionErrorLayer = "gateway" | "auth" | "contract" | "runtime" | "timeout";

export class InspectorActionError extends Error {
	readonly layer: ActionErrorLayer;
	readonly action: string;
	readonly hint: string;
	constructor(layer: ActionErrorLayer, action: string, message: string, hint: string) {
		super(message);
		this.name = "InspectorActionError";
		this.layer = layer;
		this.action = action;
		this.hint = hint;
	}
}

export function isInspectorActionError(error: unknown): error is InspectorActionError {
	return (
		error instanceof InspectorActionError ||
		(typeof error === "object" &&
			error !== null &&
			(error as { name?: string }).name === "InspectorActionError")
	);
}

/** Map a raw transport/actor error onto the failing layer. Order matters:
 * abort before contract before runtime (messages overlap on "error"). */
function classifyActionError(action: string, error: unknown): InspectorActionError {
	const message = error instanceof Error ? error.message : String(error);
	const rivet = (error ?? {}) as {
		group?: string;
		code?: string;
		statusCode?: number;
		name?: string;
	};
	if (rivet.name === "AbortError" || /\baborted\b/i.test(message)) {
		return new InspectorActionError(
			"timeout",
			action,
			`${action} timed out`,
			"The VM may still be booting or the call is hung — retry in a few seconds.",
		);
	}
	if (/was not found/i.test(message) && /action/i.test(message)) {
		return new InspectorActionError(
			"contract",
			action,
			`This runtime does not expose the \`${action}\` action.`,
			"The actor was built against a different agentOS runtime version; this panel is unavailable.",
		);
	}
	if (
		rivet.statusCode === 401 ||
		rivet.statusCode === 403 ||
		rivet.code === "unauthorized" ||
		/bearer token|x-rivet-token|unauthorized|forbidden/i.test(message)
	) {
		return new InspectorActionError(
			"auth",
			action,
			message,
			"The inspector's token was rejected — reload the dashboard to refresh credentials.",
		);
	}
	if (rivet.code === "internal_error" || /internal error/i.test(message)) {
		return new InspectorActionError(
			"runtime",
			action,
			`${action} failed inside the actor runtime.`,
			"The engine masks the underlying error. Common causes: the agent adapter timed out while booting (retry — a warm VM answers faster) or the sidecar exited. The server log has the raw error.",
		);
	}
	if (error instanceof TypeError || /fetch failed|failed to fetch|networkerror/i.test(message)) {
		return new InspectorActionError(
			"gateway",
			action,
			`Could not reach the actor gateway (${message}).`,
			"The engine/gateway is unreachable — check that the server is running.",
		);
	}
	return new InspectorActionError("runtime", action, message || `${action} failed`, "See server logs for the underlying error.");
}

function getHandle(): Any {
	if (!client || !actorId) {
		throw new Error(
			"not initialized: rivet client unset (missing <RivetProvider>)",
		);
	}
	if (!handle) handle = client.getForId(INSPECTOR_ACTOR_NAME, actorId);
	return handle;
}

/** Invoke an actor action over the gateway (`handle.action`, JSON encoding).
 *
 * `timeoutMs` aborts the request (via AbortController) on expiry — some VM
 * actions (e.g. `stat` on /proc, /sys) hang server-side, and an un-aborted hung
 * request keeps a browser connection slot open; enough of those exhaust the
 * ~6-connection pool and starve every other request.
 */
export async function callAction<T = unknown>(
	name: string,
	args: unknown[] = [],
	opts: { timeoutMs?: number } = {},
): Promise<T> {
	const h = getHandle();
	const ctrl = new AbortController();
	const timer = opts.timeoutMs
		? setTimeout(() => ctrl.abort(), opts.timeoutMs)
		: undefined;
	try {
		return (await h.action({ name, args, signal: ctrl.signal })) as T;
	} catch (error) {
		throw classifyActionError(name, error);
	} finally {
		if (timer) clearTimeout(timer);
	}
}
