export function stringifyJsonUtf8(value: unknown, context: string): string {
	try {
		const encoded = JSON.stringify(value);
		if (encoded === undefined) {
			throw new Error(`${context} must be JSON-serializable`);
		}
		return encoded;
	} catch (error) {
		throw new Error(
			`${context} must be JSON-serializable: ${
				error instanceof Error ? error.message : String(error)
			}`,
		);
	}
}

export function parseJsonUtf8(value: string, context: string): unknown {
	try {
		return JSON.parse(value);
	} catch (error) {
		throw new Error(
			`invalid ${context} JSON payload: ${
				error instanceof Error ? error.message : String(error)
			}`,
		);
	}
}
