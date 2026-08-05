import { unsupportedFunction } from "./unsupported.ts";

export function createRequire(): never {
	return unsupportedFunction("module");
}
