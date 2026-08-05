import { unsupportedFunction } from "./unsupported.ts";

export const basename = () => unsupportedFunction("path");
export const delimiter = "/";
export const dirname = () => unsupportedFunction("path");
export const extname = () => unsupportedFunction("path");
export const format = () => unsupportedFunction("path");
export const isAbsolute = () => unsupportedFunction("path");
export const join = () => unsupportedFunction("path");
export const normalize = () => unsupportedFunction("path");
export const parse = () => unsupportedFunction("path");
export const relative = () => unsupportedFunction("path");
export const resolve = () => unsupportedFunction("path");
export const sep = "/";

export const posix = {
	basename,
	delimiter,
	dirname,
	extname,
	format,
	isAbsolute,
	join,
	normalize,
	parse,
	relative,
	resolve,
	sep,
};

export default {
	basename,
	delimiter,
	dirname,
	extname,
	format,
	isAbsolute,
	join,
	normalize,
	parse,
	posix,
	relative,
	resolve,
	sep,
};
