import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sed } from "@agentos-software/common";
import { describe, expect, test } from "vitest";
import {
	actorBytes,
	actorHandle,
	startActorRuntime,
} from "./helpers/actor-runtime.js";

const RUN_E2E = process.env.AGENTOS_ACTOR_E2E === "1";

describe.skipIf(!RUN_E2E)("AgentOS real Rivet actor nightly software replay", () => {
	test("replays dynamic mounts and linked software after actor sleep", async () => {
		const storagePath = mkdtempSync(join(tmpdir(), "agentos-replay-e2e-"));
		let runtime: Awaited<ReturnType<typeof startActorRuntime>> | undefined;
		try {
			runtime = await startActorRuntime(storagePath);
			const handle = actorHandle(runtime.endpoint, `replay-${Date.now()}`);
			const mountPath = "/durable-dynamic-mount";
			await handle.mountFs({
				path: mountPath,
				plugin: {
					id: "chunked_actor_sqlite",
					config: {
						namespace: "dynamic-replay",
						chunkSize: 512 * 1024,
						inlineThreshold: 64 * 1024,
					},
				},
			});
			await handle.writeFile(`${mountPath}/message.txt`, "mounted-before-sleep");
			await handle.linkSoftware({ path: sed.packagePath });
			expect(
				(await handle.listSoftware()).some((entry: { commands: string[] }) =>
					entry.commands.includes("sed"),
				),
			).toBe(true);

			await handle.sleepActor();
			await new Promise((resolve) => setTimeout(resolve, 1_000));

			expect(await handle.listMounts()).toContainEqual(
				expect.objectContaining({ path: mountPath, readOnly: false }),
			);
			expect(
				new TextDecoder().decode(
					actorBytes(await handle.readFile(`${mountPath}/message.txt`)),
				),
			).toBe("mounted-before-sleep");
			expect(
				(await handle.listSoftware()).some((entry: { commands: string[] }) =>
					entry.commands.includes("sed"),
				),
			).toBe(true);
		} finally {
			await runtime?.stop();
			rmSync(storagePath, { recursive: true, force: true });
		}
	}, 180_000);
});
