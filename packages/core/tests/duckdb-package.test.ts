import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { existsSync } from "node:fs";
import coreutils from "@rivet-dev/agent-os-coreutils";
import duckdb from "../../../registry/software/duckdb/dist/index.js";
import httpGet from "../../../registry/software/http-get/dist/index.js";
import { AgentOs } from "../src/index.js";

const hasDuckdbPackage = existsSync(`${duckdb.commandDir}/duckdb`);
const hasHttpGetPackage = existsSync(`${httpGet.commandDir}/http_get`);
const hasCoreutilsPackage = existsSync(`${coreutils.commandDir}/sh`);

function closeServer(server: Server) {
	return new Promise<void>((resolve, reject) => {
		server.close((err) => {
			if (err) reject(err);
			else resolve();
		});
	});
}

describe.skipIf(!hasDuckdbPackage || !hasHttpGetPackage || !hasCoreutilsPackage)(
	"duckdb registry package",
	() => {
		let vm: AgentOs;

		beforeEach(async () => {
			vm = await AgentOs.create({ software: [coreutils, httpGet, duckdb] });
		});

		afterEach(async () => {
			await vm.dispose();
		});

		test("runs file-backed DuckDB DML through the registry package path", async () => {
			let result = await vm.exec(
				`duckdb -csv /tmp/app.duckdb -c "CREATE TABLE items(id INTEGER, value INTEGER); INSERT INTO items VALUES (1, 10), (2, 20); UPDATE items SET value = value + 1 WHERE id = 2;"`,
			);
			expect(result.exitCode).toBe(0);

			result = await vm.exec(
				`duckdb -csv /tmp/app.duckdb -c "SELECT id, value FROM items ORDER BY id;"`,
			);
			expect(result.exitCode).toBe(0);
			expect(result.stdout.trim()).toBe("id,value\n1,10\n2,21");
			expect(await vm.exists("/tmp/app.duckdb")).toBe(true);
		});

		test("fetches remote CSV data into the VFS and queries it from DuckDB", async () => {
			const server = createServer((req: IncomingMessage, res: ServerResponse) => {
				if (req.url === "/remote.csv") {
					res.writeHead(200, { "Content-Type": "text/csv" });
					res.end("city,value\nsf,3\nla,5\n");
					return;
				}

				res.writeHead(404, { "Content-Type": "text/plain" });
				res.end("not found");
			});

			await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));

			try {
				const address = server.address();
				if (!address || typeof address === "string") {
					throw new Error("failed to bind test HTTP server");
				}

				let result = await vm.exec(
					`http_get ${address.port} /remote.csv /tmp/remote.csv`,
				);
				expect(result.exitCode).toBe(0);

				result = await vm.exec(
					`duckdb -csv -c "SELECT SUM(value) AS total FROM read_csv_auto('/tmp/remote.csv');"`,
				);
				expect(result.exitCode).toBe(0);
				expect(result.stdout.trim()).toBe("total\n8");
			} finally {
				await closeServer(server);
			}
		});

		test("keeps DuckDB itself file-scoped while the network helper handles remote fetches", async () => {
			let requests = 0;
			const server = createServer((req: IncomingMessage, res: ServerResponse) => {
				requests += 1;
				if (req.url === "/remote.csv") {
					res.writeHead(200, { "Content-Type": "text/csv" });
					res.end("city,value\nsf,3\nla,5\n");
					return;
				}

				res.writeHead(404, { "Content-Type": "text/plain" });
				res.end("not found");
			});

			await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));

			try {
				const address = server.address();
				if (!address || typeof address === "string") {
					throw new Error("failed to bind test HTTP server");
				}

				const result = await vm.exec(
					`duckdb -csv -c "SELECT SUM(value) AS total FROM read_csv_auto('http://127.0.0.1:${address.port}/remote.csv');"`,
				);
				expect(result.exitCode).not.toBe(0);
				expect(requests).toBe(0);
			} finally {
				await closeServer(server);
			}
		});

		test("propagates registry package command permission tiers into the runtime", async () => {
			await vm.dispose();

			const httpGetReadOnly = {
				...httpGet,
				commands: [{ name: "http_get", permissionTier: "read-only" as const }],
			};
			vm = await AgentOs.create({ software: [coreutils, httpGetReadOnly] });

			const server = createServer((req: IncomingMessage, res: ServerResponse) => {
				res.writeHead(200, { "Content-Type": "text/plain" });
				res.end("ok");
			});

			await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));

			try {
				const address = server.address();
				if (!address || typeof address === "string") {
					throw new Error("failed to bind test HTTP server");
				}

				const result = await vm.exec(`http_get ${address.port} /blocked`);
				expect(result.exitCode).not.toBe(0);
			} finally {
				await closeServer(server);
			}
		});
	},
);
