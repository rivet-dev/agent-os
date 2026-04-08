import * as fsPromises from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import type { createNodeDriver, NodeRuntimeDriverFactory } from "secure-exec";

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

const DEFAULT_COMPILER_SPECIFIER = "typescript";
const moduleRequire = createRequire(import.meta.url);

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
		void options.runtimeDriverFactory;
		void options.memoryLimit;
		void options.cpuTimeLimitMs;
		const tempRoot = await fsPromises.mkdtemp(
			path.join(tmpdir(), "secure-exec-typescript-"),
		);
		try {
			await mirrorVirtualTree(filesystem, "/", tempRoot);
			const hostRequest = mapRequestToHostPaths(request, tempRoot);
			const ts = await loadTypeScriptCompiler(request.compilerSpecifier);
			await linkHostNodeModules(tempRoot, hostRequest);
			await rewriteProjectConfigPaths(hostRequest, tempRoot, ts);
			const runCompiler = new Function(
				"request",
				"ts",
				"require",
				`return (${compilerRuntimeMain.toString()})(request, ts);`,
			) as (
				request: CompilerRequest,
				ts: typeof import("typescript"),
				require: NodeJS.Require,
			) => CompilerResponse;
			const hostResult = runCompiler(hostRequest, ts, moduleRequire);
			return await mapHostResultToVirtualPaths(
				hostResult as TResult,
				filesystem,
				tempRoot,
			);
		} finally {
			await fsPromises.rm(tempRoot, { recursive: true, force: true });
		}
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return createFailureResult<TResult>(request.kind, message);
	}
}

async function linkHostNodeModules(
	tempRoot: string,
	request: CompilerRequest,
): Promise<void> {
	const hostNodeModules = findNearestNodeModules(process.cwd());
	if (!hostNodeModules) {
		return;
	}

	const linkTargets = [path.join(tempRoot, "node_modules")];
	const requestCwd = request.options.cwd;
	if (requestCwd) {
		linkTargets.push(path.join(requestCwd, "node_modules"));
	}

	for (const linkPath of linkTargets) {
		try {
			await fsPromises.lstat(linkPath);
			continue;
		} catch {}
		await fsPromises.mkdir(path.dirname(linkPath), { recursive: true });
		await fsPromises.symlink(hostNodeModules, linkPath, "junction");
	}
}

function findNearestNodeModules(startDir: string): string | null {
	let currentDir = startDir;
	while (true) {
		const candidate = path.join(currentDir, "node_modules");
		if (
			moduleRequire.resolve("typescript/package.json", { paths: [candidate] })
		) {
			return candidate;
		}
		const parentDir = path.dirname(currentDir);
		if (parentDir === currentDir) {
			return null;
		}
		currentDir = parentDir;
	}
}

async function rewriteProjectConfigPaths(
	request: CompilerRequest,
	tempRoot: string,
	ts: typeof import("typescript"),
): Promise<void> {
	const configFilePath = getProjectConfigPath(request);
	if (!configFilePath) {
		return;
	}

	try {
		await fsPromises.access(configFilePath);
	} catch {
		return;
	}

	const configFile = ts.readConfigFile(configFilePath, ts.sys.readFile);
	if (
		configFile.error ||
		!configFile.config ||
		typeof configFile.config !== "object"
	) {
		return;
	}

	const config = configFile.config as {
		compilerOptions?: Record<string, unknown>;
	};
	config.compilerOptions = mapConfigCompilerOptionsToHost(
		tempRoot,
		config.compilerOptions,
	);
	await fsPromises.writeFile(
		configFilePath,
		JSON.stringify(configFile.config, null, 2),
	);
}

function getProjectConfigPath(request: CompilerRequest): string | null {
	switch (request.kind) {
		case "typecheckProject":
		case "compileProject":
			return (
				request.options.configFilePath ??
				(request.options.cwd
					? path.join(request.options.cwd, "tsconfig.json")
					: null)
			);
		case "typecheckSource":
		case "compileSource":
			return request.options.configFilePath ?? null;
	}
}

function mapConfigCompilerOptionsToHost(
	tempRoot: string,
	compilerOptions: Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
	if (!compilerOptions) {
		return compilerOptions;
	}

	const mapped = mapCompilerOptionsToHost(tempRoot, compilerOptions) ?? {};
	for (const key of ["rootDirs", "typeRoots"]) {
		const value = mapped[key];
		if (Array.isArray(value)) {
			mapped[key] = value.map((entry) =>
				mapAbsoluteCompilerPath(tempRoot, entry),
			);
		}
	}
	return mapped;
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
		return "TypeScript compiler exceeded sandbox memory limit";
	}
	if (/cpu time limit exceeded|timed out/i.test(message)) {
		return "TypeScript compiler exceeded sandbox CPU time limit";
	}
	return message;
}

function toHostPath(tempRoot: string, virtualPath: string): string {
	if (virtualPath === "/") {
		return tempRoot;
	}
	return path.join(tempRoot, virtualPath.replace(/^\/+/, ""));
}

function toVirtualPath(tempRoot: string, hostPath: string): string {
	const relative = path.relative(tempRoot, hostPath);
	if (!relative || relative === ".") {
		return "/";
	}
	return `/${relative.split(path.sep).join("/")}`;
}

function mapAbsoluteCompilerPath(tempRoot: string, value: unknown): unknown {
	if (typeof value !== "string" || !value.startsWith("/")) {
		return value;
	}
	return toHostPath(tempRoot, value);
}

function mapCompilerOptionsToHost(
	tempRoot: string,
	compilerOptions: Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
	if (!compilerOptions) {
		return compilerOptions;
	}
	const mapped = { ...compilerOptions };
	for (const key of [
		"outDir",
		"outFile",
		"rootDir",
		"baseUrl",
		"declarationDir",
		"tsBuildInfoFile",
		"mapRoot",
		"sourceRoot",
	]) {
		mapped[key] = mapAbsoluteCompilerPath(tempRoot, mapped[key]);
	}
	return mapped;
}

function mapRequestToHostPaths(
	request: CompilerRequest,
	tempRoot: string,
): CompilerRequest {
	switch (request.kind) {
		case "typecheckProject":
		case "compileProject":
			return {
				...request,
				options: {
					...request.options,
					cwd: request.options.cwd
						? toHostPath(tempRoot, request.options.cwd)
						: request.options.cwd,
					configFilePath:
						request.options.configFilePath &&
						request.options.configFilePath.startsWith("/")
							? toHostPath(tempRoot, request.options.configFilePath)
							: request.options.configFilePath,
				},
			};
		case "typecheckSource":
		case "compileSource":
			return {
				...request,
				options: {
					...request.options,
					cwd: request.options.cwd
						? toHostPath(tempRoot, request.options.cwd)
						: request.options.cwd,
					filePath:
						request.options.filePath && request.options.filePath.startsWith("/")
							? toHostPath(tempRoot, request.options.filePath)
							: request.options.filePath,
					configFilePath:
						request.options.configFilePath &&
						request.options.configFilePath.startsWith("/")
							? toHostPath(tempRoot, request.options.configFilePath)
							: request.options.configFilePath,
					compilerOptions: mapCompilerOptionsToHost(
						tempRoot,
						request.options.compilerOptions,
					),
				},
			};
	}
}

async function mirrorVirtualTree(
	filesystem: NonNullable<TypeScriptToolsOptions["systemDriver"]["filesystem"]>,
	virtualPath: string,
	tempRoot: string,
): Promise<void> {
	const hostPath = toHostPath(tempRoot, virtualPath);
	const statInfo =
		virtualPath === "/"
			? await filesystem.stat(virtualPath)
			: await filesystem.lstat(virtualPath);

	if (statInfo.isSymbolicLink) {
		await fsPromises.mkdir(path.dirname(hostPath), { recursive: true });
		const target = await filesystem.readlink(virtualPath);
		await fsPromises.symlink(
			target.startsWith("/") ? toHostPath(tempRoot, target) : target,
			hostPath,
		);
		return;
	}

	if (statInfo.isDirectory) {
		await fsPromises.mkdir(hostPath, { recursive: true });
		for (const entry of await filesystem.readDirWithTypes(virtualPath)) {
			if (entry.name === "." || entry.name === "..") {
				continue;
			}
			const childPath =
				virtualPath === "/" ? `/${entry.name}` : `${virtualPath}/${entry.name}`;
			await mirrorVirtualTree(filesystem, childPath, tempRoot);
		}
		return;
	}

	await fsPromises.mkdir(path.dirname(hostPath), { recursive: true });
	await fsPromises.writeFile(hostPath, await filesystem.readFile(virtualPath));
}

async function loadTypeScriptCompiler(
	compilerSpecifier: string,
): Promise<typeof import("typescript")> {
	const specifier =
		compilerSpecifier === DEFAULT_COMPILER_SPECIFIER
			? compilerSpecifier
			: compilerSpecifier.startsWith("/")
				? pathToFileURL(compilerSpecifier).href
				: compilerSpecifier.startsWith("./") ||
						compilerSpecifier.startsWith("../")
					? pathToFileURL(path.resolve(compilerSpecifier)).href
					: compilerSpecifier;
	const imported = await import(specifier);
	return (imported.default ?? imported) as typeof import("typescript");
}

async function mapHostResultToVirtualPaths<TResult extends CompilerResponse>(
	result: TResult,
	filesystem: NonNullable<TypeScriptToolsOptions["systemDriver"]["filesystem"]>,
	tempRoot: string,
): Promise<TResult> {
	for (const diagnostic of result.diagnostics) {
		if (diagnostic.filePath) {
			diagnostic.filePath = toVirtualPath(tempRoot, diagnostic.filePath);
		}
	}

	if ("emittedFiles" in result) {
		result.emittedFiles = await Promise.all(
			result.emittedFiles.map(async (hostPath) => {
				const virtualPath = toVirtualPath(tempRoot, hostPath);
				await filesystem.mkdir(path.posix.dirname(virtualPath), {
					recursive: true,
				});
				await filesystem.writeFile(
					virtualPath,
					new Uint8Array(await fsPromises.readFile(hostPath)),
				);
				return virtualPath;
			}),
		);
	}

	return result;
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
			options.filePath ?? "__secure_exec_typescript_input__.ts",
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
