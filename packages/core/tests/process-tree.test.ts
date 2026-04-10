import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

describe("processTree()", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	}, 30_000);

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
		}
	}, 30_000);

	test("returns empty array on fresh VM", () => {
		expect(vm.processTree()).toEqual([]);
	});

	test("spawned process appears as a root in the tree", async () => {
		await vm.writeFile("/tmp/stay.mjs", "setTimeout(() => {}, 30000);");
		const { pid } = vm.spawn("node", ["/tmp/stay.mjs"], {
			env: { HOME: "/home/user" },
		});

		const tree = vm.processTree();
		// The node process should be a root (ppid 0 or orphan)
		const root = tree.find((n) => n.pid === pid);
		expect(root).toBeDefined();
		expect(root?.children).toEqual([]);

		vm.killProcess(pid);
	}, 30_000);

	test("guest child_process.spawn children appear under the tracked parent", async () => {
		let childPid: string | null = null;

		await vm.writeFile(
			"/tmp/parent.mjs",
			`
import { spawn } from "node:child_process";
const child = spawn("node", ["/tmp/child.mjs"]);
console.log("CHILD_PID:" + child.pid);
// Keep parent alive
setTimeout(() => {}, 30000);
`,
		);
		await vm.writeFile("/tmp/child.mjs", "setTimeout(() => {}, 30000);");

		const { pid } = vm.spawn("node", ["/tmp/parent.mjs"], {
			env: { HOME: "/home/user" },
			onStdout: (data) => {
				const text = new TextDecoder().decode(data);
				const match = text.match(/CHILD_PID:(\d+)/);
				if (match) {
					childPid = match[1];
				}
			},
		});

		for (let attempt = 0; attempt < 20 && childPid === null; attempt++) {
			await new Promise((r) => setTimeout(r, 100));
		}

		let parentNode = vm.processTree().find((node) => node.pid === pid);
		for (let attempt = 0; attempt < 20; attempt++) {
			parentNode = vm.processTree().find((node) => node.pid === pid);
			if (
				parentNode?.children.some((child) => child.pid === Number(childPid))
			) {
				break;
			}
			await new Promise((resolve) => setTimeout(resolve, 100));
		}

		expect(parentNode).toBeDefined();
		expect(childPid).not.toBeNull();
		expect(parentNode?.children.map((child) => child.pid)).toContain(
			Number(childPid),
		);

		vm.killProcess(pid);
	}, 30_000);
});
