// Nightly: requires the heavyweight DuckDB build.
import { existsSync, statSync } from "node:fs";
import {
	createServer,
	type IncomingMessage,
	type Server,
	type ServerResponse,
} from "node:http";
import { dirname, join } from "node:path";
import coreutils from "@agentos-software/coreutils";
import curl from "@agentos-software/curl";
import duckdb from "@agentos-software/duckdb";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../dist/index.js";

const DUCKDB_PACKAGE = duckdb;
const CURL_PACKAGE = curl;
// C-sysroot packages are the ONE sanctioned skip: they need the patched wasi C
// sysroot most checkouts don't build.
const duckdbPackageSkipReason = cSysrootPackageSkipReason(
	{ pkg: DUCKDB_PACKAGE, name: "duckdb" },
	{ pkg: CURL_PACKAGE, name: "curl" },
);

interface RegistryPackageRef {
	packagePath: string;
}

function isNonPlaceholderPackage(path: string): boolean {
	try {
		return existsSync(path) && statSync(path).size > 16;
	} catch {
		return false;
	}
}

function packageBuilt(pkg: RegistryPackageRef, command: string): boolean {
	const packageDir = pkg.packagePath.endsWith(".aospkg")
		? join(dirname(pkg.packagePath), "package")
		: pkg.packagePath;
	return (
		isNonPlaceholderPackage(pkg.packagePath) &&
		existsSync(join(packageDir, "agentos-package.json")) &&
		existsSync(join(packageDir, "bin", command))
	);
}

function cSysrootPackageSkipReason(
	...packages: Array<{ pkg: RegistryPackageRef; name: string }>
): string | false {
	const unbuilt = packages.filter(({ pkg, name }) => !packageBuilt(pkg, name));
	if (unbuilt.length === 0) return false;
	return (
		`C-sysroot software packages not built: ${unbuilt.map(({ name }) => name).join(", ")} ` +
		"(needs the patched wasi C sysroot: `make -C toolchain/c`, then `pnpm --dir software/<package> build`)"
	);
}

function closeServer(server: Server) {
	return new Promise<void>((resolve, reject) => {
		server.close((err) => {
			if (err) reject(err);
			else resolve();
		});
	});
}

describe("duckdb registry package", () => {
	if (duckdbPackageSkipReason) {
		test("requires registry DuckDB command artifacts", () => {
			expect(duckdbPackageSkipReason).toBe(false);
		});
		return;
	}

	let vm: AgentOs;

	async function recreateVm(options?: {
		software?: Parameters<typeof AgentOs.create>[0]["software"];
		loopbackExemptPorts?: number[];
	}) {
		if (vm) {
			await vm.dispose();
		}
		vm = await AgentOs.create({
			software: options?.software ?? [coreutils, CURL_PACKAGE, DUCKDB_PACKAGE],
			...(options?.loopbackExemptPorts
				? { loopbackExemptPorts: options.loopbackExemptPorts }
				: {}),
		});
		await vm.exec("mkdir -p /tmp");
	}

	beforeEach(async () => {
		await recreateVm();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("runs file-backed DuckDB DML through the registry package path", async () => {
		let result = await vm.exec(
			`duckdb -csv /tmp/app.duckdb -c "CREATE TABLE items(id INTEGER, value INTEGER); INSERT INTO items VALUES (1, 10), (2, 20); UPDATE items SET value = value + 1 WHERE id = 2;"`,
		);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(
			await vm.exists("/workspace"),
			`DuckDB removed the VM working directory; root entries: ${JSON.stringify(
				await vm.readdir("/"),
			)}`,
		).toBe(true);

		result = await vm.exec(
			`duckdb -csv /tmp/app.duckdb -c "SELECT id, value FROM items ORDER BY id;"`,
		);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout.trim()).toBe("id,value\n1,10\n2,21");
	}, 90_000);

	test("exports analytical SQL results to CSV and reads them back", async () => {
		let result = await vm.exec(
			[
				`duckdb /tmp/analytics.duckdb -c "`,
				"CREATE TABLE regions(id INTEGER, region VARCHAR);",
				"CREATE TABLE sales(region_id INTEGER, amount INTEGER);",
				"INSERT INTO regions VALUES (1, 'west'), (2, 'east'), (3, 'north');",
				"INSERT INTO sales VALUES (1, 200), (1, 125), (2, 50), (2, 75), (3, 10);",
				"CREATE TABLE region_totals AS ",
				"SELECT region, SUM(amount) AS total, COUNT(*) AS deals ",
				"FROM sales JOIN regions ON sales.region_id = regions.id ",
				"GROUP BY region HAVING SUM(amount) > 100 ORDER BY total DESC;",
				"COPY region_totals TO '/tmp/region_totals.csv' (HEADER, DELIMITER ',');",
				`"`,
			].join(""),
		);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(
			await vm.exists("/workspace"),
			`DuckDB removed the VM working directory; root entries: ${JSON.stringify(
				await vm.readdir("/"),
			)}`,
		).toBe(true);

		result = await vm.exec(
			`duckdb -csv /tmp/analytics.duckdb -c "SELECT region, total, deals FROM read_csv_auto('/tmp/region_totals.csv') ORDER BY total DESC;"`,
		);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout.trim()).toBe(
			"region,total,deals\nwest,325,2\neast,125,2",
		);
	}, 90_000);

	test("keeps DuckDB itself file-scoped for HTTP URLs", async () => {
		let requests = 0;
		const server = createServer(
			(_req: IncomingMessage, res: ServerResponse) => {
				requests += 1;
				res.writeHead(200, { "Content-Type": "text/csv" });
				res.end("city,value\nsf,3\nla,5\n");
			},
		);

		await new Promise<void>((resolve) =>
			server.listen(0, "127.0.0.1", resolve),
		);

		try {
			const address = server.address();
			if (!address || typeof address === "string") {
				throw new Error("failed to bind test HTTP server");
			}
			await recreateVm({ loopbackExemptPorts: [address.port] });

			const result = await vm.exec(
				`duckdb -csv -c "SELECT SUM(value) AS total FROM read_csv_auto('http://127.0.0.1:${address.port}/remote.csv');"`,
			);
			expect(result.exitCode).not.toBe(0);
			expect(requests).toBe(0);
		} finally {
			await closeServer(server);
		}
	}, 90_000);
});
