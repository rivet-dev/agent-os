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

export interface ActionError {
	group?: string;
	code?: string;
	message?: string;
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
	} finally {
		if (timer) clearTimeout(timer);
	}
}
