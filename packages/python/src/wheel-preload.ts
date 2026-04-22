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
	/**
	 * Additional NODEFS mounts to install inside Pyodide's filesystem so
	 * Python's `open()`/`os.walk()` see the same files agent-os exposes
	 * via its kernel-VFS host-dir mounts.
	 *
	 * Without this, files written from the host via `aos.writeFile(...)` to
	 * a host-dir-backed mount path live on the host filesystem but are
	 * INVISIBLE to Pyodide (which has its own MEMFS for everything outside
	 * /wheels). Mounting the same host directories under their VM paths
	 * here means a single physical file is visible to both pathways:
	 *
	 *   - `aos.writeFile("/home/user/tmp/foo")` -> host `/tmp/foo`
	 *   - Python `open("/home/user/tmp/foo")` -> host `/tmp/foo` (via NODEFS)
	 *
	 * Each entry mounts {hostDir} into Pyodide at {mountPath}. The mount
	 * path is created with `mkdirTree` if missing. Read-only flag mirrors
	 * the underlying host-dir backend's read-only state.
	 */
	extraNodefsMounts?: ExtraNodefsMount[];
}

/** Single host-dir <-> Pyodide path bridge. */
export interface ExtraNodefsMount {
	/** Absolute host directory containing the files. */
	hostDir: string;
	/** Pyodide-visible POSIX path to mount the host dir at. */
	mountPath: string;
	/** If true, NODEFS mount is treated as read-only by the worker. */
	readOnly?: boolean;
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
	extraNodefsMounts: ExtraNodefsMount[];
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
		extraNodefsMounts: options.extraNodefsMounts ?? [],
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
	// Bridge agent-os' kernel-VFS host-dir mounts into Pyodide so Python's
	// open()/os.walk() see the same files aos.writeFile() wrote. Without
	// this, files written via the actor SDK live on the host filesystem
	// but stay invisible to Pyodide's MEMFS — dbt would never see its
	// own project files.
	const extraMounts = Array.isArray(preload.extraNodefsMounts)
		? preload.extraNodefsMounts
		: [];
	for (const m of extraMounts) {
		if (!m || typeof m.hostDir !== "string" || typeof m.mountPath !== "string") {
			continue;
		}
		try {
			pyodide.FS.mkdirTree(m.mountPath);
		} catch (e) {
			// directory may already exist
		}
		try {
			pyodide.FS.mount(
				pyodide.FS.filesystems.NODEFS,
				{ root: m.hostDir },
				m.mountPath,
			);
		} catch (mountErr) {
			// Surface as a warning on stderr; don't block boot. A failed
			// extra mount means certain paths won't be bridged but the
			// runtime is still usable for everything else.
			try {
				console.error(
					"agent-os python: NODEFS mount failed for " +
						m.mountPath +
						" -> " +
						m.hostDir +
						": " +
						(mountErr && mountErr.message ? mountErr.message : String(mountErr)),
				);
			} catch (_logErr) {}
		}
	}
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
