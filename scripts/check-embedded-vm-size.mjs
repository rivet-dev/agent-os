import { statSync } from "node:fs";

const binary = process.argv[2];
if (!binary) {
	throw new Error("usage: node scripts/check-embedded-vm-size.mjs <binary>");
}

const maxBytes = 1024 * 1024;
const bytes = statSync(binary).size;
if (bytes > maxBytes) {
	throw new Error(
		`embedded VM binary is ${bytes.toLocaleString()} bytes; maximum is ${maxBytes.toLocaleString()} bytes`,
	);
}

console.log(
	`embedded VM size: OK (${bytes.toLocaleString()} bytes <= ${maxBytes.toLocaleString()} bytes)`,
);
