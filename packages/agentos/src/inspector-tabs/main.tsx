import { dehydrate, hydrate, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { lazy, type ComponentType, StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { isInspectorActionError, tabIdFromUrl } from "./lib/actor-client";
import { installStreamBodyFetchCompat } from "./lib/fetch-compat";
import { RivetProvider } from "./lib/rivet";
import { PermissionPrompts } from "./permission-prompts";
import { TabBoundary } from "./tab-boundary";
import React from "react";

import "./styles.css";

// Must run before any rivetkit client is constructed.
installStreamBodyFetchCompat();

// Tab registry: id → lazy component. Add a tab here + register the same id in
// actor.ts `inspectorTabs` pointing `source` at this shared asset dir.
const TABS: Record<string, () => Promise<{ default: ComponentType<{ actorId: string }> }>> = {
	transcript: () =>
		import("./tabs/transcript").then((m) => ({ default: m.TranscriptTabConnected })),
	filesystem: () =>
		import("./tabs/filesystem").then((m) => ({ default: m.FilesystemTabConnected })),
	system: () =>
		import("./tabs/system").then((m) => ({ default: m.SystemTabConnected })),
	terminal: () =>
		import("./tabs/terminal").then((m) => ({ default: m.TerminalTabConnected })),
};

// Hosts vendor built copies of this bundle and pin their tab-id config at
// server start, so ids that existed in older configs must keep rendering.
// Software, Mounts, and Processes merged into System; route their ids there.
const LEGACY_TAB_ALIASES: Record<string, string> = {
	software: "system",
	mounts: "system",
	processes: "system",
};

// Theme comes from the dashboard via the iframe URL; the tokens and all
// `dark:` variants key off this class. Absent param = dark (today's default).
document.documentElement.classList.toggle(
	"dark",
	new URLSearchParams(window.location.search).get("theme") !== "light",
);

const queryClient = new QueryClient({
	defaultOptions: {
		queries: {
			// VM-backed actions (listProcesses/readdir/readFile/stat) time out on
			// the FIRST call after the actor's VM hibernates and cold-wakes;
			// retry so the query recovers once the VM is warm. Contract/auth
			// failures are permanent for this actor — retrying only delays the
			// error UI by the full backoff, so fail those immediately.
			retry: (failureCount, error) => {
				if (
					isInspectorActionError(error) &&
					(error.layer === "contract" || error.layer === "auth")
				) {
					return false;
				}
				return failureCount < 3;
			},
			retryDelay: (attempt) => Math.min(1500 * 2 ** attempt, 8000),
			refetchOnWindowFocus: false,
			staleTime: 5_000,
		},
	},
});

// The dashboard swaps this iframe out on every tab switch, so each switch is
// a full document boot. Carry the query cache across in sessionStorage: the
// next document renders every tab from cached data instantly (no loading
// states) and refetches in the background (5s staleTime). Excluded keys:
// `file` bodies hold Uint8Arrays (huge and not JSON-safe) and re-fetch
// quickly anyway.
const QUERY_CACHE_KEY = "agentos-inspector:query-cache";
const QUERY_CACHE_EXCLUDED_KINDS = new Set(["file"]);
const QUERY_CACHE_MAX_BYTES = 3 * 1024 * 1024;
try {
	const raw = sessionStorage.getItem(QUERY_CACHE_KEY);
	if (raw) hydrate(queryClient, JSON.parse(raw));
} catch (error) {
	console.warn("agentos inspector: failed to restore query cache", error);
}
window.addEventListener("pagehide", () => {
	try {
		const state = dehydrate(queryClient, {
			shouldDehydrateQuery: (query) =>
				query.state.status === "success" &&
				!QUERY_CACHE_EXCLUDED_KINDS.has(String(query.queryKey[2])),
		});
		const raw = JSON.stringify(state);
		// sessionStorage has a ~5MB origin budget; leave headroom.
		if (raw.length > QUERY_CACHE_MAX_BYTES) {
			console.warn(
				`agentos inspector: query cache too large to persist (${raw.length} chars)`,
			);
			return;
		}
		sessionStorage.setItem(QUERY_CACHE_KEY, raw);
	} catch (error) {
		console.warn("agentos inspector: failed to persist query cache", error);
	}
});

function App() {
	const [auth, setAuthState] = useState<{ actorId: string; authToken: string }>();

	useEffect(() => {
		// Tell the dashboard we're ready (clears its "Connecting…" overlay).
		parent.postMessage({ type: "ready", v: 1 }, "*");
		const onMessage = (event: MessageEvent) => {
			const d = event.data || {};
			if (d.type === "init" && d.actorId && d.authToken) {
				setAuthState({ actorId: d.actorId, authToken: d.authToken });
			}
		};
		window.addEventListener("message", onMessage);
		return () => window.removeEventListener("message", onMessage);
	}, []);

	const tabId = tabIdFromUrl();
	const resolvedTabId = tabId ? (LEGACY_TAB_ALIASES[tabId] ?? tabId) : undefined;
	const loader = resolvedTabId ? TABS[resolvedTabId] : undefined;

	if (!loader) {
		return <div style={{ padding: 16 }}>Unknown inspector tab: {String(tabId)}</div>;
	}
	if (!auth) {
		return <div style={{ padding: 16, color: "#8a8a90" }}>Connecting…</div>;
	}

	const Tab = lazy(loader);
	// RivetProvider owns the shared rivetkit client (typed `useActor` + the
	// `callAction` transport). One boundary catches both the code-split chunk
	// load and the tab's useSuspenseQuery data load. VM status renders as
	// compact badges inside each tab's own top bar (vm-status-badges.tsx),
	// not as a dedicated row here.
	return (
		<RivetProvider actorId={auth.actorId} authToken={auth.authToken}>
			<div className="flex h-full min-h-0 flex-col">
				{/* Permission prompts sit outside the boundary: an agent blocked on
				    approval must stay answerable from every tab even when the tab
				    body itself is broken. */}
				<PermissionPrompts actorId={auth.actorId} />
				<div className="min-h-0 flex-1">
					<TabBoundary>
						<Tab actorId={auth.actorId} />
					</TabBoundary>
				</div>
			</div>
		</RivetProvider>
	);
}

createRoot(document.getElementById("root")!).render(
	<StrictMode>
		<QueryClientProvider client={queryClient}>
			<App />
		</QueryClientProvider>
	</StrictMode>,
);
