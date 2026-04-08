import { unsupportedFunction } from "./unsupported.ts";

export const format = (...args: unknown[]): string =>
	args.map(String).join(" ");
export const inspect = (value: unknown): string => String(value);
export const promisify = () => unsupportedFunction("util");
