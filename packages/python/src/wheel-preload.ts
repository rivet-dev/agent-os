/**
 * Shared wheel-preload helper used by both `driver.ts` (standalone Pyodide
 * runtime) and `kernel-runtime.ts` (kernel-mounted runtime).
 *
 * Both files inline a `WORKER_SOURCE` template string that runs inside a
 * Node Worker thread. To avoid drift between the two copies, this module:
 *
 *   1. Defines the `WheelPreloadOptions` shape both drivers accept.
 *   2. Exports the canonical install-pathway block regex + error string.
 *   3. Provides a worker-side helper (as a string snippet) that both
 *      drivers paste into their WORKER_SOURCE so the install logic stays
 *      in lockstep.
 */

/** Options that configure offline wheel preloading for a Pyodide worker. */
export interface WheelPreloadOptions {
	/**
	 * VFS path inside Pyodide where the host wheel directory is mounted.
	 * Wheels become reachable via `emfs:<mountPath>/<filename>`.
	 * Default: `/wheels`.
	 */
	mountPath?: string;
	/**
	 * Absolute host directory containing the `.whl` files. Mounted via
	 * NODEFS. Required.
	 */
	hostDir: string;
	/**
	 * Wheel filenames (no path) installed via micropip with `deps=False`.
	 * Order is irrelevant since dependency resolution happens at import
	 * time, not install time.
	 */
	wheels: string[];
	/**
	 * Pyodide bundled packages to pre-load via `pyodide.loadPackage(...)`
	 * before our wheels are installed. Pyodide ships ~200 packages with
	 * its distribution but does not auto-import them — `micropip.install`
	 * with `deps=False` skips their resolution, so we must explicitly load
	 * each Pyodide-bundled transitive dependency our wheel set relies on.
	 * Example: ["typing-extensions", "jinja2", "pyyaml", "protobuf"].
	 */
	pyodidePackages?: string[];
	/**
	 * Allow `micropip` / `loadPackage` calls in user code (i.e. agent code)
	 * once preloading is complete. Default false: the install pathway is
	 * locked back down after preload so agents cannot install arbitrary
	 * additional wheels at runtime.
	 */
	allowRuntimeInstalls?: boolean;
	/**
	 * Optional Python source to execute after the wheels are installed
	 * but before the worker reports ready. Used to inject monkey-patches
	 * like the multiprocessing.get_context shim required by dbt-core.
	 */
	bootstrapScript?: string;
}

/**
 * Serializable wire shape for the worker-side preload payload.
 * The worker normalizes its own defaults; this is what the host posts.
 */
export interface WheelPreloadPayload {
	mountPath: string;
	hostDir: string;
	wheels: string[];
	pyodidePackages: string[];
	bootstrapScript: string;
}

export function normalizeWheelPreload(
	options: WheelPreloadOptions | undefined,
): WheelPreloadPayload | undefined {
	if (!options) {
		return undefined;
	}
	return {
		mountPath: options.mountPath ?? "/wheels",
		hostDir: options.hostDir,
		wheels: options.wheels,
		pyodidePackages: options.pyodidePackages ?? [],
		bootstrapScript: options.bootstrapScript ?? "",
	};
}

/**
 * Worker-side JavaScript snippet that both drivers inline into their
 * WORKER_SOURCE template. Runs inside `ensurePyodide` after `loadPyodide`
 * resolves, when an `init` payload contains `wheelPreload`.
 *
 * Expects:
 *   - `pyodide` — the loaded Pyodide instance in scope
 *   - `payload` — the init payload that may contain `wheelPreload`
 */
export const WORKER_WHEEL_PRELOAD_JS = String.raw`
async function applyWheelPreload(pyodide, preload) {
	if (!preload || !preload.hostDir || !Array.isArray(preload.wheels)) {
		return;
	}
	const mountPath = preload.mountPath || "/wheels";
	try {
		pyodide.FS.mkdirTree(mountPath);
	} catch (e) {
		// directory may already exist
	}
	pyodide.FS.mount(
		pyodide.FS.filesystems.NODEFS,
		{ root: preload.hostDir },
		mountPath,
	);
	if (preload.wheels.length > 0) {
		// Step 1: load Pyodide-bundled packages our wheels transitively
		// depend on. micropip.install with deps=False skips dependency
		// resolution so we must explicitly load each bundled package the
		// dbt closure expects to be present at import time.
		const bundled = Array.isArray(preload.pyodidePackages)
			? preload.pyodidePackages
			: [];
		// micropip itself ships with Pyodide and is needed to install our wheels.
		const toLoad = ["micropip", ...bundled];
		await pyodide.loadPackage(toLoad);
		const urls = preload.wheels.map((w) => "emfs:" + mountPath + "/" + w);
		const installCode =
			"import micropip\n" +
			"import json as __json\n" +
			"__urls = __json.loads(__preload_urls_json__)\n" +
			"await micropip.install(__urls, deps=False)";
		pyodide.globals.set("__preload_urls_json__", JSON.stringify(urls));
		try {
			await pyodide.runPythonAsync(installCode);
		} finally {
			try {
				pyodide.globals.delete("__preload_urls_json__");
			} catch (e) {}
		}
	}
	if (preload.bootstrapScript && preload.bootstrapScript.length > 0) {
		await pyodide.runPythonAsync(preload.bootstrapScript);
	}
}
`;

/**
 * The regex used by `checkPackageInstallAllowed` to reject code that tries
 * to install Python packages from inside the sandbox via `run`/`exec`.
 */
export const PACKAGE_INSTALL_PATHWAYS_PATTERN =
	/\b(micropip|loadPackagesFromImports|loadPackage)\b/;

export const PYTHON_PACKAGE_UNSUPPORTED_ERROR =
	"ERR_PYTHON_PACKAGE_INSTALL_UNSUPPORTED: Python package installation is not supported in this runtime";

/**
 * Throws if the supplied code attempts to install packages and the runtime
 * has not been configured with `allowRuntimeInstalls: true`.
 */
export function checkPackageInstallAllowed(
	code: string,
	options: { allowRuntimeInstalls?: boolean } | undefined,
): void {
	if (options?.allowRuntimeInstalls) {
		return;
	}
	if (!PACKAGE_INSTALL_PATHWAYS_PATTERN.test(code)) {
		return;
	}
	throw new Error(PYTHON_PACKAGE_UNSUPPORTED_ERROR);
}
