/**
 * Detect if code uses ESM syntax.
 */
export function isESM(code: string, filePath?: string): boolean {
	if (filePath?.endsWith(".mjs")) return true;
	if (filePath?.endsWith(".cjs")) return false;

	const hasImport =
		/^\s*import\s*(?:[\w{},*\s]+\s*from\s*)?['"][^'"]+['"]/m.test(code) ||
		/^\s*import\s*\{[^}]*\}\s*from\s*['"][^'"]+['"]/m.test(code);
	const hasExport =
		/^\s*export\s+(?:default|const|let|var|function|class|{)/m.test(code) ||
		/^\s*export\s*\{/m.test(code);

	return hasImport || hasExport;
}

/**
 * Transform dynamic import() calls to __dynamicImport() calls.
 */
export function transformDynamicImport(code: string): string {
	return code.replace(/(?<![a-zA-Z_$])import\s*\(/g, "__dynamicImport(");
}

/**
 * Extract static import specifiers from transformed code.
 */
export function extractDynamicImportSpecifiers(code: string): string[] {
	const regex = /__dynamicImport\s*\(\s*['"]([^'"]+)['"]\s*\)/g;
	const specifiers = new Set<string>();
	for (const match of code.matchAll(regex)) {
		specifiers.add(match[1]);
	}
	return Array.from(specifiers);
}

/**
 * Convert CJS module to ESM-compatible wrapper.
 */
/**
 * Wrap CommonJS code in an ESM-compatible module that exports `module.exports`
 * as the default export plus any statically-detectable named exports.
 */
export function wrapCJSForESM(code: string): string {
	const modulePath = "/<cjs-module>.cjs";
	return wrapCJSForESMWithModulePath(code, modulePath);
}

function getModuleDir(path: string): string {
	const normalized = path.replace(/\\/g, "/");
	const lastSlash = normalized.lastIndexOf("/");
	if (lastSlash <= 0) {
		return "/";
	}
	return normalized.slice(0, lastSlash);
}

export function wrapCJSForESMWithModulePath(
	code: string,
	modulePath: string,
): string {
	const moduleDir = getModuleDir(modulePath);
	const namedExports = extractCjsNamedExports(code)
		.filter((name) => name !== "default" && name !== "__esModule")
		.map((name) => {
			const localName = `__cjs_named_${name}`;
			return `const ${localName} = __cjs?.${name};\nexport { ${localName} as ${name} };`;
		})
		.join("\n");

	return `
	    const __filename = ${JSON.stringify(modulePath)};
	    const __dirname = ${JSON.stringify(moduleDir)};
	    const require = (name) => globalThis._requireFrom(name, __dirname);
	    const module = { exports: {} };
	    const exports = module.exports;
	    ${code}
	    const __cjs = module.exports;
	    export default __cjs;
	    export const __cjsModule = true;
	    ${namedExports}
	  `;
}

/**
 * Scan CJS code for `module.exports.X =`, `exports.X =`, and
 * `Object.defineProperty(exports, 'X', ...)` patterns to discover named exports
 * that can be re-exported from the ESM wrapper.
 */
function extractCjsNamedExports(code: string): string[] {
	const names = new Set<string>();
	const add = (name: string) => {
		if (!/^[A-Za-z_$][\w$]*$/.test(name)) {
			return;
		}
		names.add(name);
	};

	for (const match of code.matchAll(/\bmodule\.exports\.([A-Za-z_$][\w$]*)\s*=/g)) {
		add(match[1]);
	}
	for (const match of code.matchAll(/\bexports\.([A-Za-z_$][\w$]*)\s*=/g)) {
		add(match[1]);
	}
	for (const match of code.matchAll(/\bObject\.defineProperty\(\s*(?:module\.)?exports\s*,\s*["']([^"']+)["']/g)) {
		add(match[1]);
	}

	return Array.from(names).sort();
}

export { extractCjsNamedExports };
