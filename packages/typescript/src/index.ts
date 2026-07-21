import { realpathSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import type { AgentOs } from "@rivet-dev/agentos-core";
import {
	createKernel,
	type createNodeDriver,
	createNodeRuntime,
	NodeFileSystem,
	type NodeRuntimeDriver,
	type NodeRuntimeDriverFactory,
	type Permissions,
} from "@rivet-dev/agentos-core/internal/runtime-compat";

export interface TypeScriptDiagnostic {
	code: number;
	category: "error" | "warning" | "suggestion" | "message";
	message: string;
	filePath?: string;
	line?: number;
	column?: number;
}

export interface TypeCheckResult {
	success: boolean;
	diagnostics: TypeScriptDiagnostic[];
}

export interface ProjectCompileResult extends TypeCheckResult {
	emitSkipped: boolean;
	emittedFiles: string[];
}

export interface SourceCompileResult extends TypeCheckResult {
	outputText?: string;
	sourceMapText?: string;
}

export interface ProjectCompilerOptions {
	cwd?: string;
	configFilePath?: string;
}

export interface SourceCompilerOptions {
	sourceText: string;
	filePath?: string;
	cwd?: string;
	configFilePath?: string;
	compilerOptions?: Record<string, unknown>;
}

export interface TypeScriptToolsOptions {
	systemDriver: ReturnType<typeof createNodeDriver>;
	runtimeDriverFactory: NodeRuntimeDriverFactory;
	memoryLimit?: number;
	cpuTimeLimitMs?: number;
	compilerSpecifier?: string;
}

export interface AgentOsTypeScriptToolsOptions {
	/** Guest path or package specifier for the TypeScript compiler. */
	compilerSpecifier: string;
	/** Per-compiler-invocation wall-clock limit. */
	timeoutMs?: number;
	/** Per-compiler-invocation CPU limit. */
	cpuTimeLimitMs?: number;
}

export interface TypeScriptTools {
	typecheckProject(options?: ProjectCompilerOptions): Promise<TypeCheckResult>;
	compileProject(
		options?: ProjectCompilerOptions,
	): Promise<ProjectCompileResult>;
	typecheckSource(options: SourceCompilerOptions): Promise<TypeCheckResult>;
	compileSource(options: SourceCompilerOptions): Promise<SourceCompileResult>;
}

type CompilerRequest =
	| {
			kind: "typecheckProject";
			compilerSpecifier: string;
			options: ProjectCompilerOptions;
	  }
	| {
			kind: "compileProject";
			compilerSpecifier: string;
			options: ProjectCompilerOptions;
	  }
	| {
			kind: "typecheckSource";
			compilerSpecifier: string;
			options: SourceCompilerOptions;
	  }
	| {
			kind: "compileSource";
			compilerSpecifier: string;
			options: SourceCompilerOptions;
	  };

type CompilerResponse =
	| TypeCheckResult
	| ProjectCompileResult
	| SourceCompileResult;
type RuntimeCompilerEnvelope =
	| { ok: true; result: CompilerResponse }
	| { ok: false; errorMessage?: string };
interface RuntimeNodeModulesMount {
	guestPath: string;
	hostPath: string;
}

const DEFAULT_COMPILER_SPECIFIER = "typescript";
const moduleRequire = createRequire(import.meta.url);
const GUEST_NODE_PATH_DELIMITER = ":";
let nextRuntimeRequestId = 0;

export function createTypeScriptTools(
	options: TypeScriptToolsOptions,
): TypeScriptTools {
	return {
		typecheckProject: async (requestOptions = {}) =>
			runCompilerRequest<TypeCheckResult>(options, {
				kind: "typecheckProject",
				compilerSpecifier:
					options.compilerSpecifier ?? DEFAULT_COMPILER_SPECIFIER,
				options: requestOptions,
			}),
		compileProject: async (requestOptions = {}) =>
			runCompilerRequest<ProjectCompileResult>(options, {
				kind: "compileProject",
				compilerSpecifier:
					options.compilerSpecifier ?? DEFAULT_COMPILER_SPECIFIER,
				options: requestOptions,
			}),
		typecheckSource: async (requestOptions) =>
			runCompilerRequest<TypeCheckResult>(options, {
				kind: "typecheckSource",
				compilerSpecifier:
					options.compilerSpecifier ?? DEFAULT_COMPILER_SPECIFIER,
				options: requestOptions,
			}),
		compileSource: async (requestOptions) =>
			runCompilerRequest<SourceCompileResult>(options, {
				kind: "compileSource",
				compilerSpecifier:
					options.compilerSpecifier ?? DEFAULT_COMPILER_SPECIFIER,
				options: requestOptions,
			}),
	};
}

let nextAgentOsCompilerRequestId = 0;

/**
 * Bind the TypeScript compiler to an existing AgentOS VM.
 *
 * Unlike the legacy driver-oriented constructor, this runs every compiler
 * invocation in the caller's VM, so source files, tsconfig projects,
 * permissions, and resource limits all share one sidecar-owned environment.
 */
export function createAgentOsTypeScriptTools(
	vm: AgentOs,
	options: AgentOsTypeScriptToolsOptions,
): TypeScriptTools {
	const run = async <TResult extends CompilerResponse>(
		request: Omit<CompilerRequest, "compilerSpecifier">,
	): Promise<TResult> => {
		const completeRequest = {
			...request,
			compilerSpecifier: options.compilerSpecifier,
		} as CompilerRequest;
		try {
			return (await runCompilerRequestInAgentOs(
				vm,
				completeRequest,
				options,
			)) as TResult;
		} catch (error) {
			return createFailureResult<TResult>(
				completeRequest.kind,
				error instanceof Error ? error.message : String(error),
			);
		}
	};

	return {
		typecheckProject: (requestOptions = {}) =>
			run<TypeCheckResult>({
				kind: "typecheckProject",
				options: requestOptions,
			}),
		compileProject: (requestOptions = {}) =>
			run<ProjectCompileResult>({
				kind: "compileProject",
				options: requestOptions,
			}),
		typecheckSource: (requestOptions) =>
			run<TypeCheckResult>({
				kind: "typecheckSource",
				options: requestOptions,
			}),
		compileSource: (requestOptions) =>
			run<SourceCompileResult>({
				kind: "compileSource",
				options: requestOptions,
			}),
	};
}

async function runCompilerRequestInAgentOs(
	vm: AgentOs,
	request: CompilerRequest,
	options: AgentOsTypeScriptToolsOptions,
): Promise<CompilerResponse> {
	const requestId = nextAgentOsCompilerRequestId++;
	const requestPath = `/tmp/agentos-typescript-request-${requestId}.json`;
	const runnerPath = `/tmp/agentos-typescript-runner-${requestId}.cjs`;
	await vm.writeFile(requestPath, JSON.stringify(request));
	await vm.writeFile(runnerPath, buildCompilerRuntimeScript(requestPath));

	try {
		const result = await vm.execArgv("node", [runnerPath], {
			cwd: request.options.cwd ?? "/workspace",
			timeout: options.timeoutMs,
			cpuTimeLimitMs: options.cpuTimeLimitMs,
		});
		if (result.stdout.trim()) {
			return parseRuntimeResponse(result.stdout);
		}
		if (result.exitCode !== 0) {
			throw new Error(
				result.stderr.trim() || `TypeScript runtime exited ${result.exitCode}`,
			);
		}
		throw new Error("TypeScript runtime produced no response");
	} finally {
		for (const targetPath of [requestPath, runnerPath]) {
			try {
				if (await vm.exists(targetPath)) await vm.remove(targetPath);
			} catch (error) {
				console.error(
					`[agentos] failed to remove TypeScript compiler temporary file ${targetPath}:`,
					error,
				);
			}
		}
	}
}

async function runCompilerRequest<TResult extends CompilerResponse>(
	options: TypeScriptToolsOptions,
	request: CompilerRequest,
): Promise<TResult> {
	const filesystem = options.systemDriver.filesystem;
	if (!filesystem) {
		return createFailureResult<TResult>(
			request.kind,
			"TypeScript tools require a filesystem-backed system driver",
		);
	}

	try {
		return (await runCompilerInRuntime(options, request)) as TResult;
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return createFailureResult<TResult>(request.kind, message);
	}
}

async function runCompilerInRuntime(
	options: TypeScriptToolsOptions,
	request: CompilerRequest,
): Promise<CompilerResponse> {
	const filesystem = options.systemDriver.filesystem;
	if (!filesystem) {
		throw new Error(
			"TypeScript tools require a filesystem-backed system driver",
		);
	}

	const nodeModulesMount = resolveNodeModulesMount(options);
	if (!nodeModulesMount) {
		throw new Error(
			"Unable to locate host node_modules for TypeScript runtime",
		);
	}

	const runtimeDriver = options.runtimeDriverFactory.createRuntimeDriver({
		system: options.systemDriver,
		runtime: options.systemDriver.runtime,
		memoryLimit: options.memoryLimit,
		cpuTimeLimitMs: options.cpuTimeLimitMs,
	});
	try {
		return await runCompilerWithRuntimeDriver(runtimeDriver, request);
	} catch (error) {
		if (!isUnavailableRuntimeDriverError(error)) {
			throw error;
		}
	} finally {
		try {
			runtimeDriver.dispose();
		} catch {}
	}

	return runCompilerWithKernelRuntime(options, request, nodeModulesMount);
}

async function runCompilerWithRuntimeDriver(
	runtimeDriver: NodeRuntimeDriver,
	request: CompilerRequest,
): Promise<CompilerResponse> {
	const result = await runtimeDriver.run<RuntimeCompilerEnvelope>(
		buildCompilerRuntimeEval(request),
		"/tmp/agentos-typescript-runner.cjs",
	);
	if (result.value) {
		return parseRuntimeEnvelope(result.value);
	}
	if (result.errorMessage) {
		throw new Error(result.errorMessage);
	}
	throw new Error(`TypeScript runtime exited ${result.code}`);
}

function isUnavailableRuntimeDriverError(error: unknown): boolean {
	return (
		error instanceof Error &&
		error.message.includes(
			"NodeExecutionDriver is not available after the native runtime migration",
		)
	);
}

async function runCompilerWithKernelRuntime(
	options: TypeScriptToolsOptions,
	request: CompilerRequest,
	nodeModulesMount: RuntimeNodeModulesMount,
): Promise<CompilerResponse> {
	const filesystem = options.systemDriver.filesystem;
	if (!filesystem) {
		throw new Error(
			"TypeScript tools require a filesystem-backed system driver",
		);
	}

	await filesystem.mkdir("/tmp", { recursive: true });
	const requestId = `${Date.now()}-${nextRuntimeRequestId++}`;
	const requestPath = `/tmp/agentos-typescript-request-${requestId}.json`;
	const runnerPath = `/tmp/agentos-typescript-runner-${requestId}.cjs`;
	await filesystem.writeFile(requestPath, JSON.stringify(request));
	await filesystem.writeFile(
		runnerPath,
		buildCompilerRuntimeScript(requestPath),
	);

	const kernel = createKernel({
		filesystem,
		permissions: normalizeKernelPermissions(options.systemDriver.permissions),
		env: buildRuntimeEnv(options, nodeModulesMount.guestPath),
		cwd: request.options.cwd ?? "/root",
		mounts: [
			{
				path: nodeModulesMount.guestPath,
				fs: new NodeFileSystem({ root: nodeModulesMount.hostPath }),
				readOnly: true,
			},
		],
	});

	try {
		await kernel.mount(createNodeRuntime());
		let stdout = "";
		let stderr = "";
		const child = kernel.spawn("node", [runnerPath], {
			cpuTimeLimitMs: options.cpuTimeLimitMs,
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});
		const exitCode = await child.wait();
		if (stdout.trim()) {
			return parseRuntimeResponse(stdout);
		}
		if (exitCode !== 0) {
			throw new Error(stderr.trim() || `TypeScript runtime exited ${exitCode}`);
		}
		throw new Error("TypeScript runtime produced no response");
	} finally {
		await kernel.dispose();
		await removeVirtualFileIfExists(filesystem, requestPath);
		await removeVirtualFileIfExists(filesystem, runnerPath);
	}
}

function normalizeKernelPermissions(
	permissions: TypeScriptToolsOptions["systemDriver"]["permissions"],
): Permissions {
	const normalized =
		!permissions || typeof permissions !== "string"
			? { ...(permissions ?? {}) }
			: { fs: permissions };
	if (!normalized.childProcess) {
		normalized.childProcess = {
			default: "deny",
			rules: [{ mode: "allow", operations: ["*"], patterns: ["node"] }],
		};
	}
	return normalized;
}

function findNearestNodeModules(startDir: string): string | null {
	let currentDir = startDir;
	while (true) {
		const candidate = path.join(currentDir, "node_modules");
		try {
			const packageJsonPath = moduleRequire.resolve("typescript/package.json", {
				paths: [currentDir],
			});
			const candidateRoot = realpathSync(candidate);
			const packageRoot = realpathSync(path.dirname(packageJsonPath));
			if (
				packageRoot === candidateRoot ||
				packageRoot.startsWith(`${candidateRoot}${path.sep}`)
			) {
				return candidate;
			}
		} catch {
			// Keep walking toward the filesystem root.
		}
		const parentDir = path.dirname(currentDir);
		if (parentDir === currentDir) {
			return null;
		}
		currentDir = parentDir;
	}
}

function resolveNodeModulesMount(
	options: TypeScriptToolsOptions,
): RuntimeNodeModulesMount | null {
	for (const mount of options.systemDriver.mounts) {
		const config = mount.plugin.config;
		if (
			mount.plugin.id === "host_dir" &&
			config &&
			typeof config.hostPath === "string" &&
			mount.path.endsWith("/node_modules")
		) {
			return {
				guestPath: mount.path,
				hostPath: config.hostPath,
			};
		}
	}

	const hostPath = findNearestNodeModules(process.cwd());
	return hostPath ? { guestPath: "/node_modules", hostPath } : null;
}

function createFailureResult<TResult extends CompilerResponse>(
	kind: CompilerRequest["kind"],
	errorMessage?: string,
): TResult {
	const diagnostic = {
		code: 0,
		category: "error" as const,
		message: normalizeCompilerFailureMessage(errorMessage),
	};

	if (kind === "compileProject") {
		return {
			success: false,
			diagnostics: [diagnostic],
			emitSkipped: true,
			emittedFiles: [],
		} as unknown as TResult;
	}

	if (kind === "compileSource") {
		return {
			success: false,
			diagnostics: [diagnostic],
		} as unknown as TResult;
	}

	return {
		success: false,
		diagnostics: [diagnostic],
	} as unknown as TResult;
}

function normalizeCompilerFailureMessage(errorMessage?: string): string {
	const message = (errorMessage ?? "TypeScript compiler failed").trim();
	if (/memory limit/i.test(message)) {
		return "TypeScript compiler exceeded VM memory limit";
	}
	if (/cpu time limit exceeded|timed out/i.test(message)) {
		return "TypeScript compiler exceeded VM CPU time limit";
	}
	return message;
}

function buildRuntimeEnv(
	options: TypeScriptToolsOptions,
	nodeModulesGuestPath: string,
): Record<string, string> {
	const env = { ...(options.systemDriver.runtime.process.env ?? {}) };
	env.NODE_PATH = [env.NODE_PATH, nodeModulesGuestPath]
		.filter(Boolean)
		.join(GUEST_NODE_PATH_DELIMITER);
	if (options.memoryLimit !== undefined) {
		const limit = Math.max(1, Math.floor(options.memoryLimit));
		env.NODE_OPTIONS = [env.NODE_OPTIONS, `--max-old-space-size=${limit}`]
			.filter(Boolean)
			.join(" ");
	}
	return env;
}

function buildCompilerRuntimeScript(requestPath: string): string {
	return `
const fs = require("node:fs");
const path = require("node:path");
const __require = require;

function loadTypeScriptCompiler(compilerSpecifier) {
	const specifier =
		compilerSpecifier === ${JSON.stringify(DEFAULT_COMPILER_SPECIFIER)}
			? compilerSpecifier
			: compilerSpecifier.startsWith("/")
				? compilerSpecifier
				: compilerSpecifier.startsWith("./") || compilerSpecifier.startsWith("../")
					? path.resolve(process.cwd(), compilerSpecifier)
					: compilerSpecifier;
	const imported = require(specifier);
	return imported.default ?? imported;
}

try {
	const request = JSON.parse(fs.readFileSync(${JSON.stringify(requestPath)}, "utf8"));
	const ts = loadTypeScriptCompiler(request.compilerSpecifier);
	const __name = (target) => target;
	const result = (${compilerRuntimeMain.toString()})(request, ts);
	process.stdout.write(JSON.stringify({ ok: true, result }));
} catch (error) {
	process.stdout.write(JSON.stringify({
		ok: false,
		errorMessage: error instanceof Error ? error.message : String(error),
	}));
	process.exitCode = 1;
}
`;
}

function buildCompilerRuntimeEval(request: CompilerRequest): string {
	return `
const path = require("node:path");
const __require = require;

function loadTypeScriptCompiler(compilerSpecifier) {
	const specifier =
		compilerSpecifier === ${JSON.stringify(DEFAULT_COMPILER_SPECIFIER)}
			? compilerSpecifier
			: compilerSpecifier.startsWith("/")
				? compilerSpecifier
				: compilerSpecifier.startsWith("./") || compilerSpecifier.startsWith("../")
					? path.resolve(process.cwd(), compilerSpecifier)
					: compilerSpecifier;
	const imported = require(specifier);
	return imported.default ?? imported;
}

const request = ${JSON.stringify(request)};
try {
	const ts = loadTypeScriptCompiler(request.compilerSpecifier);
	const __name = (target) => target;
	const result = (${compilerRuntimeMain.toString()})(request, ts);
	return { ok: true, result };
} catch (error) {
	return {
		ok: false,
		errorMessage: error instanceof Error ? error.message : String(error),
	};
}
`;
}

function parseRuntimeResponse(stdout: string): CompilerResponse {
	return parseRuntimeEnvelope(
		JSON.parse(stdout.trim()) as RuntimeCompilerEnvelope,
	);
}

function parseRuntimeEnvelope(
	payload: RuntimeCompilerEnvelope,
): CompilerResponse {
	if (payload.ok) {
		return payload.result;
	}
	throw new Error(payload.errorMessage ?? "TypeScript runtime failed");
}

async function removeVirtualFileIfExists(
	filesystem: NonNullable<TypeScriptToolsOptions["systemDriver"]["filesystem"]>,
	targetPath: string,
): Promise<void> {
	try {
		await filesystem.removeFile(targetPath);
	} catch {}
}

function compilerRuntimeMain(
	request: CompilerRequest,
	ts: typeof import("typescript"),
): CompilerResponse {
	const fs = require("node:fs") as typeof import("node:fs");
	const path = require("node:path") as typeof import("node:path");

	function toDiagnostic(
		diagnostic: import("typescript").Diagnostic,
	): TypeScriptDiagnostic {
		const message = ts
			.flattenDiagnosticMessageText(diagnostic.messageText, "\n")
			.trim();
		const result: TypeScriptDiagnostic = {
			code: diagnostic.code,
			category: toDiagnosticCategory(diagnostic.category),
			message,
		};

		if (!diagnostic.file || diagnostic.start === undefined) {
			return result;
		}

		const { line, character } = diagnostic.file.getLineAndCharacterOfPosition(
			diagnostic.start,
		);
		result.filePath = diagnostic.file.fileName.replace(/\\/g, "/");
		result.line = line + 1;
		result.column = character + 1;
		return result;
	}

	function toDiagnosticCategory(
		category: import("typescript").DiagnosticCategory,
	): TypeScriptDiagnostic["category"] {
		switch (category) {
			case ts.DiagnosticCategory.Warning:
				return "warning";
			case ts.DiagnosticCategory.Suggestion:
				return "suggestion";
			case ts.DiagnosticCategory.Message:
				return "message";
			default:
				return "error";
		}
	}

	function hasErrors(diagnostics: TypeScriptDiagnostic[]): boolean {
		return diagnostics.some((diagnostic) => diagnostic.category === "error");
	}

	function convertCompilerOptions(
		compilerOptions: Record<string, unknown> | undefined,
		basePath: string,
	): import("typescript").CompilerOptions {
		if (!compilerOptions) {
			return {};
		}

		const converted = ts.convertCompilerOptionsFromJson(
			compilerOptions,
			basePath,
		);
		if (converted.errors.length > 0) {
			throw new Error(
				converted.errors
					.map((diagnostic) => toDiagnostic(diagnostic).message)
					.join("\n"),
			);
		}

		return converted.options;
	}

	function resolveProjectConfig(
		options: ProjectCompilerOptions,
		overrideCompilerOptions: import("typescript").CompilerOptions = {},
	) {
		const cwd = path.resolve(options.cwd ?? "/root");
		const configFilePath = options.configFilePath
			? path.resolve(cwd, options.configFilePath)
			: ts.findConfigFile(cwd, ts.sys.fileExists, "tsconfig.json");

		if (!configFilePath) {
			throw new Error(`Unable to find tsconfig.json from '${cwd}'`);
		}

		const configFile = ts.readConfigFile(configFilePath, ts.sys.readFile);
		if (configFile.error) {
			return {
				parsed: null,
				diagnostics: [toDiagnostic(configFile.error)],
			};
		}

		const parsed = ts.parseJsonConfigFileContent(
			configFile.config,
			ts.sys,
			path.dirname(configFilePath),
			overrideCompilerOptions,
			configFilePath,
		);

		return {
			parsed,
			diagnostics: parsed.errors.map(toDiagnostic),
		};
	}

	function createSourceProgram(
		options: SourceCompilerOptions,
		overrideCompilerOptions: import("typescript").CompilerOptions = {},
	) {
		const cwd = path.resolve(options.cwd ?? "/root");
		const filePath = path.resolve(
			cwd,
			options.filePath ?? "__agentos_typescript_input__.ts",
		);
		const projectCompilerOptions = options.configFilePath
			? resolveProjectConfig(
					{ cwd, configFilePath: options.configFilePath },
					overrideCompilerOptions,
				)
			: { parsed: null, diagnostics: [] as TypeScriptDiagnostic[] };

		if (projectCompilerOptions.diagnostics.length > 0) {
			return {
				filePath,
				program: null,
				host: null,
				diagnostics: projectCompilerOptions.diagnostics,
			};
		}

		const compilerOptions = {
			target: ts.ScriptTarget.ES2022,
			module: ts.ModuleKind.CommonJS,
			...projectCompilerOptions.parsed?.options,
			...convertCompilerOptions(options.compilerOptions, cwd),
			...overrideCompilerOptions,
		};
		const host = ts.createCompilerHost(compilerOptions);
		const normalizedFilePath = ts.sys.useCaseSensitiveFileNames
			? filePath
			: filePath.toLowerCase();
		const defaultGetSourceFile = host.getSourceFile.bind(host);
		const defaultReadFile = host.readFile.bind(host);
		const defaultFileExists = host.fileExists.bind(host);

		host.fileExists = (candidatePath) => {
			const normalizedCandidate = ts.sys.useCaseSensitiveFileNames
				? candidatePath
				: candidatePath.toLowerCase();
			return (
				normalizedCandidate === normalizedFilePath ||
				defaultFileExists(candidatePath)
			);
		};

		host.readFile = (candidatePath) => {
			const normalizedCandidate = ts.sys.useCaseSensitiveFileNames
				? candidatePath
				: candidatePath.toLowerCase();
			if (normalizedCandidate === normalizedFilePath) {
				return options.sourceText;
			}
			return defaultReadFile(candidatePath);
		};

		host.getSourceFile = (
			candidatePath,
			languageVersion,
			onError,
			shouldCreateNewSourceFile,
		) => {
			const normalizedCandidate = ts.sys.useCaseSensitiveFileNames
				? candidatePath
				: candidatePath.toLowerCase();
			if (normalizedCandidate === normalizedFilePath) {
				return ts.createSourceFile(
					candidatePath,
					options.sourceText,
					languageVersion,
					true,
				);
			}
			return defaultGetSourceFile(
				candidatePath,
				languageVersion,
				onError,
				shouldCreateNewSourceFile,
			);
		};

		return {
			filePath,
			host,
			program: ts.createProgram([filePath], compilerOptions, host),
			diagnostics: [] as TypeScriptDiagnostic[],
		};
	}

	switch (request.kind) {
		case "typecheckProject": {
			const { parsed, diagnostics } = resolveProjectConfig(request.options, {
				noEmit: true,
			});
			if (!parsed) {
				return {
					success: false,
					diagnostics,
				};
			}

			const program = ts.createProgram({
				rootNames: parsed.fileNames,
				options: parsed.options,
				projectReferences: parsed.projectReferences,
			});
			const combinedDiagnostics = ts
				.sortAndDeduplicateDiagnostics([
					...parsed.errors,
					...ts.getPreEmitDiagnostics(program),
				])
				.map(toDiagnostic);

			return {
				success: !hasErrors(combinedDiagnostics),
				diagnostics: combinedDiagnostics,
			};
		}

		case "compileProject": {
			const { parsed, diagnostics } = resolveProjectConfig(request.options);
			if (!parsed) {
				return {
					success: false,
					diagnostics,
					emitSkipped: true,
					emittedFiles: [],
				};
			}

			const program = ts.createProgram({
				rootNames: parsed.fileNames,
				options: parsed.options,
				projectReferences: parsed.projectReferences,
			});
			const emittedFiles: string[] = [];
			const emitResult = program.emit(undefined, (fileName, text) => {
				fs.mkdirSync(path.dirname(fileName), { recursive: true });
				fs.writeFileSync(fileName, text, "utf8");
				emittedFiles.push(fileName.replace(/\\/g, "/"));
			});
			const combinedDiagnostics = ts
				.sortAndDeduplicateDiagnostics([
					...parsed.errors,
					...ts.getPreEmitDiagnostics(program),
					...emitResult.diagnostics,
				])
				.map(toDiagnostic);

			return {
				success: !hasErrors(combinedDiagnostics),
				diagnostics: combinedDiagnostics,
				emitSkipped: emitResult.emitSkipped,
				emittedFiles,
			};
		}

		case "typecheckSource": {
			const { program, diagnostics } = createSourceProgram(request.options, {
				noEmit: true,
			});
			if (!program) {
				return {
					success: false,
					diagnostics,
				};
			}

			const combinedDiagnostics = ts
				.sortAndDeduplicateDiagnostics(ts.getPreEmitDiagnostics(program))
				.map(toDiagnostic);

			return {
				success: !hasErrors(combinedDiagnostics),
				diagnostics: combinedDiagnostics,
			};
		}

		case "compileSource": {
			const { program, diagnostics } = createSourceProgram(request.options);
			if (!program) {
				return {
					success: false,
					diagnostics,
				};
			}

			let outputText: string | undefined;
			let sourceMapText: string | undefined;
			const emitResult = program.emit(undefined, (fileName, text) => {
				if (
					fileName.endsWith(".js") ||
					fileName.endsWith(".mjs") ||
					fileName.endsWith(".cjs")
				) {
					outputText = text;
					return;
				}
				if (fileName.endsWith(".map")) {
					sourceMapText = text;
				}
			});
			const combinedDiagnostics = ts
				.sortAndDeduplicateDiagnostics([
					...ts.getPreEmitDiagnostics(program),
					...emitResult.diagnostics,
				])
				.map(toDiagnostic);

			return {
				success: !hasErrors(combinedDiagnostics),
				diagnostics: combinedDiagnostics,
				outputText,
				sourceMapText,
			};
		}
	}
}
