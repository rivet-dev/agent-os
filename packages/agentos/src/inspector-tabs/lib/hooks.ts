// Shared React hooks for the inspector tabs: imperative browser resources and
// timers, each owning its own cleanup.
import { useEffect, useMemo, useRef, useState } from "react";

/** Blob URL for in-memory bytes, revoked when they change or on unmount. */
export function useObjectUrl(bytes: Uint8Array | null | undefined): string | null {
	const url = useMemo(
		() => (bytes ? URL.createObjectURL(new Blob([bytes as BlobPart])) : null),
		[bytes],
	);
	useEffect(() => {
		return () => {
			if (url) URL.revokeObjectURL(url);
		};
	}, [url]);
	return url;
}

/** Call `onSettled` once `value` has stopped changing for `delayMs` — a pending
 * timer is cancelled by the next change and by unmount. `onSettled` need not be
 * stable: the latest one is used. */
export function useSettledValue<T>(
	value: T,
	delayMs: number,
	onSettled: (value: T) => void,
): void {
	const latest = useRef(onSettled);
	useEffect(() => {
		latest.current = onSettled;
	});
	useEffect(() => {
		const id = setTimeout(() => latest.current(value), delayMs);
		return () => clearTimeout(id);
	}, [value, delayMs]);
}

/** Two-step confirm for irreversible actions: the first press arms an action,
 * a second press within `timeoutMs` runs it. Disarms on timeout, on unmount,
 * and whenever `resetKey` changes — an armed delete must never carry over to a
 * different target. */
export function useArmedConfirm<K extends string>({
	timeoutMs = 3_000,
	resetKey,
}: { timeoutMs?: number; resetKey?: unknown } = {}): {
	armed: K | null;
	confirm: (action: K, run: () => void) => void;
} {
	const [stored, setStored] = useState<{ armed: K | null; key: unknown }>({
		armed: null,
		key: resetKey,
	});
	const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
	if (stored.key !== resetKey) setStored({ armed: null, key: resetKey });
	const armed = stored.key === resetKey ? stored.armed : null;
	useEffect(() => () => clearTimeout(timer.current), []);

	const confirm = (action: K, run: () => void) => {
		clearTimeout(timer.current);
		if (armed === action) {
			setStored({ armed: null, key: resetKey });
			run();
			return;
		}
		setStored({ armed: action, key: resetKey });
		timer.current = setTimeout(() => setStored({ armed: null, key: resetKey }), timeoutMs);
	};

	return { armed, confirm };
}
