import {
	type BindingFunction,
	type BindingTree,
	NodeExecutionDriver as CoreNodeExecutionDriver,
	NodeFileSystem as CoreNodeFileSystem,
	NodeRuntime as CoreNodeRuntime,
	allowAll as coreAllowAll,
	allowAllChildProcess as coreAllowAllChildProcess,
	allowAllEnv as coreAllowAllEnv,
	allowAllFs as coreAllowAllFs,
	allowAllNetwork as coreAllowAllNetwork,
	createDefaultNetworkAdapter as coreCreateDefaultNetworkAdapter,
	createInMemoryFileSystem as coreCreateInMemoryFileSystem,
	createKernel as coreCreateKernel,
	createNodeDriver as coreCreateNodeDriver,
	createNodeHostCommandExecutor as coreCreateNodeHostCommandExecutor,
	createNodeRuntime as coreCreateNodeRuntime,
	createNodeRuntimeDriverFactory as coreCreateNodeRuntimeDriverFactory,
	exists as coreExists,
	isPrivateIp as coreIsPrivateIp,
	mkdir as coreMkdir,
	readDirWithTypes as coreReadDirWithTypes,
	rename as coreRename,
	stat as coreStat,
	type DefaultNetworkAdapterOptions,
	type DirEntry,
	type ExecOptions,
	type ExecResult,
	type Kernel,
	type KernelInterface,
	type ModuleAccessOptions,
	type NetworkAdapter,
	type NodeRuntimeDriver,
	type NodeRuntimeDriverFactory,
	type NodeRuntimeDriverFactoryOptions,
	type NodeRuntimeOptions,
	type OSConfig,
	type Permissions,
	type ProcessConfig,
	type ResourceBudgets,
	type RunResult,
	type StatInfo,
	type StdioChannel,
	type StdioEvent,
	type StdioHook,
	type TimingMitigation,
	type VirtualFileSystem,
} from "@rivet-dev/agent-os/internal/runtime-compat";
import * as secureExec from "secure-exec";
import { describe, expect, it } from "vitest";

describe("secure-exec", () => {
	it("re-exports the stable compatibility surface from Agent OS", () => {
		expect(secureExec.NodeRuntime).toBe(CoreNodeRuntime);
		expect(secureExec.NodeExecutionDriver).toBe(CoreNodeExecutionDriver);
		expect(secureExec.NodeFileSystem).toBe(CoreNodeFileSystem);
		expect(secureExec.createDefaultNetworkAdapter).toBe(
			coreCreateDefaultNetworkAdapter,
		);
		expect(secureExec.createNodeDriver).toBe(coreCreateNodeDriver);
		expect(secureExec.createNodeHostCommandExecutor).toBe(
			coreCreateNodeHostCommandExecutor,
		);
		expect(secureExec.createNodeRuntime).toBe(coreCreateNodeRuntime);
		expect(secureExec.createNodeRuntimeDriverFactory).toBe(
			coreCreateNodeRuntimeDriverFactory,
		);
		expect(secureExec.createKernel).toBe(coreCreateKernel);
		expect(secureExec.createInMemoryFileSystem).toBe(
			coreCreateInMemoryFileSystem,
		);
		expect(secureExec.allowAll).toBe(coreAllowAll);
		expect(secureExec.allowAllFs).toBe(coreAllowAllFs);
		expect(secureExec.allowAllNetwork).toBe(coreAllowAllNetwork);
		expect(secureExec.allowAllChildProcess).toBe(coreAllowAllChildProcess);
		expect(secureExec.allowAllEnv).toBe(coreAllowAllEnv);
		expect(secureExec.exists).toBe(coreExists);
		expect(secureExec.stat).toBe(coreStat);
		expect(secureExec.rename).toBe(coreRename);
		expect(secureExec.readDirWithTypes).toBe(coreReadDirWithTypes);
		expect(secureExec.mkdir).toBe(coreMkdir);
		expect(secureExec.isPrivateIp).toBe(coreIsPrivateIp);
	});

	it("preserves the published type surface through TypeScript", () => {
		void (null as BindingFunction | null);
		void (null as BindingTree | null);
		void (null as DefaultNetworkAdapterOptions | null);
		void (null as DirEntry | null);
		void (null as ExecOptions | null);
		void (null as ExecResult | null);
		void (null as Kernel | null);
		void (null as KernelInterface | null);
		void (null as ModuleAccessOptions | null);
		void (null as NetworkAdapter | null);
		void (null as NodeRuntimeDriver | null);
		void (null as NodeRuntimeDriverFactory | null);
		void (null as NodeRuntimeDriverFactoryOptions | null);
		void (null as NodeRuntimeOptions | null);
		void (null as OSConfig | null);
		void (null as Permissions | null);
		void (null as ProcessConfig | null);
		void (null as ResourceBudgets | null);
		void (null as RunResult | null);
		void (null as StatInfo | null);
		void (null as StdioChannel | null);
		void (null as StdioEvent | null);
		void (null as StdioHook | null);
		void (null as TimingMitigation | null);
		void (null as VirtualFileSystem | null);

		expect(true).toBe(true);
	});

	it("does not expose deferred browser or python subpaths", async () => {
		const importDeferred = (specifier: string) =>
			new Function("target", "return import(target)")(
				specifier,
			) as Promise<unknown>;

		await expect(importDeferred("secure-exec/browser")).rejects.toThrow();
		await expect(importDeferred("secure-exec/python")).rejects.toThrow();
	});
});
