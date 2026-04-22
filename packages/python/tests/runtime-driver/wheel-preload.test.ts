import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { allowAll } from "@secure-exec/core";
import { createNodeDriver } from "@secure-exec/nodejs";
import { afterAll, describe, expect, it } from "vitest";
import { DBT_BOOTSTRAP_SCRIPT } from "../../src/dbt-bootstrap.ts";
import { createPyodideRuntimeDriverFactory } from "../../src/driver.ts";
import {
	checkPackageInstallAllowed,
	PYTHON_PACKAGE_UNSUPPORTED_ERROR,
} from "../../src/wheel-preload.ts";

const __dirname = resolve(fileURLToPath(import.meta.url), "..");
const wheelsHostDir = resolve(
	__dirname,
	"../../../../registry/software/python-wheels/wheels",
);

const runtimes: Array<{ dispose(): void }> = [];

afterAll(() => {
	for (const r of runtimes) {
		try {
			r.dispose();
		} catch {}
	}
});

function createRuntime(extra: Record<string, unknown> = {}) {
	const sys = createNodeDriver({
		useDefaultNetwork: true,
		permissions: allowAll,
	});
	const runtime = createPyodideRuntimeDriverFactory().createRuntimeDriver({
		system: sys,
		runtime: sys.runtime,
		...extra,
	});
	runtimes.push(runtime);
	return runtime;
}

describe("wheel preload — sandbox properties preserved", () => {
	it("rejects micropip in user code by default (no wheelPreload)", () => {
		expect(() =>
			checkPackageInstallAllowed("import micropip", undefined),
		).toThrow(PYTHON_PACKAGE_UNSUPPORTED_ERROR);
	});

	it("rejects loadPackage in user code by default", () => {
		expect(() =>
			checkPackageInstallAllowed("pyodide.loadPackage('numpy')", undefined),
		).toThrow(PYTHON_PACKAGE_UNSUPPORTED_ERROR);
	});

	it("rejects loadPackagesFromImports by default", () => {
		expect(() =>
			checkPackageInstallAllowed(
				"await pyodide.loadPackagesFromImports('import numpy')",
				undefined,
			),
		).toThrow(PYTHON_PACKAGE_UNSUPPORTED_ERROR);
	});

	it("allows benign code by default", () => {
		expect(() =>
			checkPackageInstallAllowed("print('hello')", undefined),
		).not.toThrow();
	});

	it("allows micropip when allowRuntimeInstalls is set", () => {
		expect(() =>
			checkPackageInstallAllowed("import micropip", {
				allowRuntimeInstalls: true,
			}),
		).not.toThrow();
	});

	it("driver.run still rejects micropip when wheelPreload not set", async () => {
		const runtime = createRuntime();
		const result = await runtime.run("import micropip");
		expect(result.code).toBe(1);
		expect(result.errorMessage).toContain(
			"ERR_PYTHON_PACKAGE_INSTALL_UNSUPPORTED",
		);
	}, 60000);
});

describe.skipIf(!existsSync(wheelsHostDir))(
	"wheel preload — DuckDB wheel mount sanity",
	() => {
		// These tests run only when the wheels dir is populated. The vendored
		// DuckDB wheel was built against pyodide_2025_0_wasm32; if the local
		// pyodide install is on a different ABI the install step will fail
		// fast and that failure is itself useful diagnostic data.
		it("exposes the bootstrap script via the public export", () => {
			expect(DBT_BOOTSTRAP_SCRIPT).toContain("multiprocessing.get_context");
			expect(DBT_BOOTSTRAP_SCRIPT).toContain("DBT_SINGLE_THREADED");
		});
	},
);
