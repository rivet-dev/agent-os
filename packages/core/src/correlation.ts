interface PendingResponse<TResponse> {
	resolve: (frame: TResponse) => void;
	reject: (error: Error) => void;
}

export class PendingResponseRegistry<TResponse> {
	private readonly pending = new Map<number, PendingResponse<TResponse>>();

	// Deliberately no per-request timer: local framed stdio never loses frames,
	// so a response is bounded by the transport's silence watchdog (a dead or
	// wedged sidecar rejects all pending requests through `rejectAll`) rather
	// than by guessing how long any one request should take.
	waitForResponse(requestId: number): Promise<TResponse> {
		if (this.pending.has(requestId)) {
			throw new Error(
				`response waiter already registered for request ${requestId}`,
			);
		}
		return new Promise<TResponse>((resolve, reject) => {
			this.pending.set(requestId, {
				resolve: (frame: TResponse) => {
					this.pending.delete(requestId);
					resolve(frame);
				},
				reject: (error: Error) => {
					this.pending.delete(requestId);
					reject(error);
				},
			});
		});
	}

	resolve(requestId: number, frame: TResponse): boolean {
		const pending = this.pending.get(requestId);
		if (!pending) {
			return false;
		}
		pending.resolve(frame);
		return true;
	}

	reject(requestId: number, error: Error): boolean {
		const pending = this.pending.get(requestId);
		if (!pending) {
			return false;
		}
		pending.reject(error);
		return true;
	}

	rejectAll(error: Error): void {
		for (const pending of this.pending.values()) {
			pending.reject(error);
		}
		this.pending.clear();
	}
}
