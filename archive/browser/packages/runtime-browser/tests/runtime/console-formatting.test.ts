import { describe, expect, it } from "vitest";
import { getRequireSetupCode } from "../../src/runtime.js";

describe("browser console formatting", () => {
	it("routes console output through util.formatWithOptions", () => {
		const globalRecord = globalThis as typeof globalThis & Record<string, unknown>;
		const previous = new Map<string, unknown>();
		const keys = [
			"__agentOSEncoding",
			"_loadPolyfill",
			"_log",
			"_error",
			"console",
			"require",
			"_moduleCache",
			"_currentModule",
		];
		for (const key of keys) {
			previous.set(key, globalRecord[key]);
		}

		const stdout: unknown[][] = [];
		const stderr: unknown[][] = [];
		globalRecord._loadPolyfill = (moduleName: string) => {
			if (moduleName !== "util") return null;
			return `
				module.exports = {
					formatWithOptions(options, ...args) {
						return "UTIL:" + options.colors + ":" + args.map((value) => typeof value).join(",");
					},
				};
			`;
		};
		globalRecord._log = (...args: unknown[]) => {
			stdout.push(args);
		};
		globalRecord._error = (...args: unknown[]) => {
			stderr.push(args);
		};

		try {
			new Function(getRequireSetupCode())();

			console.log({ nested: true }, "value");
			console.warn("warning");

			expect(stdout).toEqual([["UTIL:false:object,string\n"]]);
			expect(stderr).toEqual([["UTIL:false:string\n"]]);
		} finally {
			for (const key of keys) {
				const value = previous.get(key);
				if (value === undefined) {
					delete globalRecord[key];
				} else {
					globalRecord[key] = value;
				}
			}
		}
	});
});
