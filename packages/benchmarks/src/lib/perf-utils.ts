import os from "node:os";

export interface Stats {
	mean: number;
	p50: number;
	p95: number;
	p99: number;
	min: number;
	max: number;
}

export function percentile(sorted: number[], p: number): number {
	const idx = Math.ceil((p / 100) * sorted.length) - 1;
	return sorted[Math.max(0, idx)];
}

export function round(n: number, decimals = 2): number {
	const f = 10 ** decimals;
	return Math.round(n * f) / f;
}

export function stats(samples: number[]): Stats {
	if (samples.length === 0) {
		throw new Error("stats requires at least one sample");
	}
	const sorted = [...samples].sort((a, b) => a - b);
	const mean = samples.reduce((a, b) => a + b, 0) / samples.length;
	return {
		mean: round(mean),
		p50: round(percentile(sorted, 50)),
		p95: round(percentile(sorted, 95)),
		p99: round(percentile(sorted, 99)),
		min: round(sorted[0]),
		max: round(sorted[sorted.length - 1]),
	};
}

export function getHardware() {
	const cpus = os.cpus();
	return {
		cpu: cpus[0]?.model ?? "unknown",
		cores: os.availableParallelism(),
		ram: `${round(os.totalmem() / 1024 ** 3, 1)} GB`,
		node: process.version,
		os: `${os.type()} ${os.release()}`,
		arch: os.arch(),
	};
}

export function forceGC(): void {
	if (global.gc) {
		global.gc();
	}
}

export function nowMs(start: bigint): number {
	return Number(process.hrtime.bigint() - start) / 1e6;
}

export function printTable(headers: string[], rows: (string | number)[][]): void {
	const widths = headers.map((h, i) =>
		Math.max(h.length, ...rows.map((r) => String(r[i]).length)),
	);
	const sep = widths.map((w) => "-".repeat(w)).join(" | ");
	const fmt = (row: (string | number)[]) =>
		row.map((c, i) => String(c).padStart(widths[i])).join(" | ");
	console.error("");
	console.error(fmt(headers));
	console.error(sep);
	for (const row of rows) console.error(fmt(row));
	console.error("");
}
