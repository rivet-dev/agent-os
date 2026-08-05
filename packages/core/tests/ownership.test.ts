import { describe, expect, it } from "vitest";
import {
	fromGeneratedOwnershipScope,
	ownershipMatchesSelector,
	ownershipSelectorKey,
	toGeneratedOwnershipScope,
	type LiveOwnershipScope,
} from "../src/ownership.js";

const vmOwnership: LiveOwnershipScope = {
	scope: "vm",
	connection_id: "conn",
	session_id: "session",
	vm_id: "vm",
};

describe("ownership", () => {
	it("builds stable selector keys", () => {
		expect(ownershipSelectorKey({ scope: "connection", connection_id: "c" }))
			.toBe("connection:c");
		expect(
			ownershipSelectorKey({
				scope: "session",
				connection_id: "c",
				session_id: "s",
			}),
		).toBe("session:c:s");
		expect(ownershipSelectorKey(vmOwnership)).toBe("vm:conn:session:vm");
	});

	it("matches ownership selectors exactly", () => {
		expect(ownershipMatchesSelector(undefined, vmOwnership)).toBe(true);
		expect(ownershipMatchesSelector(vmOwnership, vmOwnership)).toBe(true);
		expect(
			ownershipMatchesSelector(
				{ scope: "session", connection_id: "conn", session_id: "session" },
				vmOwnership,
			),
		).toBe(false);
	});

	it("round-trips live ownership through generated protocol shapes", () => {
		expect(
			fromGeneratedOwnershipScope(toGeneratedOwnershipScope(vmOwnership)),
		).toEqual(vmOwnership);
	});
});
