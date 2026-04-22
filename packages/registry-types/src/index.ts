/**
 * Permission tier for WASM command execution.
 * Mirrors the PermissionTier from @rivet-dev/agent-os-posix.
 *
 * - full: spawn processes, network I/O, file read/write
 * - read-write: file read/write, no network or process spawning
 * - read-only: file read-only, no writes, no spawn, no network
 * - isolated: restricted to cwd subtree reads only
 */
export type PermissionTier = "full" | "read-write" | "read-only" | "isolated";

/**
 * Descriptor for a single command within a WASM command package.
 */
export interface WasmCommandEntry {
	/** Command name as invoked (e.g., "grep", "egrep"). */
	name: string;
	/** Default permission tier for this command. */
	permissionTier: PermissionTier;
	/** If set, this command is an alias for another command in the same package. */
	aliasOf?: string;
}

/**
 * Descriptor for a WASM command package.
 * Each @rivet-dev/agent-os-* package exports a default value satisfying this type.
 */
export interface WasmCommandPackage {
	/** Package name without scope (e.g., "coreutils", "grep"). */
	name: string;
	/** Apt/Debian equivalent package name. */
	aptName: string;
	/** Human-readable description. */
	description: string;
	/** Build source: "rust" or "c". */
	source: "rust" | "c";
	/** Commands provided by this package. */
	commands: WasmCommandEntry[];
	/** Absolute path to the directory containing WASM command binaries. */
	readonly commandDir: string;
}

/**
 * Descriptor for a meta-package that aggregates other WASM command packages.
 */
export interface WasmMetaPackage {
	/** Package name without scope. */
	name: string;
	/** Human-readable description. */
	description: string;
	/** Package names (without scope) included in this meta-package. */
	includes: string[];
}

/**
 * Descriptor for a Python wheel bundle package.
 *
 * Unlike WASM command packages, these vendor pre-built `.whl` files
 * (Pyodide-compatible) for offline `micropip` install inside the
 * Python runtime. The wheels are mounted into the Pyodide VFS via
 * NODEFS and resolved through a warehouse-JSON-shaped index.
 */
export interface PythonWheelPackage {
	/** Package name without scope (e.g., "python-wheels"). */
	name: string;
	/** Human-readable description. */
	description: string;
	/** Always "pyodide" today; reserved for future multi-runtime targets. */
	target: "pyodide";
	/** Absolute host path to the directory holding all `.whl` files. */
	readonly wheelsDir: string;
	/** Absolute host path to the warehouse JSON index directory. */
	readonly indexDir: string;
	/** Absolute host path to the version+sha256 lockfile. */
	readonly lockfilePath: string;
	/** Returns wheel filenames present on disk (sorted). */
	listWheels(): string[];
	/** Returns the parsed lockfile, or null if not yet built. */
	readLockfile(): unknown;
}
