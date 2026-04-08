/**
 * Custom bindings: host-to-sandbox function bridge.
 *
 * Users register a BindingTree of host-side functions via the `bindings`
 * option. The tree is validated, flattened to __bind.* prefixed keys, and
 * merged into bridgeHandlers so sandbox code can call them through the bridge.
 */

import type { BridgeHandler } from "./bridge-handlers.js";
import { BRIDGE_GLOBAL_KEY_LIST } from "./bridge-contract.js";

/** A user-defined host-side function callable from the sandbox. */
export type BindingFunction = (...args: unknown[]) => unknown | Promise<unknown>;

/** A nested tree of binding functions. Nesting depth limited to 4. */
export interface BindingTree {
	[key: string]: BindingFunction | BindingTree;
}

/** Prefix for flattened binding keys in the bridge handler map. */
export const BINDING_PREFIX = "__bind.";

const MAX_DEPTH = 4;
const MAX_LEAVES = 64;
const JS_IDENTIFIER_RE = /^[a-zA-Z_$][a-zA-Z0-9_$]*$/;

// eslint-disable-next-line @typescript-eslint/no-empty-function
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

export interface FlattenedBinding {
	key: string;
	handler: BridgeHandler;
	isAsync: boolean;
}

/**
 * Validate and flatten a BindingTree into prefixed bridge handler entries.
 *
 * Throws on:
 * - Invalid JS identifiers as keys
 * - Nesting depth > 4
 * - More than 64 leaf functions
 * - Binding keys starting with `_` (reserved for internal bridge names)
 */
export function flattenBindingTree(tree: BindingTree): FlattenedBinding[] {
	const result: FlattenedBinding[] = [];
	const internalKeys = new Set<string>(BRIDGE_GLOBAL_KEY_LIST as readonly string[]);

	function walk(node: BindingTree, path: string[], depth: number): void {
		if (depth > MAX_DEPTH) {
			throw new Error(
				`Binding tree exceeds maximum nesting depth of ${MAX_DEPTH} at path: ${path.join(".")}`,
			);
		}

		for (const key of Object.keys(node)) {
			if (!JS_IDENTIFIER_RE.test(key)) {
				throw new Error(
					`Invalid binding key "${key}": must be a valid JavaScript identifier`,
				);
			}

			// Reject keys starting with _ to avoid collision with internal bridge names
			if (key.startsWith("_")) {
				throw new Error(
					`Binding key "${key}" starts with "_" which is reserved for internal bridge names`,
				);
			}

			const fullPath = [...path, key];
			const value = node[key];

			if (typeof value === "function") {
				const flatKey = BINDING_PREFIX + fullPath.join(".");

				// Double-check flattened key doesn't collide with known internals
				if (internalKeys.has(flatKey)) {
					throw new Error(
						`Binding "${fullPath.join(".")}" collides with internal bridge name "${flatKey}"`,
					);
				}

				result.push({
					key: flatKey,
					handler: value as BridgeHandler,
					isAsync: value instanceof AsyncFunction,
				});

				if (result.length > MAX_LEAVES) {
					throw new Error(
						`Binding tree exceeds maximum of ${MAX_LEAVES} leaf functions`,
					);
				}
			} else if (typeof value === "object" && value !== null) {
				walk(value as BindingTree, fullPath, depth + 1);
			} else {
				throw new Error(
					`Invalid binding value at "${fullPath.join(".")}": expected function or object, got ${typeof value}`,
				);
			}
		}
	}

	walk(tree, [], 1);
	return result;
}
