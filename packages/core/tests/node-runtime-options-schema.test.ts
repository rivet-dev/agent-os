import { describe, expect, test } from "vitest";
import { NodeRuntime, nodeRuntimeCreateOptionsSchema } from "../src/index.js";
import { createInMemoryFileSystem } from "../src/test-runtime.js";

describe("NodeRuntime create options validation", () => {
	test("rejects unknown top-level options before booting a VM", async () => {
		await expect(
			NodeRuntime.create({
				filesystem: createInMemoryFileSystem(),
				notARealOption: true,
			} as never),
		).rejects.toThrow(/notARealOption/);
	});

	test("rejects unknown nested permission fields", () => {
		expect(() =>
			nodeRuntimeCreateOptionsSchema.parse({
				filesystem: createInMemoryFileSystem(),
				permissions: {
					filesystem: "allow",
				},
			}),
		).toThrow(/filesystem/);
	});

	test("bounds and materializes Linux account records", () => {
		const filesystem = createInMemoryFileSystem();
		const exactPasswdRecord = {
			uid: 0,
			gid: 0,
			username: "u",
			homedir: "/",
			shell: "/",
			gecos: "x".repeat(4083),
		};
		expect(
			nodeRuntimeCreateOptionsSchema.safeParse({
				filesystem,
				user: exactPasswdRecord,
			}).success,
		).toBe(true);
		expect(
			nodeRuntimeCreateOptionsSchema.safeParse({
				filesystem,
				user: { ...exactPasswdRecord, gecos: "😀".repeat(1021) },
			}).success,
		).toBe(false);
		expect(
			nodeRuntimeCreateOptionsSchema.safeParse({
				filesystem,
				user: {
					uid: 0,
					gid: 0,
					username: "root",
					supplementaryGids: [44],
					groups: [{ gid: 99, name: "group44", members: [] }],
				},
			}).success,
		).toBe(false);
		expect(
			nodeRuntimeCreateOptionsSchema.safeParse({
				filesystem,
				user: {
					groups: [
						{
							gid: 7,
							name: "g",
							members: Array.from({ length: 257 }, (_, index) => `m${index}`),
						},
					],
				},
			}).success,
		).toBe(false);
	});
});
