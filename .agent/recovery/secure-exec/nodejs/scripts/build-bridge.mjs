import * as esbuild from "esbuild";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Bridge source lives in this package (secure-exec-nodejs).
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(__dirname, "..");

const bridgeSource = path.join(packageRoot, "src", "bridge", "index.ts");
const bridgeOutput = path.join(packageRoot, "dist", "bridge.js");

const result = esbuild.buildSync({
	entryPoints: [bridgeSource],
	bundle: true,
	format: "iife",
	globalName: "bridge",
	outfile: bridgeOutput,
});

if (result.errors.length > 0) {
	throw new Error(`Failed to build bridge.js: ${result.errors[0].text}`);
}

console.log(`Built bridge IIFE at ${bridgeOutput}`);
