/**
 * Controls how deeply and widely console.log arguments are serialized.
 * Prevents CPU amplification and memory buildup from deeply-nested or
 * massive objects being logged inside the sandbox.
 */
export interface ConsoleSerializationBudget {
	maxDepth: number;
	maxKeys: number;
	maxArrayLength: number;
	maxOutputLength: number;
}

export const DEFAULT_CONSOLE_SERIALIZATION_BUDGET: ConsoleSerializationBudget = {
	maxDepth: 6,
	maxKeys: 50,
	maxArrayLength: 50,
	maxOutputLength: 4096,
};

function normalizeBudget(
	budget: ConsoleSerializationBudget,
): ConsoleSerializationBudget {
	const defaults = {
		maxDepth: 6,
		maxKeys: 50,
		maxArrayLength: 50,
		maxOutputLength: 4096,
	};
	const clamp = (value: number, fallback: number) => {
		if (!Number.isFinite(value)) return fallback;
		const normalized = Math.floor(value);
		return normalized > 0 ? normalized : fallback;
	};

	return {
		maxDepth: clamp(budget.maxDepth, defaults.maxDepth),
		maxKeys: clamp(budget.maxKeys, defaults.maxKeys),
		maxArrayLength: clamp(budget.maxArrayLength, defaults.maxArrayLength),
		maxOutputLength: clamp(budget.maxOutputLength, defaults.maxOutputLength),
	};
}

function safeStringifyConsoleValueWithBudget(
	value: unknown,
	budget: ConsoleSerializationBudget,
): string {
	const suffix = "...[Truncated]";
	const clampOutput = (text: string) => {
		if (text.length <= budget.maxOutputLength) {
			return text;
		}
		if (budget.maxOutputLength <= suffix.length) {
			return suffix.slice(0, budget.maxOutputLength);
		}
		return (
			text.slice(0, budget.maxOutputLength - suffix.length) + suffix
		);
	};

	if (value === null) return "null";
	if (value === undefined) return "undefined";
	const valueType = typeof value;
	if (valueType !== "object") {
		if (valueType === "bigint") {
			return `${String(value)}n`;
		}
		return clampOutput(String(value));
	}

	const rootObject = value as Record<string, unknown>;
	const skipFastPath =
		(Array.isArray(rootObject) &&
			rootObject.length > budget.maxArrayLength) ||
		(!Array.isArray(rootObject) &&
			Object.keys(rootObject).length > budget.maxKeys);

	if (!skipFastPath) {
		try {
			const quickSerialized = JSON.stringify(value);
			if (quickSerialized !== undefined) {
				return clampOutput(quickSerialized);
			}
		} catch {
			// Fall back to circular-safe and budget-aware serialization.
		}
	}

	const seen = new WeakSet<object>();
	const depthByObject = new WeakMap<object, number>();
	const replacer = function (this: unknown, key: string, current: unknown) {
		if (typeof current === "bigint") {
			return `${String(current)}n`;
		}
		if (typeof current !== "object" || current === null) {
			return current;
		}

		const currentObject = current as Record<string, unknown>;
		if (seen.has(currentObject)) {
			return "[Circular]";
		}
		seen.add(currentObject);

		let depth = 0;
		if (key !== "") {
			const parent = this;
			if (typeof parent === "object" && parent !== null) {
				depth = (depthByObject.get(parent as object) ?? 0) + 1;
			}
		}
		depthByObject.set(currentObject, depth);

		if (depth > budget.maxDepth) {
			return "[MaxDepth]";
		}

		if (Array.isArray(currentObject)) {
			if (currentObject.length <= budget.maxArrayLength) {
				return currentObject;
			}
			const trimmed = currentObject.slice(0, budget.maxArrayLength);
			trimmed.push("[Truncated]");
			return trimmed;
		}

		const keys = Object.keys(currentObject);
		if (keys.length <= budget.maxKeys) {
			return currentObject;
		}

		const trimmed: Record<string, unknown> = {};
		for (let i = 0; i < budget.maxKeys; i += 1) {
			const keyName = keys[i];
			trimmed[keyName] = currentObject[keyName];
		}
		trimmed["[Truncated]"] = `${keys.length - budget.maxKeys} key(s)`;
		return trimmed;
	};

	try {
		const serialized = JSON.stringify(value, replacer);
		if (serialized === undefined) {
			return clampOutput(String(value));
		}
		return clampOutput(serialized);
	} catch {
		return clampOutput(String(value));
	}
}

/** Serialize a single value with circular reference detection and budget limits. */
export function safeStringifyConsoleValue(
	value: unknown,
	rawBudget: ConsoleSerializationBudget,
): string {
	return safeStringifyConsoleValueWithBudget(value, normalizeBudget(rawBudget));
}

/** Format an array of console arguments into a single space-separated string. */
export function formatConsoleArgs(
	args: unknown[],
	rawBudget: ConsoleSerializationBudget,
): string {
	const budget = normalizeBudget(rawBudget);
	const formatted: string[] = [];
	for (let i = 0; i < args.length; i += 1) {
		formatted.push(safeStringifyConsoleValueWithBudget(args[i], budget));
	}
	return formatted.join(" ");
}

/**
 * Generate isolate-side JavaScript that installs a `globalThis.console` shim.
 * The shim serializes arguments using the budget and forwards them to host
 * bridge references (`_log` / `_error`) via `applySync`.
 */
export function getConsoleSetupCode(
	budget: ConsoleSerializationBudget = DEFAULT_CONSOLE_SERIALIZATION_BUDGET,
): string {
	const normalizedBudget = normalizeBudget(budget);
	return `
	      // tsx/esbuild may emit __name(...) wrappers inside function source strings.
	      const __name = (value) => value;
	      const __consoleBudget = ${JSON.stringify(normalizedBudget)};
	      const normalizeBudget = ${normalizeBudget.toString()};
	      const safeStringifyConsoleValueWithBudget = ${safeStringifyConsoleValueWithBudget.toString()};
      const safeStringifyConsoleValue = ${safeStringifyConsoleValue.toString()};
      const formatConsoleArgs = ${formatConsoleArgs.toString()};

      globalThis.console = {
        log: (...args) => _log(formatConsoleArgs(args, __consoleBudget) + "\\n"),
        error: (...args) => _error(formatConsoleArgs(args, __consoleBudget) + "\\n"),
        warn: (...args) => _error(formatConsoleArgs(args, __consoleBudget) + "\\n"),
        info: (...args) => _log(formatConsoleArgs(args, __consoleBudget) + "\\n"),
        debug: (...args) => _log(formatConsoleArgs(args, __consoleBudget) + "\\n"),
        trace: (...args) => _error(formatConsoleArgs(args, __consoleBudget) + "\\n"),
        dir: (...args) => _log(formatConsoleArgs(args, __consoleBudget) + "\\n"),
        table: (...args) => _log(formatConsoleArgs(args, __consoleBudget) + "\\n"),
      };
    `;
}
