
import { QueryErrorResetBoundary } from "@tanstack/react-query";
import { Component, type ReactNode, Suspense } from "react";
import { ActionErrorNote, UnsupportedAction } from "./common";
import { isInspectorActionError } from "./lib/actor-client";
import React from "react";

function TabFallback() {
	return (
		<div className="flex h-full flex-1 items-center justify-center p-8 text-sm text-muted-foreground">
			<span className="inline-flex items-center gap-2">
				<svg className="size-4 animate-spin text-muted-foreground" viewBox="0 0 24 24" fill="none" aria-hidden="true">
					<circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" />
					<path className="opacity-75" fill="currentColor" d="M12 2a10 10 0 0 1 10 10h-3a7 7 0 0 0-7-7z" />
				</svg>
				Loading…
			</span>
		</div>
	);
}

class ErrorBoundary extends Component<
	{ children: ReactNode; onReset?: () => void },
	{ error?: Error }
> {
	state: { error?: Error } = {};
	static getDerivedStateFromError(error: Error) {
		return { error };
	}
	#retry = () => {
		this.props.onReset?.();
		this.setState({ error: undefined });
	};
	render() {
		const { error } = this.state;
		if (!error) return this.props.children;
		// Contract-layer failures are a capability gap, not a fault: render the
		// quiet unsupported state with no retry (retrying cannot succeed).
		if (isInspectorActionError(error) && error.layer === "contract") {
			return <UnsupportedAction action={error.action} />;
		}
		return (
			<div className="flex h-full flex-1 flex-col items-center justify-center p-8">
				<div className="flex w-full max-w-md flex-col items-center gap-3 rounded-xl border bg-secondary p-6 text-center">
					<ActionErrorNote error={error} className="items-center p-0 text-center" />
					<button
						type="button"
						onClick={this.#retry}
						className="rounded border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
					>
						Retry
					</button>
				</div>
			</div>
		);
	}
}

export function TabBoundary({ children }: { children: ReactNode }) {
	// QueryErrorResetBoundary clears React Query's cached suspense errors so the
	// Retry button actually refetches instead of re-throwing the stale error.
	return (
		<QueryErrorResetBoundary>
			{({ reset }) => (
				<ErrorBoundary onReset={reset}>
					<Suspense fallback={<TabFallback />}>{children}</Suspense>
				</ErrorBoundary>
			)}
		</QueryErrorResetBoundary>
	);
}
