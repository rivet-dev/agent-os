export function createUnsupportedBrowserModuleError(moduleName: string): Error {
	return new Error(
		`${moduleName} is unavailable in the browser playground bundle`,
	);
}

export function unsupportedFunction<T = never>(moduleName: string): T {
	throw createUnsupportedBrowserModuleError(moduleName);
}
