export class ForegroundPriorityGate {
	#activeForeground = 0;
	#pendingForeground = 0;
	#activeBackground = 0;
	#waiters = new Set<() => void>();

	#notify(): void {
		for (const resolve of this.#waiters) resolve();
		this.#waiters.clear();
	}

	async #waitUntil(ready: () => boolean): Promise<void> {
		while (!ready()) {
			await new Promise<void>((resolve) => this.#waiters.add(resolve));
		}
	}

	async foreground<T>(run: () => Promise<T>): Promise<T> {
		this.#pendingForeground += 1;
		this.#notify();
		let acquired = false;
		try {
			await this.#waitUntil(() => this.#activeBackground === 0);
			this.#pendingForeground -= 1;
			this.#activeForeground += 1;
			acquired = true;
			this.#notify();
			return await run();
		} finally {
			if (acquired) this.#activeForeground -= 1;
			else this.#pendingForeground -= 1;
			this.#notify();
		}
	}

	async background<T>(run: () => Promise<T>): Promise<T> {
		while (true) {
			await this.#waitUntil(
				() => this.#activeForeground === 0 && this.#pendingForeground === 0,
			);
			// Give already-arriving HTTP work one event-loop turn to register as
			// foreground, then acquire synchronously with the final state check.
			await new Promise<void>((resolve) => setImmediate(resolve));
			if (this.#activeForeground === 0 && this.#pendingForeground === 0) break;
		}
		this.#activeBackground += 1;
		try {
			return await run();
		} finally {
			this.#activeBackground -= 1;
			this.#notify();
		}
	}
}
