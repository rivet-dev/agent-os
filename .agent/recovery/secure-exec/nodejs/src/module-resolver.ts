import {
	normalizeBuiltinSpecifier,
	getPathDir,
} from "./builtin-modules.js";
import { resolveModule } from "./package-bundler.js";
import { parseJsonWithLimit } from "./isolate-bootstrap.js";
import { sourceHasModuleSyntax } from "./module-source.js";
import type { DriverDeps } from "./isolate-bootstrap.js";

type ResolverDeps = Pick<
	DriverDeps,
	"filesystem" | "packageTypeCache" | "moduleFormatCache" | "isolateJsonPayloadLimitBytes" | "resolutionCache"
>;

export async function getNearestPackageType(
	deps: ResolverDeps,
	filePath: string,
): Promise<"module" | "commonjs" | null> {
	let currentDir = getPathDir(filePath);
	const visitedDirs: string[] = [];
	while (true) {
		if (deps.packageTypeCache.has(currentDir)) {
			return deps.packageTypeCache.get(currentDir) ?? null;
		}
		visitedDirs.push(currentDir);

		const packageJsonPath =
			currentDir === "/" ? "/package.json" : `${currentDir}/package.json`;

		let hasPackageJson = false;
		try {
			hasPackageJson = await deps.filesystem.exists(packageJsonPath);
		} catch (error) {
			const err = error as NodeJS.ErrnoException;
			if (err?.code !== "EACCES" && err?.code !== "EPERM") {
				throw err;
			}
		}

		if (hasPackageJson) {
			try {
				const packageJsonText =
					await deps.filesystem.readTextFile(packageJsonPath);
				const pkgJson = parseJsonWithLimit<{ type?: unknown }>(
					`package.json ${packageJsonPath}`,
					packageJsonText,
					deps.isolateJsonPayloadLimitBytes,
				);
				const packageType =
					pkgJson.type === "module" || pkgJson.type === "commonjs"
						? pkgJson.type
						: null;
				for (const dir of visitedDirs) {
					deps.packageTypeCache.set(dir, packageType);
				}
				return packageType;
			} catch {
				for (const dir of visitedDirs) {
					deps.packageTypeCache.set(dir, null);
				}
				return null;
			}
		}

		if (currentDir === "/") {
			for (const dir of visitedDirs) {
				deps.packageTypeCache.set(dir, null);
			}
			return null;
		}
		currentDir = getPathDir(currentDir);
	}
}

export async function getModuleFormat(
	deps: ResolverDeps,
	filePath: string,
	sourceCode?: string,
): Promise<"esm" | "cjs" | "json"> {
	const cached = deps.moduleFormatCache.get(filePath);
	if (cached) {
		return cached;
	}

	let format: "esm" | "cjs" | "json";
	if (filePath.endsWith(".mjs")) {
		format = "esm";
	} else if (filePath.endsWith(".cjs")) {
		format = "cjs";
	} else if (filePath.endsWith(".json")) {
		format = "json";
	} else if (filePath.endsWith(".js")) {
		const packageType = await getNearestPackageType(deps, filePath);
		if (packageType === "module") {
			format = "esm";
		} else if (packageType === "commonjs") {
			format = "cjs";
		} else if (sourceCode && await sourceHasModuleSyntax(sourceCode, filePath)) {
			// Some package managers/projected filesystems omit package.json.
			// Fall back to syntax-based detection for plain .js modules.
			format = "esm";
		} else {
			format = "cjs";
		}
	} else {
		format = "cjs";
	}

	deps.moduleFormatCache.set(filePath, format);
	return format;
}

export async function shouldRunAsESM(
	deps: ResolverDeps,
	code: string,
	filePath?: string,
): Promise<boolean> {
	// Keep heuristic mode for string-only snippets without file metadata.
	if (!filePath) {
		return sourceHasModuleSyntax(code);
	}
	return (await getModuleFormat(deps, filePath)) === "esm";
}

export async function resolveReferrerDirectory(
	deps: Pick<DriverDeps, "filesystem">,
	referrerPath: string,
): Promise<string> {
	if (referrerPath === "" || referrerPath === "/") {
		return "/";
	}

	// Dynamic import hooks may pass either a module file path or a module
	// directory path. Prefer filesystem metadata so we do not strip one level
	// when the referrer is already a directory.
	if (deps.filesystem) {
		try {
			const statInfo = await deps.filesystem.stat(referrerPath);
			if (statInfo.isDirectory) {
				return referrerPath;
			}
		} catch {
			// Fall back to string-based path handling below.
		}
	}

	if (referrerPath.endsWith("/")) {
		return referrerPath.slice(0, -1) || "/";
	}

	const lastSlash = referrerPath.lastIndexOf("/");
	if (lastSlash <= 0) {
		return "/";
	}
	return referrerPath.slice(0, lastSlash);
}

export async function resolveESMPath(
	deps: Pick<DriverDeps, "filesystem" | "resolutionCache">,
	specifier: string,
	referrerPath: string,
): Promise<string | null> {
	// Handle built-ins and bridged modules first.
	const builtinSpecifier = normalizeBuiltinSpecifier(specifier);
	if (builtinSpecifier) {
		return builtinSpecifier;
	}

	const referrerDir = await resolveReferrerDirectory(deps, referrerPath);

	// Preserve direct path imports before falling back to node_modules
	// resolution so missing relative modules report the resolved sandbox path.
	if (specifier.startsWith("/")) {
		return specifier;
	}
	if (specifier.startsWith("./") || specifier.startsWith("../")) {
		const parts = referrerDir.split("/").filter(Boolean);
		for (const part of specifier.split("/")) {
			if (part === "..") {
				parts.pop();
				continue;
			}
			if (part !== ".") {
				parts.push(part);
			}
		}
		return `/${parts.join("/")}`;
	}

	return resolveModule(specifier, referrerDir, deps.filesystem, "import", deps.resolutionCache);
}
