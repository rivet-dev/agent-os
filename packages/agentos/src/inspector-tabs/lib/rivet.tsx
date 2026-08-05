// @rivetkit/react integration for the inspector. Builds ONE rivetkit client from
// the iframe's { actorId, authToken } and shares it two ways:
//   - a live connection to the agent-os actor, used for every broadcast stream
//     (`sessionEvent`, `shellData`, `processOutput`, `vmBooted`, …).
//   - `setRivetClient()` → the same client backs the stateless `callAction`
//     transport used by React Query (lib/actor-client.ts).
//
// The client is created once auth is known (it needs the token for the gateway
// URL segment), so this is a provider gated behind the init handshake.
//
// The inspector attaches to a PRE-PROVISIONED actor by id, so events come from
// `getForId(...).connect()`. `useActor` cannot serve this: its options are
// `{ name, key }` with no `id`, and an actor id is not a key — passing one
// resolves nothing and silently never opens a connection.
import { createClient } from "@rivetkit/react";
import React, {
	createContext,
	type ReactNode,
	useContext,
	useEffect,
	useMemo,
	useRef,
} from "react";
import { setRivetClient } from "./actor-client";
import { INSPECTOR_ACTOR_NAME, type InspectorRegistry } from "./registry";

/** Minimal shape used from rivetkit's `ActorConn`. */
interface AgentOsConn {
	on(name: string, handler: (payload: unknown) => void): unknown;
	dispose?: () => unknown;
}

interface RivetValue {
	conn: AgentOsConn | null;
	actorId: string;
}

const RivetContext = createContext<RivetValue | null>(null);

export function RivetProvider({
	actorId,
	authToken,
	children,
}: {
	actorId: string;
	authToken: string;
	children: ReactNode;
}) {
	const value = useMemo<RivetValue>(() => {
		const client = createClient<InspectorRegistry>({
			endpoint: window.location.origin,
			token: authToken,
			// Match the gateway's `x-rivet-encoding: json`. The client default is now
			// `bare` (binary), whose decoder yields BigInt for integers and blows up on
			// numeric coercion ("Cannot convert a BigInt value to a number").
			encoding: "json",
			disableMetadataLookup: true,
		});
		setRivetClient(client, actorId);
		let conn: AgentOsConn | null = null;
		try {
			conn = client
				.getForId(INSPECTOR_ACTOR_NAME, actorId)
				.connect() as unknown as AgentOsConn;
		} catch (error) {
			// A dead event stream must not blank the whole inspector: queries still
			// work, tabs just stop receiving live updates.
			console.error("agentos inspector: failed to open actor connection", error);
		}
		return { conn, actorId };
	}, [actorId, authToken]);

	useEffect(() => {
		return () => {
			try {
				value.conn?.dispose?.();
			} catch (error) {
				console.warn("agentos inspector: failed to dispose actor connection", error);
			}
		};
	}, [value]);

	return <RivetContext.Provider value={value}>{children}</RivetContext.Provider>;
}

/** Subscribe to one actor broadcast for the life of the calling component.
 * `handler` is read through a ref, so an inline closure does not resubscribe. */
function useAgentOsEvent(
	conn: AgentOsConn | null,
	name: string,
	handler: (payload: unknown) => void,
): void {
	const handlerRef = useRef(handler);
	handlerRef.current = handler;
	useEffect(() => {
		if (!conn) return;
		const unsubscribe = conn.on(name, (payload) => handlerRef.current(payload));
		return () => {
			if (typeof unsubscribe === "function") (unsubscribe as () => void)();
		};
	}, [conn, name]);
}

/** Live connection to the agent-os actor, resolved by id. */
export function useAgentOsActor() {
	const ctx = useContext(RivetContext);
	if (!ctx) throw new Error("useAgentOsActor must be used within <RivetProvider>");
	const { conn } = ctx;
	return {
		useEvent: (name: string, handler: (payload: unknown) => void) =>
			useAgentOsEvent(conn, name, handler),
	};
}
