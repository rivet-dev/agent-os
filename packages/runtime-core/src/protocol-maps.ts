import * as protocol from "./generated-protocol.js";

export type LiveGuestRuntimeKind = "java_script" | "python" | "web_assembly";
export type LiveDisposeReason =
	| "requested"
	| "connection_closed"
	| "host_shutdown";
export type LiveRootFilesystemMode = "ephemeral" | "read_only";
export type LiveRootFilesystemEntryKind = "file" | "directory" | "symlink";
export type LiveRootFilesystemEntryEncoding = "utf8" | "base64";
export type LiveWasmPermissionTier =
	| "full"
	| "read-write"
	| "read-only"
	| "isolated";
export type LiveStandaloneWasmBackend = "v8" | "wasmtime" | "wasmtime-threads";
export type LivePermissionMode = "allow" | "ask" | "deny";
export type LiveGuestFilesystemOperation =
	| "read_file"
	| "write_file"
	| "create_dir"
	| "mkdir"
	| "exists"
	| "stat"
	| "lstat"
	| "read_dir"
	| "read_dir_recursive"
	| "remove_file"
	| "remove_dir"
	| "remove"
	| "copy"
	| "move"
	| "rename"
	| "realpath"
	| "symlink"
	| "read_link"
	| "link"
	| "chmod"
	| "chown"
	| "utimes"
	| "truncate"
	| "pread"
	| "pwrite";
export type LiveFilesystemOperation =
	| "read"
	| "write"
	| "stat"
	| "read_dir"
	| "mkdir"
	| "remove"
	| "rename";
export type LiveVmLifecycleState =
	| "creating"
	| "ready"
	| "disposing"
	| "disposed"
	| "failed";
export type LiveStreamChannel = "stdout" | "stderr";
export type LiveProcessSnapshotStatus = "running" | "exited" | "stopped";
export type LiveSignalDispositionAction = "default" | "ignore" | "user";

export function toGeneratedPermissionMode(
	mode: LivePermissionMode,
): protocol.PermissionMode {
	switch (mode) {
		case "allow":
			return protocol.PermissionMode.Allow;
		case "ask":
			return protocol.PermissionMode.Ask;
		case "deny":
			return protocol.PermissionMode.Deny;
	}
}

export function toGeneratedGuestRuntimeKind(
	runtime: LiveGuestRuntimeKind,
): protocol.GuestRuntimeKind {
	switch (runtime) {
		case "java_script":
			return protocol.GuestRuntimeKind.JavaScript;
		case "python":
			return protocol.GuestRuntimeKind.Python;
		case "web_assembly":
			return protocol.GuestRuntimeKind.WebAssembly;
	}
}

export function toGeneratedDisposeReason(
	reason: LiveDisposeReason,
): protocol.DisposeReason {
	switch (reason) {
		case "requested":
			return protocol.DisposeReason.Requested;
		case "connection_closed":
			return protocol.DisposeReason.ConnectionClosed;
		case "host_shutdown":
			return protocol.DisposeReason.HostShutdown;
	}
}

export function toGeneratedRootFilesystemMode(
	mode: LiveRootFilesystemMode,
): protocol.RootFilesystemMode {
	switch (mode) {
		case "ephemeral":
			return protocol.RootFilesystemMode.Ephemeral;
		case "read_only":
			return protocol.RootFilesystemMode.ReadOnly;
	}
}

export function toGeneratedRootFilesystemEntryKind(
	kind: LiveRootFilesystemEntryKind,
): protocol.RootFilesystemEntryKind {
	switch (kind) {
		case "file":
			return protocol.RootFilesystemEntryKind.File;
		case "directory":
			return protocol.RootFilesystemEntryKind.Directory;
		case "symlink":
			return protocol.RootFilesystemEntryKind.Symlink;
	}
}

export function toGeneratedRootFilesystemEntryEncoding(
	encoding: LiveRootFilesystemEntryEncoding,
): protocol.RootFilesystemEntryEncoding {
	switch (encoding) {
		case "utf8":
			return protocol.RootFilesystemEntryEncoding.UtF8;
		case "base64":
			return protocol.RootFilesystemEntryEncoding.BasE64;
	}
}

export function toGeneratedWasmPermissionTier(
	tier: LiveWasmPermissionTier,
): protocol.WasmPermissionTier {
	switch (tier) {
		case "full":
			return protocol.WasmPermissionTier.Full;
		case "read-write":
			return protocol.WasmPermissionTier.ReadWrite;
		case "read-only":
			return protocol.WasmPermissionTier.ReadOnly;
		case "isolated":
			return protocol.WasmPermissionTier.Isolated;
	}
}

export function toGeneratedStandaloneWasmBackend(
	backend: LiveStandaloneWasmBackend,
): protocol.StandaloneWasmBackend {
	switch (backend) {
		case "v8":
			return protocol.StandaloneWasmBackend.V8;
		case "wasmtime":
			return protocol.StandaloneWasmBackend.Wasmtime;
		case "wasmtime-threads":
			return protocol.StandaloneWasmBackend.WasmtimeThreads;
	}
}

export function toGeneratedGuestFilesystemOperation(
	operation: LiveGuestFilesystemOperation,
): protocol.GuestFilesystemOperation {
	switch (operation) {
		case "read_file":
			return protocol.GuestFilesystemOperation.ReadFile;
		case "write_file":
			return protocol.GuestFilesystemOperation.WriteFile;
		case "create_dir":
			return protocol.GuestFilesystemOperation.CreateDir;
		case "mkdir":
			return protocol.GuestFilesystemOperation.Mkdir;
		case "exists":
			return protocol.GuestFilesystemOperation.Exists;
		case "stat":
			return protocol.GuestFilesystemOperation.Stat;
		case "lstat":
			return protocol.GuestFilesystemOperation.Lstat;
		case "read_dir":
			return protocol.GuestFilesystemOperation.ReadDir;
		case "read_dir_recursive":
			return protocol.GuestFilesystemOperation.ReadDirRecursive;
		case "remove_file":
			return protocol.GuestFilesystemOperation.RemoveFile;
		case "remove_dir":
			return protocol.GuestFilesystemOperation.RemoveDir;
		case "remove":
			return protocol.GuestFilesystemOperation.Remove;
		case "copy":
			return protocol.GuestFilesystemOperation.Copy;
		case "move":
			return protocol.GuestFilesystemOperation.Move;
		case "rename":
			return protocol.GuestFilesystemOperation.Rename;
		case "realpath":
			return protocol.GuestFilesystemOperation.Realpath;
		case "symlink":
			return protocol.GuestFilesystemOperation.Symlink;
		case "read_link":
			return protocol.GuestFilesystemOperation.ReadLink;
		case "link":
			return protocol.GuestFilesystemOperation.Link;
		case "chmod":
			return protocol.GuestFilesystemOperation.Chmod;
		case "chown":
			return protocol.GuestFilesystemOperation.Chown;
		case "utimes":
			return protocol.GuestFilesystemOperation.Utimes;
		case "truncate":
			return protocol.GuestFilesystemOperation.Truncate;
		case "pread":
			return protocol.GuestFilesystemOperation.Pread;
		case "pwrite":
			return protocol.GuestFilesystemOperation.Pwrite;
	}
}

export function toGeneratedFilesystemOperation(
	operation: LiveFilesystemOperation,
): protocol.FilesystemOperation {
	switch (operation) {
		case "read":
			return protocol.FilesystemOperation.Read;
		case "write":
			return protocol.FilesystemOperation.Write;
		case "stat":
			return protocol.FilesystemOperation.Stat;
		case "read_dir":
			return protocol.FilesystemOperation.ReadDir;
		case "mkdir":
			return protocol.FilesystemOperation.Mkdir;
		case "remove":
			return protocol.FilesystemOperation.Remove;
		case "rename":
			return protocol.FilesystemOperation.Rename;
	}
}

export function fromGeneratedFilesystemOperation(
	operation: protocol.FilesystemOperation,
): LiveFilesystemOperation {
	switch (operation) {
		case protocol.FilesystemOperation.Read:
			return "read";
		case protocol.FilesystemOperation.Write:
			return "write";
		case protocol.FilesystemOperation.Stat:
			return "stat";
		case protocol.FilesystemOperation.ReadDir:
			return "read_dir";
		case protocol.FilesystemOperation.Mkdir:
			return "mkdir";
		case protocol.FilesystemOperation.Remove:
			return "remove";
		case protocol.FilesystemOperation.Rename:
			return "rename";
	}
}

export function fromGeneratedVmLifecycleState(
	state: protocol.VmLifecycleState,
): LiveVmLifecycleState {
	switch (state) {
		case protocol.VmLifecycleState.Creating:
			return "creating";
		case protocol.VmLifecycleState.Ready:
			return "ready";
		case protocol.VmLifecycleState.Disposing:
			return "disposing";
		case protocol.VmLifecycleState.Disposed:
			return "disposed";
		case protocol.VmLifecycleState.Failed:
			return "failed";
	}
}

export function fromGeneratedStreamChannel(
	channel: protocol.StreamChannel,
): LiveStreamChannel {
	switch (channel) {
		case protocol.StreamChannel.Stdout:
			return "stdout";
		case protocol.StreamChannel.Stderr:
			return "stderr";
	}
}

export function fromGeneratedProcessSnapshotStatus(
	status: protocol.ProcessSnapshotStatus,
): LiveProcessSnapshotStatus {
	switch (status) {
		case protocol.ProcessSnapshotStatus.Running:
			return "running";
		case protocol.ProcessSnapshotStatus.Exited:
			return "exited";
		case protocol.ProcessSnapshotStatus.Stopped:
			return "stopped";
	}
}

export function fromGeneratedSignalDispositionAction(
	action: protocol.SignalDispositionAction,
): LiveSignalDispositionAction {
	switch (action) {
		case protocol.SignalDispositionAction.Default:
			return "default";
		case protocol.SignalDispositionAction.Ignore:
			return "ignore";
		case protocol.SignalDispositionAction.User:
			return "user";
	}
}

export function fromGeneratedRootFilesystemEntryKind(
	kind: protocol.RootFilesystemEntryKind,
): LiveRootFilesystemEntryKind {
	switch (kind) {
		case protocol.RootFilesystemEntryKind.File:
			return "file";
		case protocol.RootFilesystemEntryKind.Directory:
			return "directory";
		case protocol.RootFilesystemEntryKind.Symlink:
			return "symlink";
	}
}

export function fromGeneratedRootFilesystemEntryEncoding(
	encoding: protocol.RootFilesystemEntryEncoding,
): LiveRootFilesystemEntryEncoding {
	switch (encoding) {
		case protocol.RootFilesystemEntryEncoding.UtF8:
			return "utf8";
		case protocol.RootFilesystemEntryEncoding.BasE64:
			return "base64";
	}
}

export function fromGeneratedGuestFilesystemOperation(
	operation: protocol.GuestFilesystemOperation,
): LiveGuestFilesystemOperation {
	switch (operation) {
		case protocol.GuestFilesystemOperation.ReadFile:
			return "read_file";
		case protocol.GuestFilesystemOperation.WriteFile:
			return "write_file";
		case protocol.GuestFilesystemOperation.CreateDir:
			return "create_dir";
		case protocol.GuestFilesystemOperation.Mkdir:
			return "mkdir";
		case protocol.GuestFilesystemOperation.Exists:
			return "exists";
		case protocol.GuestFilesystemOperation.Stat:
			return "stat";
		case protocol.GuestFilesystemOperation.Lstat:
			return "lstat";
		case protocol.GuestFilesystemOperation.ReadDir:
			return "read_dir";
		case protocol.GuestFilesystemOperation.ReadDirRecursive:
			return "read_dir_recursive";
		case protocol.GuestFilesystemOperation.RemoveFile:
			return "remove_file";
		case protocol.GuestFilesystemOperation.RemoveDir:
			return "remove_dir";
		case protocol.GuestFilesystemOperation.Remove:
			return "remove";
		case protocol.GuestFilesystemOperation.Copy:
			return "copy";
		case protocol.GuestFilesystemOperation.Move:
			return "move";
		case protocol.GuestFilesystemOperation.Rename:
			return "rename";
		case protocol.GuestFilesystemOperation.Realpath:
			return "realpath";
		case protocol.GuestFilesystemOperation.Symlink:
			return "symlink";
		case protocol.GuestFilesystemOperation.ReadLink:
			return "read_link";
		case protocol.GuestFilesystemOperation.Link:
			return "link";
		case protocol.GuestFilesystemOperation.Chmod:
			return "chmod";
		case protocol.GuestFilesystemOperation.Chown:
			return "chown";
		case protocol.GuestFilesystemOperation.Utimes:
			return "utimes";
		case protocol.GuestFilesystemOperation.Truncate:
			return "truncate";
		case protocol.GuestFilesystemOperation.Pread:
			return "pread";
		case protocol.GuestFilesystemOperation.Pwrite:
			return "pwrite";
	}
}
