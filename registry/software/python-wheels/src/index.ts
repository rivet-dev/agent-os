import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { PythonWheelPackage } from "@rivet-dev/agent-os-registry-types";

const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Lockfile shape produced by the wheel-build pipeline.
 * Each entry pins the exact wheel version, filename, and sha256.
 */
export interface WheelLockfile {
	/** ISO timestamp when the lockfile was generated. */
	generatedAt: string;
	/** Pyodide ABI tag (e.g., "pyodide_2025_0_wasm32"). */
	pyodideAbi: string;
	/** Python tag (e.g., "cp313"). */
	pythonTag: string;
	/** Pinned wheels in dependency order. */
	wheels: Array<{
		name: string;
		version: string;
		filename: string;
		sha256: string;
		size: number;
		/** "pure" = py3-none-any, "native" = ABI-tagged */
		kind: "pure" | "native";
	}>;
	/** Default install order for micropip (respects dependency graph). */
	installOrder: string[];
}

/**
 * The dbt + DuckDB Pyodide wheel set, intended to be loaded into a Pyodide
 * runtime via micropip from a local filesystem mount (NODEFS).
 *
 * Wheels live under `wheels/`. The lockfile (`wheels/lockfile.json`) pins
 * exact versions and sha256s. The micropip-compatible JSON index lives at
 * `wheels/index/<package>.json` (warehouse JSON shape).
 */
const pkg: PythonWheelPackage = {
	name: "python-wheels",
	description:
		"Pyodide wheels for dbt-core + dbt-duckdb + DuckDB, vendored for offline install in the agent-os Python runtime.",
	target: "pyodide",
	get wheelsDir(): string {
		return resolve(__dirname, "..", "wheels");
	},
	get indexDir(): string {
		return resolve(__dirname, "..", "wheels", "index");
	},
	get lockfilePath(): string {
		return resolve(__dirname, "..", "wheels", "lockfile.json");
	},
	listWheels(): string[] {
		const dir = this.wheelsDir;
		if (!existsSync(dir)) {
			return [];
		}
		return readdirSync(dir)
			.filter((name) => name.endsWith(".whl"))
			.sort();
	},
	readLockfile(): WheelLockfile | null {
		const path = this.lockfilePath;
		if (!existsSync(path)) {
			return null;
		}
		return JSON.parse(readFileSync(path, "utf-8")) as WheelLockfile;
	},
};

export default pkg;
