import { build } from "esbuild";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import stdLibBrowser from "node-stdlib-browser";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const workspaceRoot = path.resolve(packageRoot, "..", "..");
const outputPath = path.join(
	workspaceRoot,
	"packages",
	"runtime-browser",
	"src",
	"generated",
	"node-polyfills.ts",
);

const moduleEntries = {
	assert: "assert",
	events: "events",
	querystring: "querystring",
	stream: "readable-stream",
	stringDecoder: "string_decoder",
	url: "url",
};

const aliases = {};
for (const [name, modulePath] of Object.entries(stdLibBrowser)) {
	if (typeof modulePath !== "string") {
		continue;
	}
	aliases[name] = modulePath;
	aliases[`node:${name}`] = modulePath;
}
aliases["readable-stream"] = path.dirname(
	fileURLToPath(import.meta.resolve("readable-stream/package.json")),
);

const generated = {};
for (const [exportName, request] of Object.entries(moduleEntries)) {
	const augmentation =
		exportName === "url"
			? [
					'const path = require("path");',
					'const punycode = require("punycode");',
					"resolved.URL = globalThis.URL;",
					"resolved.URLSearchParams = globalThis.URLSearchParams;",
					"resolved.domainToASCII = punycode.toASCII;",
					"resolved.domainToUnicode = punycode.toUnicode;",
					"resolved.fileURLToPath = (input) => {",
					"  const url = input instanceof globalThis.URL ? input : new globalThis.URL(input);",
					'  if (url.protocol !== "file:") throw new TypeError("The URL must be of scheme file");',
					'  if (url.host && url.host !== "localhost") throw new TypeError("File URL host must be empty or localhost");',
					'  if (/%2f/i.test(url.pathname)) throw new TypeError("File URL path must not include encoded / characters");',
					"  return decodeURIComponent(url.pathname);",
					"};",
					"resolved.pathToFileURL = (input) => {",
					'  const absolute = path.posix.resolve(String(input || "/"));',
					'  const encoded = encodeURI(absolute).replaceAll("#", "%23").replaceAll("?", "%3F");',
					'  return new globalThis.URL(`file://${encoded.startsWith("/") ? encoded : `/${encoded}`}`);',
					"};",
				].join("\n")
			: exportName === "stream"
				? [
						"resolved.Readable.toWeb = (stream) => new ReadableStream({",
						'  start(controller) { stream.on("data", (chunk) => controller.enqueue(chunk)); stream.once("end", () => controller.close()); stream.once("error", (error) => controller.error(error)); stream.resume?.(); },',
						"  cancel(reason) { stream.destroy?.(reason instanceof Error ? reason : undefined); },",
						"});",
						"resolved.Writable.toWeb = (stream) => new WritableStream({",
						'  start(controller) { stream.on("error", (error) => controller.error(error)); },',
						"  write(chunk) { return new Promise((resolve, reject) => stream.write(chunk, undefined, (error) => error ? reject(error) : resolve())); },",
						"  close() { return new Promise((resolve, reject) => stream.end((error) => error ? reject(error) : resolve())); },",
						"  abort(reason) { stream.destroy?.(reason instanceof Error ? reason : new Error(String(reason))); },",
						"});",
						"resolved.Duplex.toWeb = (stream) => ({",
						"  readable: resolved.Readable.toWeb(stream),",
						"  writable: resolved.Writable.toWeb(stream),",
						"});",
					].join("\n")
				: "";
	const result = await build({
		stdin: {
			contents: [
				`const imported = require(${JSON.stringify(request)});`,
				"const resolved = imported.default ?? imported;",
				augmentation,
				"module.exports = resolved;",
			].join("\n"),
			resolveDir: workspaceRoot,
			loader: "js",
		},
		bundle: true,
		write: false,
		format: "cjs",
		platform: "browser",
		alias: aliases,
		banner: {
			js: [
				"var process = globalThis.process || {",
				"  env: {},",
				"  cwd: () => '/',",
				"  nextTick: (fn, ...args) => queueMicrotask(() => fn(...args)),",
				"};",
			].join("\n"),
		},
		target: "es2020",
	});
	let bundle = result.outputFiles[0].text;
	bundle +=
		"\nif (module.exports && module.exports.default == null) module.exports.default = module.exports;\n";
	generated[exportName] = bundle;
}

await mkdir(path.dirname(outputPath), { recursive: true });
await writeFile(
	outputPath,
	[
		"// @generated - run node packages/build-tools/scripts/build-browser-node-polyfills.mjs",
		...Object.entries(generated).map(
			([name, code]) =>
				`export const BROWSER_${name.replace(/([A-Z])/g, "_$1").toUpperCase()}_POLYFILL_CODE = ${JSON.stringify(code)};`,
		),
		"",
	].join("\n"),
);

console.log("Built packages/runtime-browser/src/generated/node-polyfills.ts");
