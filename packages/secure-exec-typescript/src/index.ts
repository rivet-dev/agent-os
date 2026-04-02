import path from "node:path";
import {
	type createNodeDriver,
	NodeRuntime,
	type NodeRuntimeDriverFactory,
} from "secure-exec";

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

const DEFAULT_COMPILER_RUNTIME_MEMORY_LIMIT = 512;
const DEFAULT_COMPILER_SPECIFIER = "typescript";
const COMPILER_RUNTIME_FILE_PATH =
	"/tmp/__secure_exec_typescript_compiler__.js";

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

	const compilerModulePath = resolveCompilerModulePath(
		request.compilerSpecifier,
	);
	const runtime = new NodeRuntime({
		systemDriver: options.systemDriver,
		runtimeDriverFactory: options.runtimeDriverFactory,
		memoryLimit: options.memoryLimit ?? DEFAULT_COMPILER_RUNTIME_MEMORY_LIMIT,
		cpuTimeLimitMs: options.cpuTimeLimitMs,
	});

	try {
		const compilerModuleSource =
			await filesystem.readTextFile(compilerModulePath);
		const result = await runtime.run<TResult>(
			buildCompilerRuntimeSource(
				request,
				compilerModulePath,
				compilerModuleSource,
			),
			COMPILER_RUNTIME_FILE_PATH,
		);
		if (result.code === 0 && result.exports) {
			return result.exports;
		}
		return createFailureResult<TResult>(request.kind, result.errorMessage);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return createFailureResult<TResult>(request.kind, message);
	} finally {
		runtime.dispose();
	}
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

function resolveCompilerModulePath(compilerSpecifier: string): string {
	if (compilerSpecifier === "typescript") {
		return "/root/node_modules/typescript/lib/typescript.js";
	}
	if (compilerSpecifier.startsWith("/")) {
		return compilerSpecifier;
	}
	if (
		compilerSpecifier.startsWith("./") ||
		compilerSpecifier.startsWith("../")
	) {
		return path.posix.resolve("/root", compilerSpecifier);
	}
	return `/root/node_modules/${compilerSpecifier}/lib/typescript.js`;
}

function buildCompilerRuntimeSource(
	request: CompilerRequest,
	compilerModulePath: string,
	compilerModuleSource: string,
): string {
	return `
const path = require("node:path");
const request = ${JSON.stringify(request)};
const compilerModulePath = ${JSON.stringify(compilerModulePath)};
const compilerModuleSource = ${JSON.stringify(compilerModuleSource)};
const compilerModule = { exports: {} };
const compilerFactory = new Function(
	"exports",
	"require",
	"module",
	"__filename",
	"__dirname",
	compilerModuleSource,
);
compilerFactory(
	compilerModule.exports,
	require,
	compilerModule,
	compilerModulePath,
	path.dirname(compilerModulePath),
);
module.exports = (${compilerRuntimeMain.toString()})(request, compilerModule.exports);
`;
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
