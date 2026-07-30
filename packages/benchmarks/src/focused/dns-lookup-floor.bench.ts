/**
 * Focused DNS lookup floor benchmark.
 *
 * Splits the broad dns/* rows into warm single lookup, sequential repeated
 * lookup, concurrent same-host lookup, and fresh-process lookup shapes.
 */

import { execFileSync } from "node:child_process";
import dns from "node:dns/promises";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

type DnsWorkload = "single" | "sequential" | "concurrent" | "coldProcess";

interface DnsRow {
	id: string;
	workload: DnsWorkload;
	hostname: string;
	lookupCount: number;
	concurrency: number;
	description: string;
}

interface DnsRunResult {
	totalMs: number[];
	firstLookupMs?: number[];
	restLookupMs?: number[];
}

interface DnsRowResult {
	id: string;
	workload: DnsWorkload;
	hostname: string;
	lookupCount: number;
	concurrency: number;
	description: string;
	iterations: number;
	warmup: number;
	host: ReturnType<typeof stats>;
	guest: ReturnType<typeof stats>;
	guestVsHostRatio: number;
	guestMsPerLookup: number;
	hostMsPerLookup: number;
	firstLookup?: {
		host: ReturnType<typeof stats>;
		guest: ReturnType<typeof stats>;
	};
	restLookup?: {
		host: ReturnType<typeof stats>;
		guest: ReturnType<typeof stats>;
	};
	raw: {
		hostMs: number[];
		guestMs: number[];
		hostFirstLookupMs?: number[];
		guestFirstLookupMs?: number[];
		hostRestLookupMs?: number[];
		guestRestLookupMs?: number[];
	};
}

function defaultRows(): DnsRow[] {
	return [
		{
			id: "single_localhost",
			workload: "single",
			hostname: "localhost",
			lookupCount: 1,
			concurrency: 1,
			description: "one warm same-process dns.lookup('localhost')",
		},
		{
			id: "sequential_same_2",
			workload: "sequential",
			hostname: "localhost",
			lookupCount: 2,
			concurrency: 1,
			description: "two sequential same-process dns.lookup('localhost') calls",
		},
		{
			id: "sequential_same_8",
			workload: "sequential",
			hostname: "localhost",
			lookupCount: 8,
			concurrency: 1,
			description: "eight sequential same-process dns.lookup('localhost') calls",
		},
		{
			id: "sequential_same_32",
			workload: "sequential",
			hostname: "localhost",
			lookupCount: 32,
			concurrency: 1,
			description: "thirty-two sequential same-process dns.lookup('localhost') calls",
		},
		{
			id: "concurrent_same_4",
			workload: "concurrent",
			hostname: "localhost",
			lookupCount: 4,
			concurrency: 4,
			description: "four concurrent same-host dns.lookup('localhost') calls",
		},
		{
			id: "concurrent_same_16",
			workload: "concurrent",
			hostname: "localhost",
			lookupCount: 16,
			concurrency: 16,
			description: "sixteen concurrent same-host dns.lookup('localhost') calls",
		},
		{
			id: "cold_process_single",
			workload: "coldProcess",
			hostname: "localhost",
			lookupCount: 1,
			concurrency: 1,
			description: "fresh Node process for one dns.lookup('localhost')",
		},
	];
}

function parseArgs(): { iterations: number; warmup: number; rows: DnsRow[] } {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const iterations = Number(value("iterations") ?? 10);
	const warmup = Number(value("warmup") ?? 2);
	const selected = value("rows") ?? value("cases");
	const rows = selected
		? selected
				.split(",")
				.map((id) => id.trim())
				.filter(Boolean)
				.map(rowForId)
		: defaultRows();
	if (iterations < 1 || warmup < 0 || rows.length === 0) {
		throw new Error("invalid args; expected --iterations>=1 --warmup>=0 --rows=<row ids>");
	}
	return { iterations, warmup, rows };
}

function rowForId(id: string): DnsRow {
	const row = defaultRows().find((candidate) => candidate.id === id);
	if (row) return row;
	const sequential = /^sequential_same_(\d+)$/.exec(id);
	if (sequential) {
		const lookupCount = Number(sequential[1]);
		return {
			id,
			workload: "sequential",
			hostname: "localhost",
			lookupCount,
			concurrency: 1,
			description: `${lookupCount} sequential same-process dns.lookup('localhost') calls`,
		};
	}
	const concurrent = /^concurrent_same_(\d+)$/.exec(id);
	if (concurrent) {
		const lookupCount = Number(concurrent[1]);
		return {
			id,
			workload: "concurrent",
			hostname: "localhost",
			lookupCount,
			concurrency: lookupCount,
			description: `${lookupCount} concurrent same-host dns.lookup('localhost') calls`,
		};
	}
	throw new Error(`unknown dns-lookup-floor row: ${id}`);
}

function nowMs(start: number): number {
	return performance.now() - start;
}

async function runDnsRow(row: DnsRow): Promise<{ firstLookupMs?: number; restLookupMs?: number }> {
	if (row.workload === "single") {
		await dns.lookup(row.hostname);
		return {};
	}
	if (row.workload === "sequential") {
		const firstStart = performance.now();
		await dns.lookup(row.hostname);
		const firstLookupMs = performance.now() - firstStart;
		let restLookupMs = 0;
		for (let i = 1; i < row.lookupCount; i++) {
			const start = performance.now();
			await dns.lookup(row.hostname);
			restLookupMs += performance.now() - start;
		}
		return { firstLookupMs, restLookupMs };
	}
	if (row.workload === "concurrent") {
		await Promise.all(Array.from({ length: row.lookupCount }, () => dns.lookup(row.hostname)));
		return {};
	}
	throw new Error(`runDnsRow does not support ${row.workload}`);
}

async function runHost(row: DnsRow, iterations: number, warmup: number): Promise<DnsRunResult> {
	if (row.workload === "coldProcess") return runHostColdProcess(row, iterations, warmup);
	const totalMs: number[] = [];
	const firstLookupMs: number[] = [];
	const restLookupMs: number[] = [];
	for (let i = 0; i < warmup + iterations; i++) {
		const start = performance.now();
		const detail = await runDnsRow(row);
		const elapsed = nowMs(start);
		if (i >= warmup) {
			totalMs.push(elapsed);
			if (detail.firstLookupMs !== undefined) firstLookupMs.push(detail.firstLookupMs);
			if (detail.restLookupMs !== undefined) restLookupMs.push(detail.restLookupMs);
		}
	}
	return {
		totalMs,
		firstLookupMs: firstLookupMs.length > 0 ? firstLookupMs : undefined,
		restLookupMs: restLookupMs.length > 0 ? restLookupMs : undefined,
	};
}

function coldProcessSource(hostname: string): string {
	return `
const dns = await import("node:dns/promises");
const result = await dns.lookup(${JSON.stringify(hostname)});
if (!result.address) throw new Error("no address");
`;
}

function runHostColdProcess(row: DnsRow, iterations: number, warmup: number): DnsRunResult {
	const dir = mkdtempSync(join(tmpdir(), "agentos-dns-floor-host-"));
	const file = join(dir, "lookup.mjs");
	try {
		writeFileSync(file, coldProcessSource(row.hostname));
		const totalMs: number[] = [];
		for (let i = 0; i < warmup + iterations; i++) {
			const start = performance.now();
			execFileSync("node", [file], { stdio: "ignore" });
			const elapsed = nowMs(start);
			if (i >= warmup) totalMs.push(elapsed);
		}
		return { totalMs };
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

function guestScript(row: DnsRow, iterations: number, warmup: number): string {
	return `
import dns from "node:dns/promises";
const row = ${JSON.stringify(row)};
const iterations = ${iterations};
const warmup = ${warmup};
const totalMs = [];
const firstLookupMs = [];
const restLookupMs = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;
async function runDnsRow() {
  if (row.workload === "single") {
    await dns.lookup(row.hostname);
    return {};
  }
  if (row.workload === "sequential") {
    const firstStart = now();
    await dns.lookup(row.hostname);
    const firstLookupMs = now() - firstStart;
    let restLookupMs = 0;
    for (let i = 1; i < row.lookupCount; i++) {
      const start = now();
      await dns.lookup(row.hostname);
      restLookupMs += now() - start;
    }
    return { firstLookupMs, restLookupMs };
  }
  if (row.workload === "concurrent") {
    await Promise.all(Array.from({ length: row.lookupCount }, () => dns.lookup(row.hostname)));
    return {};
  }
  throw new Error("unsupported guest workload " + row.workload);
}
for (let i = 0; i < warmup + iterations; i++) {
  const start = now();
  const detail = await runDnsRow();
  const elapsed = now() - start;
  if (i >= warmup) {
    totalMs.push(elapsed);
    if (detail.firstLookupMs !== undefined) firstLookupMs.push(detail.firstLookupMs);
    if (detail.restLookupMs !== undefined) restLookupMs.push(detail.restLookupMs);
  }
}
process.stdout.write(JSON.stringify({
  totalMs,
  firstLookupMs: firstLookupMs.length > 0 ? firstLookupMs : undefined,
  restLookupMs: restLookupMs.length > 0 ? restLookupMs : undefined,
}));
`;
}

async function runGuest(
	vm: BenchVm,
	row: DnsRow,
	iterations: number,
	warmup: number,
): Promise<DnsRunResult> {
	if (row.workload === "coldProcess") return runGuestColdProcess(vm, row, iterations, warmup);
	const path = `/tmp/dns-lookup-floor-${row.id}-${Date.now()}.mjs`;
	await vm.writeFile(path, guestScript(row, iterations, warmup));
	let stdout = "";
	let stderr = "";
	const proc = vm.spawn("node", [path], {
		onStdout: (data) => {
			stdout += Buffer.from(data).toString("utf8");
		},
		onStderr: (data) => {
			stderr += Buffer.from(data).toString("utf8");
		},
	});
	const code = await vm.waitProcess(proc.pid);
	if (code !== 0) throw new Error(`guest dns row ${row.id} exited ${code}\n${stderr}`);
	return JSON.parse(stdout) as DnsRunResult;
}

async function runGuestColdProcess(
	vm: BenchVm,
	row: DnsRow,
	iterations: number,
	warmup: number,
): Promise<DnsRunResult> {
	const path = `/tmp/dns-lookup-floor-cold-${Date.now()}.mjs`;
	await vm.writeFile(path, coldProcessSource(row.hostname));
	const totalMs: number[] = [];
	for (let i = 0; i < warmup + iterations; i++) {
		const start = performance.now();
		const proc = vm.spawn("node", [path]);
		const code = await vm.waitProcess(proc.pid);
		const elapsed = nowMs(start);
		if (code !== 0) throw new Error(`guest cold dns row ${row.id} exited ${code}`);
		if (i >= warmup) totalMs.push(elapsed);
	}
	return { totalMs };
}

function maybeStats(samples: number[] | undefined): ReturnType<typeof stats> | undefined {
	return samples && samples.length > 0 ? stats(samples) : undefined;
}

async function runCase(
	vm: BenchVm,
	row: DnsRow,
	iterations: number,
	warmup: number,
): Promise<DnsRowResult> {
	const hostRun = await runHost(row, iterations, warmup);
	const guestRun = await runGuest(vm, row, iterations, warmup);
	const host = stats(hostRun.totalMs);
	const guest = stats(guestRun.totalMs);
	const hostFirst = maybeStats(hostRun.firstLookupMs);
	const guestFirst = maybeStats(guestRun.firstLookupMs);
	const hostRest = maybeStats(hostRun.restLookupMs);
	const guestRest = maybeStats(guestRun.restLookupMs);
	return {
		...row,
		iterations,
		warmup,
		host,
		guest,
		guestVsHostRatio: host.p50 === 0 ? 0 : round(guest.p50 / host.p50),
		guestMsPerLookup: round(guest.p50 / row.lookupCount),
		hostMsPerLookup: round(host.p50 / row.lookupCount),
		...(hostFirst && guestFirst ? { firstLookup: { host: hostFirst, guest: guestFirst } } : {}),
		...(hostRest && guestRest ? { restLookup: { host: hostRest, guest: guestRest } } : {}),
		raw: {
			hostMs: hostRun.totalMs,
			guestMs: guestRun.totalMs,
			hostFirstLookupMs: hostRun.firstLookupMs,
			guestFirstLookupMs: guestRun.firstLookupMs,
			hostRestLookupMs: hostRun.restLookupMs,
			guestRestLookupMs: guestRun.restLookupMs,
		},
	};
}

async function main(): Promise<void> {
	const { iterations, warmup, rows } = parseArgs();
	console.error("=== DNS Lookup Floor Benchmark ===");
	const hardware = getHardware();
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), rows: ${rows.map((row) => row.id).join(",")}`,
	);
	const sidecar = await createBenchSidecar();
	const vm = await createBenchVm({
		sidecar,
	});
	try {
		const results: DnsRowResult[] = [];
		for (const row of rows) {
			const result = await runCase(vm, row, iterations, warmup);
			results.push(result);
			console.error(
				`  row=${row.id}: host.p50=${result.host.p50}ms guest.p50=${result.guest.p50}ms ratio=${result.guestVsHostRatio}x guest.ms/lookup=${result.guestMsPerLookup}`,
			);
		}
		printTable(
			[
				"row",
				"lookups",
				"concurrency",
				"host p50",
				"guest p50",
				"guest/host",
				"guest ms/lookup",
				"first guest",
				"rest guest",
			],
			results.map((result) => [
				result.id,
				String(result.lookupCount),
				String(result.concurrency),
				`${result.host.p50}ms`,
				`${result.guest.p50}ms`,
				`${result.guestVsHostRatio}x`,
				`${result.guestMsPerLookup}ms`,
				result.firstLookup ? `${result.firstLookup.guest.p50}ms` : "n/a",
				result.restLookup ? `${result.restLookup.guest.p50}ms` : "n/a",
			]),
		);
		console.log(
			JSON.stringify(
				{
					benchmark: "dns-lookup-floor",
					generatedAt: new Date().toISOString(),
					hardware,
					iterations,
					warmup,
					rows: results,
				},
				null,
				2,
			),
		);
	} finally {
		await vm.dispose();
		await sidecar.dispose();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
