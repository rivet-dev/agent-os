import { createReadStream, existsSync } from "node:fs";
import { createServer } from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const packageDir = path.resolve(__dirname, "..");
const port = Number.parseInt(process.env.PORT ?? "43173", 10);

const routes = new Map([
	[
		"/agentos-worker.js",
		path.join(packageDir, ".cache/browser-tests/agentos-worker.js"),
	],
	[
		"/frontend/converged-harness.html",
		path.join(
			packageDir,
			"tests/browser/fixtures/frontend/converged-harness.html",
		),
	],
	[
		"/agentos-converged-harness.js",
		path.join(
			packageDir,
			".cache/browser-tests/agentos-converged-harness.js",
		),
	],
	[
		"/frontend/converged-runtime-harness.html",
		path.join(
			packageDir,
			"tests/browser/fixtures/frontend/converged-runtime-harness.html",
		),
	],
	[
		"/agentos-converged-runtime-harness.js",
		path.join(
			packageDir,
			".cache/browser-tests/agentos-converged-runtime-harness.js",
		),
	],
	[
		"/frontend/converged-conformance-harness.html",
		path.join(
			packageDir,
			"tests/browser/fixtures/frontend/converged-conformance-harness.html",
		),
	],
	[
		"/agentos-converged-conformance-harness.js",
		path.join(
			packageDir,
			".cache/browser-tests/agentos-converged-conformance-harness.js",
		),
	],
]);

function contentType(filePath) {
	if (filePath.endsWith(".html")) return "text/html; charset=utf-8";
	if (filePath.endsWith(".js")) return "text/javascript; charset=utf-8";
	if (filePath.endsWith(".wasm")) return "application/wasm";
	return "application/octet-stream";
}

function resolveRequestPath(urlPath) {
	if (routes.has(urlPath)) {
		return routes.get(urlPath);
	}
	if (urlPath.startsWith("/dist/")) {
		const relative = urlPath.slice("/dist/".length);
		if (relative.includes("..")) {
			return null;
		}
		return path.join(packageDir, "dist", relative);
	}
	// The web-target wasm package (js glue + .wasm) served as static files.
	if (urlPath.startsWith("/sidecar-wasm-web/")) {
		const relative = urlPath.slice("/sidecar-wasm-web/".length);
		if (relative.includes("..")) {
			return null;
		}
		return path.join(packageDir, ".cache/browser-tests/sidecar-wasm-web", relative);
	}
	return null;
}

const server = createServer((request, response) => {
	const url = new URL(request.url ?? "/", `http://${request.headers.host}`);
	const filePath = resolveRequestPath(url.pathname);
	if (!filePath || !existsSync(filePath)) {
		response.writeHead(404, {
			"content-type": "text/plain; charset=utf-8",
		});
		response.end("not found");
		return;
	}

	response.writeHead(200, {
		"content-type": contentType(filePath),
		"cross-origin-opener-policy": "same-origin",
		"cross-origin-embedder-policy": "require-corp",
		"cross-origin-resource-policy": "same-origin",
	});
	createReadStream(filePath).pipe(response);
});

server.listen(port, "127.0.0.1", () => {
	console.log(`agentos browser test server listening on http://127.0.0.1:${port}`);
});
