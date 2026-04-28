/**
 * Static dev server for the browser playground.
 *
 * SharedArrayBuffer (required by the Agent OS web worker) needs COOP/COEP
 * headers. Once COEP is "require-corp", every subresource must be same-origin
 * or carry Cross-Origin-Resource-Policy. Vendor assets (Monaco and
 * TypeScript) are installed as npm packages and symlinked into vendor/ by
 * `scripts/setup-vendor.ts`, so everything is served from the local filesystem.
 */
import { createReadStream } from "node:fs";
import { realpath, stat } from "node:fs/promises";
import {
	createServer,
	type OutgoingHttpHeaders,
	type Server,
	type ServerResponse,
} from "node:http";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_PORT = Number(process.env.PORT ?? "4173");
const playgroundDir = resolve(fileURLToPath(new URL("..", import.meta.url)));

const mimeTypes = new Map<string, string>([
	[".css", "text/css; charset=utf-8"],
	[".data", "application/octet-stream"],
	[".html", "text/html; charset=utf-8"],
	[".js", "text/javascript; charset=utf-8"],
	[".json", "application/json; charset=utf-8"],
	[".mjs", "text/javascript; charset=utf-8"],
	[".svg", "image/svg+xml"],
	[".wasm", "application/wasm"],
	[".zip", "application/zip"],
]);

function isWithinPlayground(candidatePath: string): boolean {
	return (
		candidatePath === playgroundDir ||
		candidatePath.startsWith(`${playgroundDir}${sep}`)
	);
}

function getFilePath(urlPath: string): string | null {
	const pathname = decodeURIComponent(urlPath.split("?")[0] ?? "/");
	const relativePath = pathname === "/" ? "/frontend/index.html" : pathname;

	const safePath = normalize(relativePath).replace(/^(\.\.[/\\])+/, "");
	const absolutePath = resolve(playgroundDir, `.${safePath}`);
	if (!isWithinPlayground(absolutePath)) {
		return null;
	}
	return absolutePath;
}

function getRedirectLocation(urlPath: string): string | null {
	const [pathname, search = ""] = urlPath.split("?");
	if (pathname === "/" || pathname.endsWith("/")) {
		return null;
	}
	return `${pathname}/${search ? `?${search}` : ""}`;
}

const COEP_HEADERS = {
	"Cross-Origin-Embedder-Policy": "require-corp",
	"Cross-Origin-Opener-Policy": "same-origin",
} as const;

function writeHeaders(
	response: ServerResponse,
	status: number,
	extras: OutgoingHttpHeaders = {},
): void {
	response.writeHead(status, {
		"Cache-Control": "no-store",
		...COEP_HEADERS,
		...extras,
	});
}

export function createBrowserPlaygroundServer(): Server {
	return createServer(async (_request, response) => {
		const requestUrl = _request.url ?? "/";

		const filePath = getFilePath(requestUrl);
		if (!filePath) {
			writeHeaders(response, 403);
			response.end("Forbidden");
			return;
		}

		/* Resolve symlinks (vendor/ entries point into node_modules) */
		let resolvedPath: string;
		try {
			resolvedPath = await realpath(filePath);
		} catch {
			writeHeaders(response, 404);
			response.end("Not found");
			return;
		}
		if (!isWithinPlayground(resolvedPath)) {
			writeHeaders(response, 403);
			response.end("Forbidden: resolved path escapes playground directory");
			return;
		}

		let finalPath = resolvedPath;
		try {
			const fileStat = await stat(resolvedPath);
			if (fileStat.isDirectory()) {
				const redirectLocation = getRedirectLocation(requestUrl);
				if (redirectLocation) {
					writeHeaders(response, 308, { Location: redirectLocation });
					response.end();
					return;
				}
				finalPath = join(resolvedPath, "index.html");
			}
		} catch {
			writeHeaders(response, 404);
			response.end("Not found");
			return;
		}

		try {
			finalPath = await realpath(finalPath);
		} catch {
			writeHeaders(response, 404);
			response.end("Not found");
			return;
		}
		if (!isWithinPlayground(finalPath)) {
			writeHeaders(response, 403);
			response.end("Forbidden: resolved path escapes playground directory");
			return;
		}

		try {
			const fileStat = await stat(finalPath);
			if (!fileStat.isFile()) {
				writeHeaders(response, 404);
				response.end("Not found");
				return;
			}

			const mimeType =
				mimeTypes.get(extname(finalPath)) ?? "application/octet-stream";
			writeHeaders(response, 200, {
				"Content-Length": String(fileStat.size),
				"Content-Type": mimeType,
			});
			createReadStream(finalPath).pipe(response);
		} catch {
			writeHeaders(response, 500);
			response.end("Failed to read file");
		}
	});
}

export function startBrowserPlaygroundServer(port = DEFAULT_PORT): Server {
	const server = createBrowserPlaygroundServer();
	server.listen(port, () => {
		console.log(`Browser playground: http://localhost:${port}/`);
	});
	return server;
}

if (
	process.argv[1] &&
	resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
	startBrowserPlaygroundServer();
}
