import * as protocol from "./generated-protocol.js";

export type LiveOwnershipScope =
	| { scope: "connection"; connection_id: string }
	| { scope: "session"; connection_id: string; session_id: string }
	| {
			scope: "vm";
			connection_id: string;
			session_id: string;
			vm_id: string;
	  };

export function ownershipSelectorKey(ownership: LiveOwnershipScope): string {
	switch (ownership.scope) {
		case "connection":
			return `connection:${ownership.connection_id}`;
		case "session":
			return `session:${ownership.connection_id}:${ownership.session_id}`;
		case "vm":
			return `vm:${ownership.connection_id}:${ownership.session_id}:${ownership.vm_id}`;
	}
}

export function ownershipMatchesSelector(
	selector: LiveOwnershipScope | undefined,
	ownership: LiveOwnershipScope,
): boolean {
	if (!selector) {
		return true;
	}
	switch (selector.scope) {
		case "connection":
			return (
				ownership.scope === "connection" &&
				selector.connection_id === ownership.connection_id
			);
		case "session":
			return (
				ownership.scope === "session" &&
				selector.connection_id === ownership.connection_id &&
				selector.session_id === ownership.session_id
			);
		case "vm":
			return (
				ownership.scope === "vm" &&
				selector.connection_id === ownership.connection_id &&
				selector.session_id === ownership.session_id &&
				selector.vm_id === ownership.vm_id
			);
	}
}

export function toGeneratedOwnershipScope(
	ownership: LiveOwnershipScope,
): protocol.OwnershipScope {
	switch (ownership.scope) {
		case "connection":
			return {
				tag: "ConnectionOwnership",
				val: { connectionId: ownership.connection_id },
			};
		case "session":
			return {
				tag: "SessionOwnership",
				val: {
					connectionId: ownership.connection_id,
					sessionId: ownership.session_id,
				},
			};
		case "vm":
			return {
				tag: "VmOwnership",
				val: {
					connectionId: ownership.connection_id,
					sessionId: ownership.session_id,
					vmId: ownership.vm_id,
				},
			};
	}
}

export function fromGeneratedOwnershipScope(
	ownership: protocol.OwnershipScope,
): LiveOwnershipScope {
	switch (ownership.tag) {
		case "ConnectionOwnership":
			return {
				scope: "connection",
				connection_id: ownership.val.connectionId,
			};
		case "SessionOwnership":
			return {
				scope: "session",
				connection_id: ownership.val.connectionId,
				session_id: ownership.val.sessionId,
			};
		case "VmOwnership":
			return {
				scope: "vm",
				connection_id: ownership.val.connectionId,
				session_id: ownership.val.sessionId,
				vm_id: ownership.val.vmId,
			};
	}
}
