/**
 * Mount-table readdir benchmark.
 *
 * Isolates mount-count effects by timing BenchVm.readdir over native host_dir
 * mounts while varying unrelated mount count and child mount count.
 */

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createBenchSidecar, createBenchVm, type BenchVm } from "../lib/vm.js";
import type { HostDirectoryMount, SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";

interface MountReaddirCaseResult {
	scenario: string;
	mountCount: number;
	entryCount: number;
	iterations: number;
	warmup: number;
	returnedCount: number;
	readdir: ReturnType<typeof stats>;
	msPerMount: number | null;
	rawMs: number[];
}

function parseArgs(): {
	iterations: number;
	warmup: number;
	mountCounts: number[];
	entryCount: number;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const iterations = Number(value("iterations") ?? 20);
	const warmup = Number(value("warmup") ?? 3);
	const mountCounts = (value("mount-counts") ?? "0,10,100")
		.split(",")
		.map((n) => Number(n.trim()))
		.filter((n) => Number.isFinite(n) && n >= 0);
	const entryCount = Number(value("entry-count") ?? 32);
	if (
		iterations < 1 ||
		warmup < 0 ||
		mountCounts.length === 0 ||
		entryCount < 0
	) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --mount-counts=0,10,100 --entry-count>=0",
		);
	}
	return { iterations, warmup, mountCounts, entryCount };
}

function nowMs(start: number): number {
	return performance.now() - start;
}

function makeHostDir(root: string, name: string, entryCount = 0): string {
	const dir = join(root, name);
	mkdirSync(dir, { recursive: true });
	for (let i = 0; i < entryCount; i++) {
		writeFileSync(join(dir, `entry-${String(i).padStart(4, "0")}.txt`), "x");
	}
	return dir;
}

function hostDirMount(path: string, hostPath: string): HostDirectoryMount {
	return {
		guestPath: path,
		hostPath,
		readOnly: true,
	};
}

async function createVm(
	sidecar: SidecarProcess,
	mounts: HostDirectoryMount[],
): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
		mounts,
	});
}

async function timeReaddir(
	vm: BenchVm,
	path: string,
	iterations: number,
	warmup: number,
	expectedCount: number,
): Promise<{ samples: number[]; returnedCount: number }> {
	const samples: number[] = [];
	let returnedCount = 0;
	for (let i = 0; i < warmup + iterations; i++) {
		const start = performance.now();
	const entries = await vm.readDir(path);
		const elapsed = nowMs(start);
		returnedCount = entries.length;
		if (returnedCount !== expectedCount) {
			throw new Error(
				`readdir ${path} returned ${returnedCount}, expected ${expectedCount}`,
			);
		}
		if (i >= warmup) {
			samples.push(elapsed);
		}
	}
	return { samples, returnedCount };
}

async function runUnrelatedMountCase(
	sidecar: SidecarProcess,
	root: string,
	mountCount: number,
	entryCount: number,
	iterations: number,
	warmup: number,
): Promise<MountReaddirCaseResult> {
	const target = makeHostDir(root, `target-${mountCount}`, entryCount);
	const mounts: HostDirectoryMount[] = [hostDirMount("/target", target)];
	for (let i = 0; i < mountCount; i++) {
		mounts.push(
			hostDirMount(
				`/unrelated/mount-${String(i).padStart(4, "0")}`,
				makeHostDir(root, `unrelated-${mountCount}-${i}`),
			),
		);
	}
	const vm = await createVm(sidecar, mounts);
	try {
		const { samples, returnedCount } = await timeReaddir(
			vm,
			"/target",
			iterations,
			warmup,
			entryCount,
		);
		const readdir = stats(samples);
		return {
			scenario: "target-with-unrelated-mounts",
			mountCount,
			entryCount,
			iterations,
			warmup,
			returnedCount,
			readdir,
			msPerMount: mountCount === 0 ? null : round(readdir.p50 / mountCount),
			rawMs: samples,
		};
	} finally {
		await vm.dispose();
	}
}

async function runChildMountCase(
	sidecar: SidecarProcess,
	root: string,
	mountCount: number,
	iterations: number,
	warmup: number,
): Promise<MountReaddirCaseResult> {
	const mounts: HostDirectoryMount[] = [
		hostDirMount("/mnt", makeHostDir(root, `parent-${mountCount}`)),
	];
	for (let i = 0; i < mountCount; i++) {
		mounts.push(
			hostDirMount(
				`/mnt/child-${String(i).padStart(4, "0")}`,
				makeHostDir(root, `child-${mountCount}-${i}`),
			),
		);
	}
	const vm = await createVm(sidecar, mounts);
	try {
		const { samples, returnedCount } = await timeReaddir(
			vm,
			"/mnt",
			iterations,
			warmup,
			mountCount,
		);
		const readdir = stats(samples);
		return {
			scenario: "parent-with-child-mounts",
			mountCount,
			entryCount: 0,
			iterations,
			warmup,
			returnedCount,
			readdir,
			msPerMount: mountCount === 0 ? null : round(readdir.p50 / mountCount),
			rawMs: samples,
		};
	} finally {
		await vm.dispose();
	}
}

async function main(): Promise<void> {
	const { iterations, warmup, mountCounts, entryCount } = parseArgs();
	const hardware = getHardware();
	console.error("=== Mount Readdir Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), mount counts: ${mountCounts.join(",")}, entry count: ${entryCount}`,
	);

	const root = mkdtempSync(join(tmpdir(), "agentos-mount-readdir-"));
	const sidecar = await createBenchSidecar();
	try {
		const cases: MountReaddirCaseResult[] = [];
		for (const mountCount of mountCounts) {
			cases.push(
				await runUnrelatedMountCase(
					sidecar,
					root,
					mountCount,
					entryCount,
					iterations,
					warmup,
				),
			);
			cases.push(
				await runChildMountCase(sidecar, root, mountCount, iterations, warmup),
			);
		}

		printTable(
			[
				"scenario",
				"mounts",
				"entries",
				"returned",
				"p50",
				"p95",
				"ms/mount",
			],
			cases.map((result) => [
				result.scenario,
				result.mountCount,
				result.entryCount,
				result.returnedCount,
				`${result.readdir.p50}ms`,
				`${result.readdir.p95}ms`,
				result.msPerMount === null ? "n/a" : `${result.msPerMount}ms`,
			]),
		);

		console.log(
			JSON.stringify(
				{
					benchmark: "mount-readdir",
					hardware,
					iterations,
					warmup,
					mountCounts,
					entryCount,
					cases,
				},
				null,
				2,
			),
		);
	} finally {
		await sidecar.dispose();
		rmSync(root, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
