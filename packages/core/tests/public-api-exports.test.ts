import { describe, expect, test } from "vitest";
import {
	isAcpTimeoutErrorData,
	type AcpTimeoutErrorData,
	type ExecOptions,
	type HostDirMountPluginConfig,
	type JsonRpcErrorData,
	type KernelExecOptions,
	type KernelExecResult,
	type KernelSpawnOptions,
	type MountConfigJsonPrimitive,
	type OpenShellOptions,
	type PromptCapabilities,
	type PromptResult,
	type StdioChannel,
	type TimingMitigation,
} from "../src/index.js";

describe("root public API exports", () => {
	test("re-exports current public SDK types from the root entrypoint", () => {
		void (null as AcpTimeoutErrorData | null);
		void (null as ExecOptions | null);
		void (null as HostDirMountPluginConfig | null);
		void (null as JsonRpcErrorData | null);
		void (null as KernelExecOptions | null);
		void (null as KernelExecResult | null);
		void (null as KernelSpawnOptions | null);
		void (null as MountConfigJsonPrimitive | null);
		void (null as OpenShellOptions | null);
		void (null as PromptCapabilities | null);
		void (null as PromptResult | null);
		void (null as StdioChannel | null);
		void (null as TimingMitigation | null);

		expect(true).toBe(true);
	});

	test("re-exports ACP timeout diagnostics helper from the root entrypoint", () => {
		const timeout: AcpTimeoutErrorData = {
			kind: "acp_timeout",
			method: "session/prompt",
			id: 7,
			timeoutMs: 5000,
			recentActivity: ["waiting for adapter"],
		};

		expect(isAcpTimeoutErrorData(timeout)).toBe(true);
		expect(isAcpTimeoutErrorData({ kind: "other" })).toBe(false);
	});
});
