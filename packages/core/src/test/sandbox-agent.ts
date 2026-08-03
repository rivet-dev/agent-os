import { once } from "node:events";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { mkdtemp, mkdir, readFile, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import type { SandboxAgent } from "sandbox-agent";

type ProcessState = "running" | "exited";
type ProcessStream = "stdout" | "stderr";

interface LoggedEntry {
	data: string;
	encoding: "base64";
	sequence: number;
	stream: ProcessStream;
	timestampMs: number;
}

interface ManagedProcess {
	args: string[];
	child: ChildProcessWithoutNullStreams;
	command: string;
	createdAtMs: number;
	cwd: string | null;
	exitCode: number | null;
	exitedAtMs: number | null;
	id: string;
	interactive: boolean;
	logs: LoggedEntry[];
	pid: number | null;
	sequence: number;
	status: ProcessState;
	tty: boolean;
}

export interface MockSandboxAgentOptions {
	token?: string;
}

export interface MockSandboxAgentHandle {
	baseUrl: string;
	client: SandboxAgent;
	path(...segments: string[]): string;
	rootDir: string;
	stop(): Promise<void>;
}

function json(response: ServerResponse, status: number, value: unknown): void {
	const body = Buffer.from(JSON.stringify(value));
	response.writeHead(status, {
		"content-length": String(body.length),
		"content-type": "application/json",
	});
	response.end(body);
}

function problem(response: ServerResponse, status: number, detail: string): void {
	json(response, status, {
		type: "about:blank",
		title: status === 404 ? "Not Found" : "Bad Request",
		status,
		detail,
	});
}

async function readBody(request: IncomingMessage): Promise<Buffer> {
	const chunks: Buffer[] = [];
	for await (const chunk of request) {
		chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
	}
	return Buffer.concat(chunks);
}

function decodePath(rootDir: string, rawPath: string | null): string {
	const candidate = rawPath && rawPath.length > 0 ? rawPath : rootDir;
	const direct = resolve(candidate.startsWith("/") ? candidate : resolve(rootDir, candidate));
	const directRel = relative(rootDir, direct);
	if (!(directRel.startsWith("..") || directRel === "..")) {
		return direct;
	}

	const mapped = resolve(rootDir, candidate.replace(/^\/+/, ""));
	const mappedRel = relative(rootDir, mapped);
	if (mappedRel.startsWith("..") || mappedRel === "..") {
		throw new Error(`Path escapes mock sandbox root: ${candidate}`);
	}
	return mapped;
}

function mapProcessPath(rootDir: string, value: string): string {
	const direct = resolve(value);
	const directRel = relative(rootDir, direct);
	if (!(directRel.startsWith("..") || directRel === "..")) {
		return direct;
	}
	if (!value.startsWith("/")) {
		return value;
	}
	return decodePath(rootDir, value);
}

function processInfo(proc: ManagedProcess) {
	return {
		id: proc.id,
		command: proc.command,
		args: proc.args,
		status: proc.status,
		pid: proc.pid,
		exitCode: proc.exitCode,
		cwd: proc.cwd,
		createdAtMs: proc.createdAtMs,
		exitedAtMs: proc.exitedAtMs,
		interactive: proc.interactive,
		tty: proc.tty,
		owner: "user" as const,
	};
}

function appendLog(proc: ManagedProcess, stream: ProcessStream, chunk: Buffer): void {
	proc.sequence += 1;
	proc.logs.push({
		data: chunk.toString("base64"),
		encoding: "base64",
		sequence: proc.sequence,
		stream,
		timestampMs: Date.now(),
	});
}

async function waitForProcessExit(proc: ManagedProcess, timeoutMs = 2_000): Promise<void> {
	if (proc.status === "exited") {
		return;
	}

	await Promise.race([
		once(proc.child, "close").then(() => undefined),
		new Promise<void>((resolveTimeout) => {
			setTimeout(resolveTimeout, timeoutMs);
		}),
	]);
}

async function runCommand(request: {
	rootDir: string;
	args?: string[];
	command: string;
	cwd?: string | null;
	env?: Record<string, string>;
	timeoutMs?: number | null;
}) {
	const startedAt = Date.now();
	return await new Promise<{
		durationMs: number;
		exitCode: number | null;
		stderr: string;
		stderrTruncated: boolean;
		stdout: string;
		stdoutTruncated: boolean;
		timedOut: boolean;
	}>((resolveRun) => {
		let stdout = "";
		let stderr = "";
		let settled = false;
		let timedOut = false;
		const child = spawn(
			request.command,
			request.args?.map((value) => mapProcessPath(request.rootDir, value)) ?? [],
			{
			cwd: request.cwd ? mapProcessPath(request.rootDir, request.cwd) : undefined,
			env: request.env ? { ...process.env, ...request.env } : process.env,
			stdio: ["ignore", "pipe", "pipe"],
			},
		);

		const finish = (value: {
			durationMs: number;
			exitCode: number | null;
			stderr: string;
			stderrTruncated: boolean;
			stdout: string;
			stdoutTruncated: boolean;
			timedOut: boolean;
		}) => {
			if (settled) {
				return;
			}
			settled = true;
			resolveRun(value);
		};

		const timeout =
			request.timeoutMs && request.timeoutMs > 0
				? setTimeout(() => {
						timedOut = true;
						child.kill("SIGKILL");
					}, request.timeoutMs)
				: null;

		child.stdout.on("data", (chunk: Buffer) => {
			stdout += chunk.toString("utf8");
		});
		child.stderr.on("data", (chunk: Buffer) => {
			stderr += chunk.toString("utf8");
		});
		child.on("error", (error) => {
			if (timeout) {
				clearTimeout(timeout);
			}
			finish({
				durationMs: Date.now() - startedAt,
				exitCode: 127,
				stderr: error.message,
				stderrTruncated: false,
				stdout: "",
				stdoutTruncated: false,
				timedOut,
			});
		});
		child.on("close", (code) => {
			if (timeout) {
				clearTimeout(timeout);
			}
			finish({
				durationMs: Date.now() - startedAt,
				exitCode: code,
				stderr,
				stderrTruncated: false,
				stdout,
				stdoutTruncated: false,
				timedOut,
			});
		});
	});
}

export async function startMockSandboxAgent(
	options: MockSandboxAgentOptions = {},
): Promise<MockSandboxAgentHandle> {
	const rootDir = await mkdtemp(resolve(tmpdir(), "agentos-sandbox-agent-"));
	const processes = new Map<string, ManagedProcess>();

	const server = createServer(async (request, response) => {
		try {
			if (
				options.token &&
				request.headers.authorization !== `Bearer ${options.token}`
			) {
				problem(response, 401, "Invalid sandbox token");
				return;
			}
			const url = new URL(request.url ?? "/", "http://127.0.0.1");
			const method = request.method ?? "GET";

			if (method === "GET" && url.pathname === "/") {
				json(response, 200, { ok: true });
				return;
			}

			if (method === "GET" && url.pathname === "/v1/health") {
				json(response, 200, { status: "ok" });
				return;
			}

			if (method === "GET" && url.pathname === "/v1/fs/entries") {
				const target = decodePath(rootDir, url.searchParams.get("path"));
				const entries = await readdir(target, { withFileTypes: true });
				const payload = await Promise.all(
					entries.map(async (entry) => {
						const entryPath = resolve(target, entry.name);
						const metadata = await stat(entryPath);
						return {
							name: entry.name,
							path: entryPath,
							entryType: entry.isDirectory() ? "directory" : "file",
							size: metadata.size,
							modified: null,
						};
					}),
				);
				payload.sort((left, right) => left.name.localeCompare(right.name));
				json(response, 200, payload);
				return;
			}

			if (method === "GET" && url.pathname === "/v1/fs/file") {
				const target = decodePath(rootDir, url.searchParams.get("path"));
				const bytes = await readFile(target);
				response.writeHead(200, {
					"content-length": String(bytes.length),
					"content-type": "application/octet-stream",
				});
				response.end(bytes);
				return;
			}

			if (method === "PUT" && url.pathname === "/v1/fs/file") {
				const target = decodePath(rootDir, url.searchParams.get("path"));
				await mkdir(dirname(target), { recursive: true });
				const body = await readBody(request);
				await writeFile(target, body);
				json(response, 200, {
					path: target,
					bytesWritten: body.length,
				});
				return;
			}

			if (method === "DELETE" && url.pathname === "/v1/fs/entry") {
				const target = decodePath(rootDir, url.searchParams.get("path"));
				await rm(target, {
					force: true,
					recursive: url.searchParams.get("recursive") === "true",
				});
				json(response, 200, { path: target });
				return;
			}

			if (method === "POST" && url.pathname === "/v1/fs/mkdir") {
				const target = decodePath(rootDir, url.searchParams.get("path"));
				await mkdir(target, { recursive: true });
				json(response, 200, { path: target });
				return;
			}

			if (method === "POST" && url.pathname === "/v1/fs/move") {
				const body = JSON.parse((await readBody(request)).toString("utf8")) as {
					from: string;
					to: string;
				};
				const from = decodePath(rootDir, body.from);
				const to = decodePath(rootDir, body.to);
				await mkdir(dirname(to), { recursive: true });
				await rename(from, to);
				json(response, 200, { from, to });
				return;
			}

			if (method === "GET" && url.pathname === "/v1/fs/stat") {
				const target = decodePath(rootDir, url.searchParams.get("path"));
				const metadata = await stat(target);
				json(response, 200, {
					path: target,
					entryType: metadata.isDirectory() ? "directory" : "file",
					size: metadata.size,
					modified: null,
				});
				return;
			}

			if (method === "POST" && url.pathname === "/v1/processes/run") {
				const requestBody = JSON.parse((await readBody(request)).toString("utf8")) as {
					args?: string[];
					command: string;
					cwd?: string | null;
					env?: Record<string, string>;
					timeoutMs?: number | null;
				};
				json(
					response,
					200,
					await runCommand({
						...requestBody,
						rootDir,
						cwd: requestBody.cwd ?? undefined,
					}),
				);
				return;
			}

			if (method === "POST" && url.pathname === "/v1/processes") {
				const requestBody = JSON.parse((await readBody(request)).toString("utf8")) as {
					args?: string[];
					command: string;
					cwd?: string | null;
					env?: Record<string, string>;
					interactive?: boolean;
					tty?: boolean;
				};
				const child = spawn(
					requestBody.command,
					requestBody.args?.map((value) => mapProcessPath(rootDir, value)) ?? [],
					{
						cwd: requestBody.cwd
							? mapProcessPath(rootDir, requestBody.cwd)
							: undefined,
						env: requestBody.env
							? { ...process.env, ...requestBody.env }
							: process.env,
						stdio: ["pipe", "pipe", "pipe"],
					},
				);
				const proc: ManagedProcess = {
					id: randomUUID(),
					command: requestBody.command,
					args: requestBody.args ?? [],
					child,
					createdAtMs: Date.now(),
					cwd: requestBody.cwd ?? null,
					exitCode: null,
					exitedAtMs: null,
					interactive: requestBody.interactive === true,
					logs: [],
					pid: child.pid ?? null,
					sequence: 0,
					status: "running",
					tty: requestBody.tty === true,
				};
				processes.set(proc.id, proc);
				child.stdout.on("data", (chunk: Buffer) => appendLog(proc, "stdout", chunk));
				child.stderr.on("data", (chunk: Buffer) => appendLog(proc, "stderr", chunk));
				child.on("close", (code) => {
					proc.status = "exited";
					proc.exitCode = code;
					proc.exitedAtMs = Date.now();
				});
				child.on("error", (error) => {
					appendLog(proc, "stderr", Buffer.from(error.message, "utf8"));
					proc.status = "exited";
					proc.exitCode = 127;
					proc.exitedAtMs = Date.now();
				});
				json(response, 200, processInfo(proc));
				return;
			}

			if (method === "GET" && url.pathname === "/v1/processes") {
				json(response, 200, {
					processes: Array.from(processes.values()).map(processInfo),
				});
				return;
			}

			const processMatch = url.pathname.match(/^\/v1\/processes\/([^/]+)(?:\/(stop|kill|logs|input))?$/);
			if (processMatch) {
				const [, rawId, action] = processMatch;
				const proc = processes.get(decodeURIComponent(rawId));
				if (!proc) {
					problem(response, 404, `Unknown process: ${rawId}`);
					return;
				}

				if (method === "POST" && action === "stop") {
					if (proc.status === "running") {
						proc.child.kill("SIGTERM");
						await waitForProcessExit(proc);
					}
					json(response, 200, processInfo(proc));
					return;
				}

				if (method === "POST" && action === "kill") {
					if (proc.status === "running") {
						proc.child.kill("SIGKILL");
						await waitForProcessExit(proc);
					}
					json(response, 200, processInfo(proc));
					return;
				}

				if (method === "GET" && action === "logs") {
					const tail = Number(url.searchParams.get("tail") ?? "0");
					const stream = (url.searchParams.get("stream") ?? "combined") as
						| "combined"
						| ProcessStream;
					let entries = proc.logs;
					if (stream !== "combined") {
						entries = entries.filter((entry) => entry.stream === stream);
					}
					if (Number.isFinite(tail) && tail > 0) {
						entries = entries.slice(-tail);
					}
					json(response, 200, {
						processId: proc.id,
						stream,
						entries,
					});
					return;
				}

				if (method === "POST" && action === "input") {
					const body = JSON.parse((await readBody(request)).toString("utf8")) as {
						data: string;
						encoding?: string;
					};
					const bytes =
						body.encoding === "base64"
							? Buffer.from(body.data, "base64")
							: Buffer.from(body.data, "utf8");
					proc.child.stdin.write(bytes);
					json(response, 200, { bytesWritten: bytes.length });
					return;
				}
			}

			problem(response, 404, `Unhandled mock sandbox-agent route: ${method} ${url.pathname}`);
		} catch (error) {
			problem(
				response,
				400,
				error instanceof Error ? error.message : String(error),
			);
		}
	});

	server.listen(0, "127.0.0.1");
	await once(server, "listening");

	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("Mock sandbox-agent failed to bind to a TCP port");
	}

	const baseUrl = `http://127.0.0.1:${address.port}`;
	const { SandboxAgent } = await import("sandbox-agent");
	const client = await SandboxAgent.connect({
		baseUrl,
		...(options.token ? { token: options.token } : {}),
		waitForHealth: { timeoutMs: 5_000 },
	});

	return {
		baseUrl,
		client,
		rootDir,
		path: (...segments: string[]) => resolve(rootDir, ...segments),
		stop: async () => {
			for (const proc of processes.values()) {
				if (proc.status === "running") {
					proc.child.kill("SIGKILL");
					await waitForProcessExit(proc);
				}
			}
			await new Promise<void>((resolveClose, rejectClose) => {
				server.close((error) => {
					if (error) {
						rejectClose(error);
						return;
					}
					resolveClose();
				});
			});
			await rm(rootDir, { force: true, recursive: true });
		},
	};
}
