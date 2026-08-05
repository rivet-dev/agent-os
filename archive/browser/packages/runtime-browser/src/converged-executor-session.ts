// Converged executor session bootstrap.
//
// Brings up a VM inside the wasm sidecar over the synchronous `pushFrame`
// dispatcher (authenticate -> open_session -> create_vm), then hands out a
// per-execution `ConvergedSyncBridgeHandler` bound to that VM's ownership. This
// is the glue between the wasm sidecar and the guest Worker's sync-bridge: the
// handshake runs once at setup, and each guest execution gets a synchronous
// handler that routes its syscalls to the kernel.
//
// The handshake uses the same client identity and versions as the core
// `SidecarProcess` so the wasm sidecar accepts it. Unit-tested with a fake
// synchronous `pushFrame`.

import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import type { LiveRootFilesystemEntry as RootFilesystemEntry } from "@rivet-dev/agentos-runtime-core/filesystem";
import type { LiveOwnershipScope } from "@rivet-dev/agentos-runtime-core/ownership";
import { SIDECAR_PROTOCOL_SCHEMA } from "@rivet-dev/agentos-runtime-core/protocol-schema";
import type { ProtocolFramePayloadCodec } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import type { LiveRequestPayload } from "@rivet-dev/agentos-runtime-core/request-payloads";
import {
	ConvergedSyncBridgeHandler,
	type ConvergedPushFrame,
	PushFrameSidecarTransport,
} from "./converged-sync-bridge-handler.js";

// Mirror `SidecarProcess`'s client identity so the sidecar handshake succeeds.
const CLIENT_NAME = "agentos-core-client";
const AUTH_TOKEN = "agentos-core-client-token";
const BRIDGE_CONTRACT_VERSION = 1;

type GuestRuntimeKind = Extract<
	LiveRequestPayload,
	{ type: "create_vm" }
>["runtime"];

export interface ConvergedExecutorSessionOptions {
	pushFrame: ConvergedPushFrame;
	codec?: ProtocolFramePayloadCodec;
}

export interface ConvergedVmBootstrap {
	runtime: GuestRuntimeKind;
	config: CreateVmConfig;
	sessionMetadata?: Record<string, string>;
}

/** A bootstrapped VM inside the wasm sidecar. */
export interface ConvergedVm {
	connectionId: string;
	sessionId: string;
	vmId: string;
}

export class ConvergedExecutorSession {
	private readonly pushFrame: ConvergedPushFrame;
	private readonly codec: ProtocolFramePayloadCodec;
	private vm: ConvergedVm | undefined;

	constructor(options: ConvergedExecutorSessionOptions) {
		this.pushFrame = options.pushFrame;
		this.codec = options.codec ?? "bare";
	}

	/** The bootstrapped VM, or throw if `bootstrap()` has not run. */
	get currentVm(): ConvergedVm {
		if (!this.vm) {
			throw new Error("converged executor session has not bootstrapped a VM");
		}
		return this.vm;
	}

	/** Run the authenticate -> open_session -> create_vm handshake. */
	bootstrap(options: ConvergedVmBootstrap): ConvergedVm {
		const authenticated = this.send(
			{ scope: "connection", connection_id: "client-hint" },
			{
				type: "authenticate",
				client_name: CLIENT_NAME,
				auth_token: AUTH_TOKEN,
				protocol_version: SIDECAR_PROTOCOL_SCHEMA.version,
				bridge_version: BRIDGE_CONTRACT_VERSION,
			},
		);
		if (authenticated.type !== "authenticated") {
			throw new Error(`unexpected authenticate response: ${authenticated.type}`);
		}
		const connectionId = authenticated.connection_id;

		const opened = this.send(
			{ scope: "connection", connection_id: connectionId },
			{
				type: "open_session",
				placement: { kind: "shared", pool: null },
				metadata: options.sessionMetadata ?? {},
			},
		);
		if (opened.type !== "session_opened") {
			throw new Error(`unexpected open_session response: ${opened.type}`);
		}
		const sessionId = opened.session_id;

		const created = this.send(
			{ scope: "session", connection_id: connectionId, session_id: sessionId },
			{ type: "create_vm", runtime: options.runtime, config: options.config },
		);
		if (created.type !== "vm_created") {
			throw new Error(`unexpected create_vm response: ${created.type}`);
		}

		this.vm = { connectionId, sessionId, vmId: created.vm_id };
		return this.vm;
	}

	/** A synchronous syscall handler scoped to the bootstrapped VM + execution. */
	handlerForExecution(executionId: string): ConvergedSyncBridgeHandler {
		return new ConvergedSyncBridgeHandler({
			transport: this.transportForVm(),
			executionId,
		});
	}

	/**
	 * Register a guest execution (kernel process) in the sidecar via an `execute`
	 * wire request, so guest `net.*`/`dgram.*` syscalls can resolve their
	 * `execution_id` to a kernel pid. The guest itself runs in the browser worker;
	 * this only owns the kernel-side process/socket lifecycle. Requires the wasm
	 * sidecar to be constructed with an execution host bridge whose
	 * `startExecution` echoes `processId` back as the execution id.
	 */
	registerExecution(options: {
		processId: string;
		entrypoint?: string;
		args?: readonly string[];
		cwd?: string;
	}): { processId: string } {
		const response = this.transportForVm().sendRequest({
			type: "execute",
			process_id: options.processId,
			runtime: "java_script",
			entrypoint: options.entrypoint,
			args: [...(options.args ?? [])],
			cwd: options.cwd,
		});
		if (response.type !== "process_started") {
			throw new Error(`unexpected execute response: ${response.type}`);
		}
		return { processId: response.process_id };
	}

	/**
	 * Snapshot the VM's root filesystem (the writable changes) so callers can
	 * persist them to host storage (e.g. OPFS) across runtimes.
	 */
	snapshotRootFilesystem(): RootFilesystemEntry[] {
		const response = this.transportForVm().sendRequest({
			type: "snapshot_root_filesystem",
		});
		if (response.type !== "root_filesystem_snapshot") {
			throw new Error(
				`unexpected snapshot_root_filesystem response: ${response.type}`,
			);
		}
		return response.entries;
	}

	/** A request transport bound to the bootstrapped VM ownership. */
	transportForVm(): PushFrameSidecarTransport {
		const vm = this.currentVm;
		return new PushFrameSidecarTransport({
			pushFrame: this.pushFrame,
			codec: this.codec,
			ownership: {
				scope: "vm",
				connection_id: vm.connectionId,
				session_id: vm.sessionId,
				vm_id: vm.vmId,
			},
		});
	}

	private send(ownership: LiveOwnershipScope, payload: LiveRequestPayload) {
		const transport = new PushFrameSidecarTransport({
			pushFrame: this.pushFrame,
			codec: this.codec,
			ownership,
		});
		return transport.sendRequest(payload);
	}
}
