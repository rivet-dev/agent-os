import { unsupportedFunction } from "./unsupported.ts";

export const appendFileSync = () => unsupportedFunction("fs");
export const createReadStream = () => unsupportedFunction("fs");
export const createWriteStream = () => unsupportedFunction("fs");
export const existsSync = () => unsupportedFunction("fs");
export const mkdirSync = () => unsupportedFunction("fs");
export const readFileSync = () => unsupportedFunction("fs");
export const readdirSync = () => unsupportedFunction("fs");
export const statSync = () => unsupportedFunction("fs");
export const writeFileSync = () => unsupportedFunction("fs");
