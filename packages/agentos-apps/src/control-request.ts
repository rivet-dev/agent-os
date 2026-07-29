const CONTROL_TIMEOUT_MS = 15_000;
const MAX_CONTROL_ATTEMPTS = 4;
const BASE_RETRY_DELAY_MS = 50;
const MAX_RETRY_DELAY_MS = 1_000;

function retryableStatus(status: number): boolean {
	return status === 409 || status === 429 || status >= 500;
}

function retryDelay(response: Response | undefined, attempt: number): number {
	const retryAfter = response?.headers.get("retry-after");
	if (retryAfter) {
		const seconds = Number(retryAfter);
		if (Number.isFinite(seconds) && seconds >= 0) {
			return Math.min(seconds * 1_000, MAX_RETRY_DELAY_MS);
		}
		const at = Date.parse(retryAfter);
		if (Number.isFinite(at)) {
			return Math.min(Math.max(0, at - Date.now()), MAX_RETRY_DELAY_MS);
		}
	}
	return Math.min(
		BASE_RETRY_DELAY_MS * 2 ** Math.max(0, attempt - 1),
		MAX_RETRY_DELAY_MS,
	);
}

/** Bounded retry policy for idempotent Rivet control-plane requests. */
export async function controlFetch(
	input: string | URL,
	init: RequestInit = {},
): Promise<Response> {
	const deadline = Date.now() + CONTROL_TIMEOUT_MS;
	let lastError: unknown;

	for (let attempt = 1; attempt <= MAX_CONTROL_ATTEMPTS; attempt += 1) {
		let response: Response | undefined;
		try {
			response = await fetch(input, {
				...init,
				signal: AbortSignal.timeout(Math.max(1, deadline - Date.now())),
			});
			if (
				!retryableStatus(response.status) ||
				attempt === MAX_CONTROL_ATTEMPTS
			) {
				return response;
			}
		} catch (error) {
			lastError = error;
			if (attempt === MAX_CONTROL_ATTEMPTS || Date.now() >= deadline)
				throw error;
		}

		if (response?.body) await response.body.cancel();
		const delay = retryDelay(response, attempt);
		if (Date.now() + delay >= deadline) {
			if (response) return response;
			throw lastError;
		}
		await new Promise((resolve) => setTimeout(resolve, delay));
	}

	throw lastError;
}
