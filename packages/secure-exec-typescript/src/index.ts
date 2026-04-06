import { mkdirSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import ts from "typescript";
import {
	type createNodeDriver,
	type NodeRuntimeDriverFactory,
} from "secure-exec";

const require = createRequire(import.meta.url);

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

type TsModule = typeof ts;

const DEFAULT_COMPILER_SPECIFIER = "typescript";

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

	const compiler = loadCompilerModule(request.compilerSpecifier);
	if (!compiler.ok) {
		return createFailureResult<TResult>(request.kind, compiler.error);
	}

	try {
		if (request.kind === "typecheckProject") {
			return (await typecheckProject(
				compiler.module,
				filesystem,
				request.options,
			)) as TResult;
		}
		if (request.kind === "compileProject") {
			return (await compileProject(
				compiler.module,
				filesystem,
				request.options,
			)) as TResult;
		}
		if (request.kind === "typecheckSource") {
			return (await typecheckSource(
				compiler.module,
				request.options,
			)) as TResult;
		}
		return (await compileSource(compiler.module, request.options)) as TResult;
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return createFailureResult<TResult>(request.kind, message);
	}
}

function loadCompilerModule(
	compilerSpecifier: string,
): { ok: true; module: TsModule } | { ok: false; error: string } {
	try {
		if (compilerSpecifier === "typescript") {
			return { ok: true, module: ts };
		}

		const resolved = resolveCompilerModuleSpecifier(compilerSpecifier);
		return {
			ok: true,
			module: require(resolved) as TsModule,
		};
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return {
			ok: false,
			error: message.includes(compilerSpecifier)
				? message
				: `Unable to load ${compilerSpecifier}: ${message}`,
		};
	}
}

function resolveCompilerModuleSpecifier(compilerSpecifier: string): string {
	if (compilerSpecifier.startsWith("/")) {
		return compilerSpecifier;
	}
	if (
		compilerSpecifier.startsWith("./") ||
		compilerSpecifier.startsWith("../")
	) {
		return path.resolve(compilerSpecifier);
	}
	return require.resolve(compilerSpecifier);
}

function resolveHostNodeModulesRoot(): string {
	return path.resolve(
		path.dirname(require.resolve("typescript/package.json")),
		"..",
	);
}

function toDiagnosticCategory(
	category: ts.DiagnosticCategory,
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

function toVirtualPath(
	hostRoot: string,
	virtualRoot: string,
	hostPath: string,
): string {
	const relativePath = path.relative(hostRoot, hostPath);
	return path.posix.join(
		virtualRoot,
		...relativePath.split(path.sep).filter(Boolean),
	);
}

function toHostPath(
	hostRoot: string,
	virtualRoot: string,
	virtualPath: string,
): string {
	const relativePath = path.posix.relative(virtualRoot, virtualPath);
	return path.join(hostRoot, ...relativePath.split("/").filter(Boolean));
}

function mapDiagnostic(
	compiler: TsModule,
	diagnostic: ts.Diagnostic,
	hostRoot: string | null,
	virtualRoot: string | null,
): TypeScriptDiagnostic {
	const result: TypeScriptDiagnostic = {
		code: diagnostic.code,
		category: toDiagnosticCategory(diagnostic.category),
		message: compiler.flattenDiagnosticMessageText(
			diagnostic.messageText,
			"\n",
		),
	};

	if (diagnostic.file && typeof diagnostic.start === "number") {
		const location = diagnostic.file.getLineAndCharacterOfPosition(
			diagnostic.start,
		);
		result.filePath =
			hostRoot && virtualRoot
				? toVirtualPath(hostRoot, virtualRoot, diagnostic.file.fileName)
				: diagnostic.file.fileName;
		result.line = location.line + 1;
		result.column = location.character + 1;
	}

	return result;
}

function remapCompilerOptionPath(
	value: string | undefined,
	hostRoot: string,
	virtualRoot: string,
): string | undefined {
	if (!value || !path.posix.isAbsolute(value)) {
		return value;
	}
	return toHostPath(hostRoot, virtualRoot, value);
}

function remapCompilerOptionPaths(
	options: ts.CompilerOptions,
	hostRoot: string,
	virtualRoot: string,
): ts.CompilerOptions {
	return {
		...options,
		outDir: remapCompilerOptionPath(options.outDir, hostRoot, virtualRoot),
		outFile: remapCompilerOptionPath(options.outFile, hostRoot, virtualRoot),
		rootDir: remapCompilerOptionPath(options.rootDir, hostRoot, virtualRoot),
		baseUrl: remapCompilerOptionPath(options.baseUrl, hostRoot, virtualRoot),
		declarationDir: remapCompilerOptionPath(
			options.declarationDir,
			hostRoot,
			virtualRoot,
		),
		tsBuildInfoFile: remapCompilerOptionPath(
			options.tsBuildInfoFile,
			hostRoot,
			virtualRoot,
		),
	};
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

async function materializeVirtualTree(
	filesystem: NonNullable<ReturnType<typeof createNodeDriver>["filesystem"]>,
	virtualRoot: string,
	hostRoot: string,
): Promise<void> {
	mkdirSync(hostRoot, { recursive: true });
	const entries = await filesystem.readDirWithTypes(virtualRoot);
	for (const entry of entries) {
		if (entry.name === "." || entry.name === "..") {
			continue;
		}
		const virtualPath = path.posix.join(virtualRoot, entry.name);
		const hostPath = path.join(hostRoot, entry.name);
		if (entry.isDirectory) {
			await materializeVirtualTree(filesystem, virtualPath, hostPath);
			continue;
		}
		const contents = await filesystem.readFile(virtualPath);
		mkdirSync(path.dirname(hostPath), { recursive: true });
		writeFileSync(hostPath, contents);
	}
}

async function withProjectWorkspace<T>(
	filesystem: NonNullable<ReturnType<typeof createNodeDriver>["filesystem"]>,
	virtualRoot: string,
	fn: (hostRoot: string) => Promise<T>,
): Promise<T> {
	const hostRoot = await mkdtemp(path.join(tmpdir(), "secure-exec-ts-"));
	try {
		await materializeVirtualTree(filesystem, virtualRoot, hostRoot);
		const nodeModulesLink = path.join(hostRoot, "node_modules");
		const hostNodeModulesRoot = resolveHostNodeModulesRoot();
		if (!pathExists(nodeModulesLink)) {
			symlinkSync(hostNodeModulesRoot, nodeModulesLink, "dir");
		}
		return await fn(hostRoot);
	} finally {
		await rm(hostRoot, { recursive: true, force: true });
	}
}

function pathExists(targetPath: string): boolean {
	try {
		readFileSync(targetPath);
		return true;
	} catch {
		return false;
	}
}

function collectProgramDiagnostics(
	compiler: TsModule,
	program: ts.Program,
	emitDiagnostics: readonly ts.Diagnostic[] = [],
	hostRoot: string,
	virtualRoot: string,
): TypeScriptDiagnostic[] {
	return [...compiler.getPreEmitDiagnostics(program), ...emitDiagnostics].map(
		(diagnostic) => mapDiagnostic(compiler, diagnostic, hostRoot, virtualRoot),
	);
}

async function typecheckProject(
	compiler: TsModule,
	filesystem: NonNullable<ReturnType<typeof createNodeDriver>["filesystem"]>,
	options: ProjectCompilerOptions,
): Promise<TypeCheckResult> {
	const virtualCwd = options.cwd ?? "/root";
	const virtualConfigPath =
		options.configFilePath ?? path.posix.join(virtualCwd, "tsconfig.json");
	const virtualRoot = path.posix.dirname(virtualConfigPath);

	return withProjectWorkspace(filesystem, virtualRoot, async (hostRoot) => {
		const hostConfigPath = toHostPath(hostRoot, virtualRoot, virtualConfigPath);
		const config = compiler.readConfigFile(hostConfigPath, compiler.sys.readFile);
		if (config.error) {
			return {
				success: false,
				diagnostics: [
					mapDiagnostic(compiler, config.error, hostRoot, virtualRoot),
				],
			};
		}

		const parsed = compiler.parseJsonConfigFileContent(
			config.config,
			compiler.sys,
			path.dirname(hostConfigPath),
		);
		parsed.options = remapCompilerOptionPaths(
			parsed.options,
			hostRoot,
			virtualRoot,
		);
		const program = compiler.createProgram({
			rootNames: parsed.fileNames,
			options: parsed.options,
		});
		const diagnostics = collectProgramDiagnostics(
			compiler,
			program,
			[],
			hostRoot,
			virtualRoot,
		);

		return {
			success: diagnostics.every((diagnostic) => diagnostic.category !== "error"),
			diagnostics,
		};
	});
}

async function compileProject(
	compiler: TsModule,
	filesystem: NonNullable<ReturnType<typeof createNodeDriver>["filesystem"]>,
	options: ProjectCompilerOptions,
): Promise<ProjectCompileResult> {
	const virtualCwd = options.cwd ?? "/root";
	const virtualConfigPath =
		options.configFilePath ?? path.posix.join(virtualCwd, "tsconfig.json");
	const virtualRoot = path.posix.dirname(virtualConfigPath);

	return withProjectWorkspace(filesystem, virtualRoot, async (hostRoot) => {
		const hostConfigPath = toHostPath(hostRoot, virtualRoot, virtualConfigPath);
		const config = compiler.readConfigFile(hostConfigPath, compiler.sys.readFile);
		if (config.error) {
			return {
				success: false,
				diagnostics: [
					mapDiagnostic(compiler, config.error, hostRoot, virtualRoot),
				],
				emitSkipped: true,
				emittedFiles: [],
			};
		}

		const parsed = compiler.parseJsonConfigFileContent(
			config.config,
			compiler.sys,
			path.dirname(hostConfigPath),
		);
		parsed.options = remapCompilerOptionPaths(
			parsed.options,
			hostRoot,
			virtualRoot,
		);
		const emittedHostFiles: string[] = [];
		const program = compiler.createProgram({
			rootNames: parsed.fileNames,
			options: parsed.options,
		});
		const emitResult = program.emit(
			undefined,
			(fileName, data) => {
				mkdirSync(path.dirname(fileName), { recursive: true });
				writeFileSync(fileName, data);
				emittedHostFiles.push(fileName);
			},
		);
		const diagnostics = collectProgramDiagnostics(
			compiler,
			program,
			emitResult.diagnostics,
			hostRoot,
			virtualRoot,
		);

		const emittedFiles: string[] = [];
		for (const hostFile of emittedHostFiles) {
			const virtualPath = toVirtualPath(hostRoot, virtualRoot, hostFile);
			const contents = readFileSync(hostFile);
			const parentDir = path.posix.dirname(virtualPath);
			await filesystem.mkdir(parentDir, { recursive: true });
			await filesystem.writeFile(virtualPath, contents);
			emittedFiles.push(virtualPath);
		}

		return {
			success:
				!emitResult.emitSkipped &&
				diagnostics.every((diagnostic) => diagnostic.category !== "error"),
			diagnostics,
			emitSkipped: emitResult.emitSkipped,
			emittedFiles,
		};
	});
}

async function typecheckSource(
	compiler: TsModule,
	options: SourceCompilerOptions,
): Promise<TypeCheckResult> {
	const sourcePath = options.filePath ?? "/root/input.ts";
	const hostRoot = await mkdtemp(path.join(tmpdir(), "secure-exec-ts-src-"));
	try {
		const hostSourcePath = toHostPath(hostRoot, "/root", sourcePath);
		mkdirSync(path.dirname(hostSourcePath), { recursive: true });
		writeFileSync(hostSourcePath, options.sourceText);
		const nodeModulesLink = path.join(hostRoot, "node_modules");
		symlinkSync(resolveHostNodeModulesRoot(), nodeModulesLink, "dir");

		const compilerOptions: ts.CompilerOptions = {
			module: ts.ModuleKind.CommonJS,
			target: ts.ScriptTarget.ES2022,
			skipLibCheck: true,
			...(options.compilerOptions as ts.CompilerOptions | undefined),
		};
		const program = compiler.createProgram({
			rootNames: [hostSourcePath],
			options: compilerOptions,
		});
		const diagnostics = collectProgramDiagnostics(
			compiler,
			program,
			[],
			hostRoot,
			"/root",
		);

		return {
			success: diagnostics.every((diagnostic) => diagnostic.category !== "error"),
			diagnostics,
		};
	} finally {
		await rm(hostRoot, { recursive: true, force: true });
	}
}

async function compileSource(
	compiler: TsModule,
	options: SourceCompilerOptions,
): Promise<SourceCompileResult> {
	const result = compiler.transpileModule(options.sourceText, {
		fileName: options.filePath ?? "/root/input.ts",
		compilerOptions: {
			module: ts.ModuleKind.CommonJS,
			target: ts.ScriptTarget.ES2022,
			...(options.compilerOptions as ts.CompilerOptions | undefined),
		},
		reportDiagnostics: true,
	});
	const diagnostics = (result.diagnostics ?? []).map((diagnostic) =>
		mapDiagnostic(compiler, diagnostic, null, null),
	);

	return {
		success: diagnostics.every((diagnostic) => diagnostic.category !== "error"),
		diagnostics,
		outputText: result.outputText,
		sourceMapText: result.sourceMapText,
	};
}
