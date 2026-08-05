import { describe, expect, it } from "vitest";
import * as protocol from "../src/generated-protocol.js";
import {
	fromGeneratedGuestFilesystemOperation,
	fromGeneratedProcessSnapshotStatus,
	fromGeneratedRootFilesystemEntryEncoding,
	fromGeneratedRootFilesystemEntryKind,
	fromGeneratedSignalDispositionAction,
	fromGeneratedStreamChannel,
	fromGeneratedVmLifecycleState,
	toGeneratedDisposeReason,
	toGeneratedGuestFilesystemOperation,
	toGeneratedGuestRuntimeKind,
	toGeneratedPermissionMode,
	toGeneratedRootFilesystemEntryEncoding,
	toGeneratedRootFilesystemEntryKind,
	toGeneratedRootFilesystemMode,
	toGeneratedWasmPermissionTier,
} from "../src/protocol-maps.js";

describe("protocol maps", () => {
	it("maps live scalar strings to generated enums", () => {
		expect(toGeneratedPermissionMode("allow")).toBe(
			protocol.PermissionMode.Allow,
		);
		expect(toGeneratedGuestRuntimeKind("python")).toBe(
			protocol.GuestRuntimeKind.Python,
		);
		expect(toGeneratedDisposeReason("host_shutdown")).toBe(
			protocol.DisposeReason.HostShutdown,
		);
		expect(toGeneratedRootFilesystemMode("read_only")).toBe(
			protocol.RootFilesystemMode.ReadOnly,
		);
		expect(toGeneratedRootFilesystemEntryKind("symlink")).toBe(
			protocol.RootFilesystemEntryKind.Symlink,
		);
		expect(toGeneratedRootFilesystemEntryEncoding("base64")).toBe(
			protocol.RootFilesystemEntryEncoding.BasE64,
		);
		expect(toGeneratedWasmPermissionTier("read-write")).toBe(
			protocol.WasmPermissionTier.ReadWrite,
		);
		expect(toGeneratedGuestFilesystemOperation("pread")).toBe(
			protocol.GuestFilesystemOperation.Pread,
		);
	});

	it("maps generated enums to live scalar strings", () => {
		expect(
			fromGeneratedVmLifecycleState(protocol.VmLifecycleState.Disposed),
		).toBe("disposed");
		expect(fromGeneratedStreamChannel(protocol.StreamChannel.Stderr)).toBe(
			"stderr",
		);
		expect(
			fromGeneratedProcessSnapshotStatus(
				protocol.ProcessSnapshotStatus.Stopped,
			),
		).toBe("stopped");
		expect(
			fromGeneratedSignalDispositionAction(
				protocol.SignalDispositionAction.Ignore,
			),
		).toBe("ignore");
		expect(
			fromGeneratedRootFilesystemEntryKind(
				protocol.RootFilesystemEntryKind.Directory,
			),
		).toBe("directory");
		expect(
			fromGeneratedRootFilesystemEntryEncoding(
				protocol.RootFilesystemEntryEncoding.UtF8,
			),
		).toBe("utf8");
		expect(
			fromGeneratedGuestFilesystemOperation(
				protocol.GuestFilesystemOperation.ReadLink,
			),
		).toBe("read_link");
	});
});
