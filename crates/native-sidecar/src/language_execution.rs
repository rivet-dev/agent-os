//! First-class language execution admission and lifecycle.
//!
//! The public protocol names semantic operations. This module is the only
//! place that lowers them to the process transport used by the execution
//! engines; clients never construct runtime or package-manager commands.

use crate::protocol::*;
use crate::service::{normalize_path, DispatchResult, NativeSidecar, SidecarError};
use crate::state::{BridgeError, ExecutionValueKind, ManagedLanguageExecution};
use crate::NativeSidecarBridge;
use oxc_allocator::Allocator;
use oxc_ast::ast::{ImportDeclarationSpecifier, Statement};
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{Module, TransformOptions, Transformer};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const EXECUTION_EVENT_BYTES_LIMIT: usize = 2 * 1024 * 1024;
const MAX_EXECUTION_OUTPUT_PAGE_EVENTS: u32 = 1_000;
const DEFAULT_EXECUTION_OUTPUT_PAGE_EVENTS: u32 = 100;
const EXECUTION_CANCEL_GRACE_MS: u64 = 1_000;
const TTY_ENV: &str = "AGENTOS_EXEC_TTY";
const TTY_COLS_ENV: &str = "AGENTOS_EXEC_TTY_COLS";
const TTY_ROWS_ENV: &str = "AGENTOS_EXEC_TTY_ROWS";
const RETAIN_LANGUAGE_CONTEXT_ENV: &str = "AGENTOS_RETAIN_LANGUAGE_CONTEXT";
const INLINE_FILE_PATH_ENV: &str = "AGENTOS_INLINE_FILE_PATH";
const USE_BUNDLED_TYPESCRIPT_ENV: &str = "AGENTOS_USE_BUNDLED_TYPESCRIPT";
const SEMANTIC_RESULT_PATH_PREFIX: &str = "/tmp/.agentos-semantic-result-";

/// Monotonic suffix for minted public execution ids. Process-wide so ids stay
/// unique across every VM this sidecar hosts, not merely within one VM.
static NEXT_PUBLIC_EXECUTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct LoweredOperation {
    identity: ExecutionIdentityOptions,
    output: ExecutionOutputOptions,
    operation_id: Option<String>,
    background: bool,
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    stdin: Option<Vec<u8>>,
    pty: Option<ExecutionPtyOptions>,
    timeout_ms: Option<u64>,
    retained_language: Option<RetainedExecutionLanguage>,
    retained_source: Option<String>,
    retained_file_path: Option<String>,
    retained_module: bool,
    package_mutation: bool,
    value_kind: ExecutionValueKind,
    semantic_result_path: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn options(
    process: ProcessExecutionOptions,
) -> (
    ExecutionIdentityOptions,
    ExecutionOutputOptions,
    Option<String>,
    bool,
    Vec<String>,
    Option<String>,
    BTreeMap<String, String>,
    Option<Vec<u8>>,
    Option<ExecutionPtyOptions>,
    Option<u64>,
) {
    (
        process.identity,
        process.output,
        process.operation_id,
        process.background.unwrap_or(false),
        process.args,
        process.cwd,
        process.env.unwrap_or_default().into_iter().collect(),
        process.stdin,
        process.pty,
        process.timeout_ms,
    )
}

fn inline_inputs_prefix(inputs: Option<String>, python: bool) -> String {
    let inputs = inputs.unwrap_or_else(|| String::from("{}"));
    if python {
        format!(
            "import json as __agentos_json\ninputs = __agentos_json.loads({})\n",
            serde_json::to_string(&inputs).expect("JSON string serialization cannot fail")
        )
    } else {
        format!(
            "globalThis.inputs = Object.freeze(JSON.parse({}));\n",
            serde_json::to_string(&inputs).expect("JSON string serialization cannot fail")
        )
    }
}

fn semantic_result_path() -> String {
    static NEXT_RESULT_ID: AtomicU64 = AtomicU64::new(1);
    let nonce = NEXT_RESULT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{SEMANTIC_RESULT_PATH_PREFIX}{}-{nonce}.json", now_ms())
}

fn typescript_check_runner(request: serde_json::Value, result_path: &str) -> String {
    const RUNNER: &str = r#"
const __request = __AGENTOS_TYPESCRIPT_REQUEST__;
const __compilerPath = process.env.AGENTOS_TYPESCRIPT_COMPILER_PATH;
if (!__compilerPath) throw new Error("bundled TypeScript compiler path is unavailable");
const ts = require(__compilerPath);
const path = require("node:path");

const diagnostic = (item) => {
  const result = {
    code: item.code,
    category: item.category === ts.DiagnosticCategory.Warning
      ? "warning"
      : item.category === ts.DiagnosticCategory.Suggestion
        ? "suggestion"
        : item.category === ts.DiagnosticCategory.Message
          ? "message"
          : "error",
    message: ts.flattenDiagnosticMessageText(item.messageText, "\n").trim(),
  };
  if (item.file && item.start !== undefined) {
    const location = item.file.getLineAndCharacterOfPosition(item.start);
    result.filePath = item.file.fileName.replace(/\\/g, "/");
    result.line = location.line + 1;
    result.column = location.character + 1;
  }
  return result;
};

const cwd = path.resolve(__request.cwd || process.cwd());
let diagnostics;
if (__request.kind === "project") {
  const configPath = __request.tsconfigPath
    ? path.resolve(cwd, __request.tsconfigPath)
    : ts.findConfigFile(cwd, ts.sys.fileExists, "tsconfig.json");
  if (!configPath) throw new Error(`Unable to find tsconfig.json from '${cwd}'`);
  const config = ts.readConfigFile(configPath, ts.sys.readFile);
  if (config.error) {
    diagnostics = [config.error];
  } else {
    const parsed = ts.parseJsonConfigFileContent(
      config.config,
      ts.sys,
      path.dirname(configPath),
      { noEmit: true },
      configPath,
    );
    const program = ts.createProgram({
      rootNames: parsed.fileNames,
      options: parsed.options,
      projectReferences: parsed.projectReferences,
    });
    diagnostics = [...parsed.errors, ...ts.getPreEmitDiagnostics(program)];
  }
} else {
  const filePath = path.resolve(cwd, __request.filePath || "agentos-inline.ts");
  let projectOptions = {};
  let configDiagnostics = [];
  if (__request.tsconfigPath) {
    const configPath = path.resolve(cwd, __request.tsconfigPath);
    const config = ts.readConfigFile(configPath, ts.sys.readFile);
    if (config.error) {
      configDiagnostics = [config.error];
    } else {
      const parsed = ts.parseJsonConfigFileContent(
        config.config,
        ts.sys,
        path.dirname(configPath),
        {},
        configPath,
      );
      projectOptions = parsed.options;
      configDiagnostics = parsed.errors;
    }
  }
  const converted = ts.convertCompilerOptionsFromJson(
    __request.compilerOptions || {},
    cwd,
  );
  const compilerOptions = {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.CommonJS,
    ...projectOptions,
    ...converted.options,
    noEmit: true,
  };
  const host = ts.createCompilerHost(compilerOptions);
  const normalizedFilePath = ts.sys.useCaseSensitiveFileNames
    ? filePath
    : filePath.toLowerCase();
  const originalFileExists = host.fileExists.bind(host);
  const originalReadFile = host.readFile.bind(host);
  const originalGetSourceFile = host.getSourceFile.bind(host);
  const normalize = (candidate) => ts.sys.useCaseSensitiveFileNames
    ? candidate
    : candidate.toLowerCase();
  host.fileExists = (candidate) =>
    normalize(candidate) === normalizedFilePath || originalFileExists(candidate);
  host.readFile = (candidate) =>
    normalize(candidate) === normalizedFilePath
      ? __request.source
      : originalReadFile(candidate);
  host.getSourceFile = (candidate, languageVersion, onError, fresh) =>
    normalize(candidate) === normalizedFilePath
      ? ts.createSourceFile(candidate, __request.source, languageVersion, true)
      : originalGetSourceFile(candidate, languageVersion, onError, fresh);
  const program = ts.createProgram([filePath], compilerOptions, host);
  diagnostics = [
    ...configDiagnostics,
    ...converted.errors,
    ...ts.getPreEmitDiagnostics(program),
  ];
}

const result = ts.sortAndDeduplicateDiagnostics(diagnostics).map(diagnostic);
require("node:fs").writeFileSync(
  __AGENTOS_TYPESCRIPT_RESULT_PATH__,
  JSON.stringify({
    hasErrors: result.some((item) => item.category === "error"),
    diagnostics: result,
  }),
);
"#;
    format!("(async () => {{\n{}\n}})()", RUNNER)
        .replace(
            "__AGENTOS_TYPESCRIPT_REQUEST__",
            &serde_json::to_string(&request).expect("TypeScript request serialization cannot fail"),
        )
        .replace(
            "__AGENTOS_TYPESCRIPT_RESULT_PATH__",
            &serde_json::to_string(result_path)
                .expect("TypeScript result path serialization cannot fail"),
        )
}

fn transform_source(
    source: &str,
    file_path: &str,
    typescript: bool,
    common_js: bool,
) -> Result<String, SidecarError> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(Path::new(file_path))
        .unwrap_or_default()
        .with_typescript(typescript)
        .with_module(true);
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        let message = parsed
            .errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let language = if typescript {
            "TypeScript"
        } else {
            "JavaScript"
        };
        return Err(SidecarError::InvalidState(format!(
            "{language} syntax error in {file_path}: {message}"
        )));
    }

    let mut program = parsed.program;
    let semantic = SemanticBuilder::new()
        .with_excess_capacity(2.0)
        .build(&program);
    if !semantic.errors.is_empty() {
        let message = semantic
            .errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let language = if typescript {
            "TypeScript"
        } else {
            "JavaScript"
        };
        return Err(SidecarError::InvalidState(format!(
            "{language} semantic transform error in {file_path}: {message}"
        )));
    }
    let mut transform_options = TransformOptions::default();
    if common_js {
        transform_options.env.module = Module::CommonJS;
    }
    let result = Transformer::new(&allocator, Path::new(file_path), &transform_options)
        .build_with_scoping(semantic.semantic.into_scoping(), &mut program);
    if !result.errors.is_empty() {
        let message = result
            .errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let language = if typescript {
            "TypeScript"
        } else {
            "JavaScript"
        };
        return Err(SidecarError::InvalidState(format!(
            "{language} transpilation failed for {file_path}: {message}"
        )));
    }
    Ok(Codegen::new().build(&program).code)
}

fn transpile_typescript(
    source: &str,
    file_path: &str,
    common_js: bool,
) -> Result<String, SidecarError> {
    transform_source(source, file_path, true, common_js)
}

fn transform_retained_javascript_module(
    source: &str,
    file_path: &str,
) -> Result<String, SidecarError> {
    let source = rewrite_static_imports(source, file_path, false)?;
    transform_source(&source, file_path, false, true)
}

fn transform_retained_typescript_module(
    source: &str,
    file_path: &str,
) -> Result<String, SidecarError> {
    let source = rewrite_static_imports(source, file_path, true)?;
    transform_source(&source, file_path, true, true)
}

/// Retained cells execute as scripts so their lexical declarations remain in
/// the context's shared script environment. Rewrite only static imports into
/// equivalent `require` declarations before the normal OXC transform; this
/// keeps the caller's local import names as real retained lexical bindings.
fn rewrite_static_imports(
    source: &str,
    file_path: &str,
    typescript: bool,
) -> Result<String, SidecarError> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(Path::new(file_path))
        .unwrap_or_default()
        .with_typescript(typescript)
        .with_module(true);
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        let language = if typescript {
            "TypeScript"
        } else {
            "JavaScript"
        };
        let message = parsed
            .errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(SidecarError::InvalidState(format!(
            "{language} syntax error in {file_path}: {message}"
        )));
    }

    let mut replacements = Vec::new();
    for statement in &parsed.program.body {
        let Statement::ImportDeclaration(declaration) = statement else {
            continue;
        };
        let replacement = if declaration.import_kind.is_type() {
            String::new()
        } else {
            let source_literal = serde_json::to_string(declaration.source.value.as_str())
                .expect("module specifier serialization cannot fail");
            let mut declarations = Vec::new();
            match declaration.specifiers.as_deref() {
                None => declarations.push(format!("require({source_literal});")),
                Some(specifiers) if specifiers.is_empty() => {
                    declarations.push(format!("require({source_literal});"));
                }
                Some(specifiers) => {
                    for specifier in specifiers {
                        match specifier {
                            ImportDeclarationSpecifier::ImportSpecifier(specifier) => {
                                if specifier.import_kind.is_type() {
                                    continue;
                                }
                                let imported =
                                    serde_json::to_string(specifier.imported.name().as_str())
                                        .expect("import name serialization cannot fail");
                                declarations.push(format!(
                                    "const {} = require({source_literal})[{imported}];",
                                    specifier.local.name
                                ));
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) => {
                                declarations.push(format!(
                                    "const {} = (() => {{ const value = require({source_literal}); return value && value.__esModule ? value.default : value; }})();",
                                    specifier.local.name
                                ));
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(specifier) => {
                                declarations.push(format!(
                                    "const {} = require({source_literal});",
                                    specifier.local.name
                                ));
                            }
                        }
                    }
                }
            }
            declarations.join("\n")
        };
        replacements.push((
            declaration.span.start as usize,
            declaration.span.end as usize,
            replacement,
        ));
    }

    if replacements.is_empty() {
        return Ok(source.to_owned());
    }
    let mut rewritten = source.to_owned();
    for (start, end, replacement) in replacements.into_iter().rev() {
        rewritten.replace_range(start..end, &replacement);
    }
    Ok(rewritten)
}

fn lowered_process(
    process: ProcessExecutionOptions,
    command: impl Into<String>,
    mut prefix_args: Vec<String>,
) -> LoweredOperation {
    let (identity, output, operation_id, background, args, cwd, env, stdin, pty, timeout_ms) =
        options(process);
    prefix_args.extend(args);
    LoweredOperation {
        identity,
        output,
        operation_id,
        background,
        command: command.into(),
        args: prefix_args,
        cwd,
        env,
        stdin,
        pty,
        timeout_ms,
        retained_language: None,
        retained_source: None,
        retained_file_path: None,
        retained_module: false,
        package_mutation: false,
        value_kind: ExecutionValueKind::None,
        semantic_result_path: None,
    }
}

fn lowered_install(
    identity: ExecutionIdentityOptions,
    output: ExecutionOutputOptions,
    cwd: Option<String>,
    env: Option<std::collections::HashMap<String, String>>,
    timeout_ms: Option<u64>,
    command: impl Into<String>,
    args: Vec<String>,
) -> LoweredOperation {
    LoweredOperation {
        identity,
        output,
        operation_id: None,
        background: false,
        command: command.into(),
        args,
        cwd,
        env: env.unwrap_or_default().into_iter().collect(),
        stdin: None,
        pty: None,
        timeout_ms,
        retained_language: None,
        retained_source: None,
        retained_file_path: None,
        retained_module: false,
        package_mutation: false,
        value_kind: ExecutionValueKind::None,
        semantic_result_path: None,
    }
}

fn lower_operation(payload: RequestPayload) -> Result<LoweredOperation, SidecarError> {
    let lowered = match payload {
        RequestPayload::ShellExecution(payload) => lowered_process(
            payload.process,
            "sh",
            vec![String::from("-c"), payload.command],
        ),
        RequestPayload::ArgvExecution(payload) => {
            lowered_process(payload.process, payload.command, Vec::new())
        }
        RequestPayload::JavaScriptExecution(payload) => {
            let file_path = payload
                .file_path
                .unwrap_or_else(|| String::from("/[agentos-inline.js]"));
            let module = payload.format == Some(JavaScriptModuleFormat::Module);
            let mut source = inline_inputs_prefix(payload.inputs, false);
            source.push_str(&payload.source);
            if module {
                source = transform_retained_javascript_module(&source, &file_path)?;
            }
            let retained_source = source.clone();
            let mut operation =
                lowered_process(payload.process, "node", vec![String::from("-e"), source]);
            operation.retained_language = Some(RetainedExecutionLanguage::JavaScript);
            operation.retained_source = Some(retained_source);
            operation.retained_file_path = Some(file_path.clone());
            operation.retained_module = false;
            operation
                .env
                .insert(String::from(INLINE_FILE_PATH_ENV), file_path);
            operation
        }
        RequestPayload::JavaScriptEvaluation(payload) => {
            let file_path = payload
                .file_path
                .unwrap_or_else(|| String::from("/[agentos-evaluation.js]"));
            let module = payload.format == Some(JavaScriptModuleFormat::Module);
            let result_path = semantic_result_path();
            let mut source = inline_inputs_prefix(payload.inputs, false);
            source.push_str(&format!(
                "Promise.resolve((async () => ({}))()).then((value) => {{ let result; try {{ if (value === undefined || typeof value === 'function' || typeof value === 'symbol') throw new TypeError('undefined, functions, and symbols are not supported'); result = JSON.stringify({{ __agentosEvaluation: true, ok: true, value }}); }} catch (error) {{ result = JSON.stringify({{ __agentosEvaluation: true, ok: false, error: `AgentOS evaluation result must be JSON-serializable: ${{error instanceof Error ? error.message : String(error)}}` }}); }} require('node:fs').writeFileSync({}, result); }});",
                payload.expression,
                serde_json::to_string(&result_path)
                    .expect("semantic result path serialization cannot fail")
            ));
            if module {
                source = transform_retained_javascript_module(&source, &file_path)?;
            }
            let mut operation =
                lowered_process(payload.process, "node", vec![String::from("-e"), source]);
            operation.retained_language = Some(RetainedExecutionLanguage::JavaScript);
            operation.retained_source = operation.args.get(1).cloned();
            operation.retained_file_path = Some(file_path.clone());
            operation.retained_module = false;
            operation
                .env
                .insert(String::from(INLINE_FILE_PATH_ENV), file_path);
            operation.value_kind = ExecutionValueKind::JavaScript;
            operation.semantic_result_path = Some(result_path);
            operation
        }
        RequestPayload::JavaScriptFileExecution(payload) => {
            lowered_process(payload.process, "node", vec![payload.path])
        }
        RequestPayload::TypeScriptExecution(payload) => {
            let file_path = payload
                .file_path
                .unwrap_or_else(|| String::from("agentos-inline.ts"));
            let mut source = inline_inputs_prefix(payload.inputs, false);
            source.push_str(&payload.source);
            let source = transform_retained_typescript_module(&source, &file_path)?;
            let mut operation =
                lowered_process(payload.process, "node", vec![String::from("-e"), source]);
            operation.retained_language = Some(RetainedExecutionLanguage::JavaScript);
            operation.retained_source = operation.args.get(1).cloned();
            operation.retained_file_path = Some(file_path.clone());
            operation
                .env
                .insert(String::from(INLINE_FILE_PATH_ENV), file_path);
            operation
        }
        RequestPayload::TypeScriptEvaluation(payload) => {
            let result_path = semantic_result_path();
            let file_path = payload
                .file_path
                .unwrap_or_else(|| String::from("agentos-evaluation.ts"));
            let mut source = inline_inputs_prefix(payload.inputs, false);
            source.push_str(&format!(
                "Promise.resolve((async () => ({}))()).then((value) => {{ let result; try {{ if (value === undefined || typeof value === 'function' || typeof value === 'symbol') throw new TypeError('undefined, functions, and symbols are not supported'); result = JSON.stringify({{ __agentosEvaluation: true, ok: true, value }}); }} catch (error) {{ result = JSON.stringify({{ __agentosEvaluation: true, ok: false, error: `AgentOS evaluation result must be JSON-serializable: ${{error instanceof Error ? error.message : String(error)}}` }}); }} require('node:fs').writeFileSync({}, result); }});",
                payload.expression,
                serde_json::to_string(&result_path)
                    .expect("semantic result path serialization cannot fail")
            ));
            let source = transform_retained_typescript_module(&source, &file_path)?;
            let mut operation =
                lowered_process(payload.process, "node", vec![String::from("-e"), source]);
            operation.retained_language = Some(RetainedExecutionLanguage::JavaScript);
            operation.retained_source = operation.args.get(1).cloned();
            operation.retained_file_path = Some(file_path.clone());
            operation
                .env
                .insert(String::from(INLINE_FILE_PATH_ENV), file_path);
            operation.value_kind = ExecutionValueKind::JavaScript;
            operation.semantic_result_path = Some(result_path);
            operation
        }
        RequestPayload::TypeScriptFileExecution(payload) => lowered_process(
            payload.process,
            "__agentos_typescript_file",
            vec![payload.path],
        ),
        RequestPayload::TypeScriptCheck(payload) => {
            let result_path = semantic_result_path();
            let request = serde_json::json!({
                "kind": "source",
                "source": payload.source,
                "cwd": payload.cwd,
                "filePath": payload.file_path,
                "tsconfigPath": payload.tsconfig_path,
                "compilerOptions": payload
                    .compiler_options
                    .as_deref()
                    .map(serde_json::from_str::<serde_json::Value>)
                    .transpose()
                    .map_err(|error| SidecarError::InvalidState(format!("invalid TypeScript compiler options: {error}")))?,
            });
            let mut operation = lowered_install(
                payload.identity,
                payload.output,
                request["cwd"].as_str().map(str::to_owned),
                None,
                payload.timeout_ms,
                "node",
                vec![
                    String::from("-e"),
                    typescript_check_runner(request, &result_path),
                ],
            );
            operation
                .env
                .insert(String::from(USE_BUNDLED_TYPESCRIPT_ENV), String::from("1"));
            operation.value_kind = ExecutionValueKind::TypeScriptCheck;
            operation.semantic_result_path = Some(result_path);
            operation
        }
        RequestPayload::TypeScriptProjectCheck(payload) => {
            let result_path = semantic_result_path();
            let cwd = payload.cwd.clone();
            let request = serde_json::json!({
                "kind": "project",
                "cwd": payload.cwd,
                "tsconfigPath": payload.tsconfig_path,
            });
            let mut operation = lowered_install(
                payload.identity,
                payload.output,
                cwd,
                None,
                payload.timeout_ms,
                "node",
                vec![
                    String::from("-e"),
                    typescript_check_runner(request, &result_path),
                ],
            );
            operation
                .env
                .insert(String::from(USE_BUNDLED_TYPESCRIPT_ENV), String::from("1"));
            operation.value_kind = ExecutionValueKind::TypeScriptCheck;
            operation.semantic_result_path = Some(result_path);
            operation
        }
        RequestPayload::NpmProjectInstall(payload) => {
            let args = if payload.frozen.unwrap_or(false) {
                vec![String::from("ci")]
            } else {
                vec![String::from("install")]
            };
            let mut operation = lowered_install(
                payload.identity,
                payload.output,
                payload.cwd,
                payload.env,
                payload.timeout_ms,
                "npm",
                args,
            );
            operation.package_mutation = true;
            operation
        }
        RequestPayload::NpmPackageInstall(payload) => {
            let mut args = vec![String::from("install")];
            if payload.dev.unwrap_or(false) {
                args.push(String::from("--save-dev"));
            }
            if payload.global.unwrap_or(false) {
                args.push(String::from("--global"));
            }
            args.extend(payload.packages);
            let mut operation = lowered_install(
                payload.identity,
                payload.output,
                payload.cwd,
                payload.env,
                payload.timeout_ms,
                "npm",
                args,
            );
            operation.package_mutation = true;
            operation
        }
        RequestPayload::NpmScriptExecution(payload) => {
            let script = payload.script;
            lowered_process(
                payload.process,
                "npm",
                vec![String::from("run"), script, String::from("--")],
            )
        }
        RequestPayload::NpmPackageExecution(payload) => {
            let mut args = vec![
                String::from("exec"),
                String::from("--package"),
                payload.package_spec,
            ];
            if let Some(binary) = payload.binary {
                args.extend([String::from("--"), binary]);
            }
            lowered_process(payload.process, "npm", args)
        }
        RequestPayload::PythonExecution(payload) => {
            let mut source = inline_inputs_prefix(payload.inputs, true);
            source.push_str(&payload.source);
            let mut operation =
                lowered_process(payload.process, "python", vec![String::from("-c"), source]);
            operation.retained_language = Some(RetainedExecutionLanguage::Python);
            operation.retained_source = operation.args.get(1).cloned();
            operation
        }
        RequestPayload::PythonEvaluation(payload) => {
            let result_path = semantic_result_path();
            let mut source = inline_inputs_prefix(payload.inputs, true);
            source.push_str(&format!(
                "\n__agentos_value = ({})\ntry:\n    __agentos_result = __agentos_json.dumps({{\"__agentosEvaluation\": True, \"ok\": True, \"value\": __agentos_value}}, allow_nan=False)\nexcept Exception as __agentos_error:\n    __agentos_result = __agentos_json.dumps({{\"__agentosEvaluation\": True, \"ok\": False, \"error\": \"AgentOS evaluation result must be JSON-serializable: \" + str(__agentos_error)}})\nprint({} + __agentos_result)\n",
                payload.expression,
                serde_json::to_string(&result_path)
                    .expect("semantic result path serialization cannot fail"),
            ));
            let mut operation =
                lowered_process(payload.process, "python", vec![String::from("-c"), source]);
            operation.retained_language = Some(RetainedExecutionLanguage::Python);
            operation.retained_source = operation.args.get(1).cloned();
            operation.value_kind = ExecutionValueKind::Python;
            operation.semantic_result_path = Some(result_path);
            operation
        }
        RequestPayload::PythonFileExecution(payload) => {
            lowered_process(payload.process, "python", vec![payload.path])
        }
        RequestPayload::PythonModuleExecution(payload) => lowered_process(
            payload.process,
            "python",
            vec![String::from("-m"), payload.module],
        ),
        RequestPayload::PythonInstall(payload) => {
            if !payload.packages.is_empty() && payload.requirements_file.is_some() {
                return Err(SidecarError::InvalidState(String::from(
                    "installPythonPackages cannot combine packages with requirementsFile",
                )));
            }
            let mut args = vec![
                String::from("-m"),
                String::from("pip"),
                String::from("install"),
            ];
            if payload.upgrade.unwrap_or(false) {
                args.push(String::from("--upgrade"));
            }
            if let Some(path) = payload.requirements_file {
                args.extend([String::from("--requirement"), path]);
            }
            if let Some(url) = payload.index_url {
                args.extend([String::from("--index-url"), url]);
            }
            for url in payload.extra_index_urls {
                args.extend([String::from("--extra-index-url"), url]);
            }
            args.extend(payload.packages);
            let mut operation = lowered_install(
                payload.identity,
                payload.output,
                payload.cwd,
                payload.env,
                payload.timeout_ms,
                "python",
                args,
            );
            operation.package_mutation = true;
            operation
        }
        _ => {
            return Err(SidecarError::InvalidState(String::from(
                "request is not a language execution operation",
            )))
        }
    };
    Ok(lowered)
}

fn typed_rejection(request: &RequestFrame, code: &str, message: impl AsRef<str>) -> DispatchResult {
    DispatchResult {
        response: agentos_native_sidecar_core::reject(request, code, message.as_ref()),
        events: Vec::new(),
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn execute_language_operation(
        &mut self,
        request: &RequestFrame,
        payload: RequestPayload,
    ) -> Result<DispatchResult, SidecarError> {
        // The caller deadline begins before source transformation, guest-file
        // staging, and compiler staging. The remaining budget is handed to the
        // runtime after those sidecar-owned phases finish.
        let operation_started_at_ms = now_ms();
        let mut operation = match lower_operation(payload) {
            Ok(operation) => operation,
            Err(error) => {
                return Ok(typed_rejection(
                    request,
                    "invalid_execution_request",
                    error.to_string(),
                ));
            }
        };
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        let context_execution = operation.identity.context_id.is_some();
        if operation.background && context_execution {
            return Ok(typed_rejection(
                request,
                "invalid_execution_identity",
                "spawned language processes cannot use contextId",
            ));
        }
        if operation.background && operation.operation_id.is_none() {
            return Ok(typed_rejection(
                request,
                "invalid_execution_identity",
                "spawned language process requires an internal operationId",
            ));
        }
        if !operation.background && operation.operation_id.is_some() {
            return Ok(typed_rejection(
                request,
                "invalid_execution_identity",
                "operationId is reserved for spawned language processes",
            ));
        }
        if operation.output.retain_events == Some(true)
            && !context_execution
            && !operation.background
        {
            return Ok(typed_rejection(
                request,
                "invalid_execution_identity",
                "retainEvents requires a context or spawned language process",
            ));
        }
        if !context_execution {
            // Ephemeral calls must not park a language interpreter after the
            // operation completes.
            operation.retained_language = None;
            operation.retained_source = None;
            operation.retained_file_path = None;
            operation.retained_module = false;
        }

        if operation.command == "__agentos_typescript_file" {
            let requested_path = operation.args.first().cloned().ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "executeTypeScriptFile requires a file path",
                ))
            })?;
            let vm = self
                .vms
                .get_mut(&vm_id)
                .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
            let guest_path = if requested_path.starts_with('/') {
                normalize_path(&requested_path)
            } else {
                let cwd = operation.cwd.as_deref().unwrap_or(&vm.guest_cwd);
                normalize_path(&format!("{}/{requested_path}", cwd.trim_end_matches('/')))
            };
            let source = vm.kernel.read_file(&guest_path).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "failed to read TypeScript file {guest_path}: {error}"
                ))
            })?;
            let source = String::from_utf8(source).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "TypeScript file {guest_path} is not UTF-8: {error}"
                ))
            })?;
            operation.command = String::from("node");
            operation.args = vec![
                String::from("-e"),
                transpile_typescript(&source, &guest_path, false)?,
            ];
            operation.env.insert(
                String::from("AGENTOS_GUEST_ENTRYPOINT_MODULE_MODE"),
                String::from("1"),
            );
            operation
                .env
                .insert(String::from(INLINE_FILE_PATH_ENV), guest_path);
        }

        if operation
            .env
            .get(USE_BUNDLED_TYPESCRIPT_ENV)
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        {
            const COMPILER_ROOT: &str = "/.agentos/runtime/typescript";
            const COMPILER_PATH: &str = "/.agentos/runtime/typescript/typescript.js";
            let vm = self
                .vms
                .get_mut(&vm_id)
                .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
            if !vm.typescript_compiler_staged {
                let assets = agentos_execution::bundled_typescript_assets();
                if assets.is_empty() {
                    return Err(SidecarError::InvalidState(String::from(
                        "bundled TypeScript compiler is unavailable in this build",
                    )));
                }
                vm.kernel.mkdir(COMPILER_ROOT, true).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "failed to create TypeScript compiler runtime directory: {error}"
                    ))
                })?;
                for (file_name, bytes) in assets {
                    vm.kernel
                        .write_file(&format!("{COMPILER_ROOT}/{file_name}"), bytes.to_vec())
                        .map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "failed to stage TypeScript compiler asset {file_name}: {error}"
                            ))
                        })?;
                }
                vm.typescript_compiler_staged = true;
            }
            operation.env.insert(
                String::from("AGENTOS_TYPESCRIPT_COMPILER_PATH"),
                String::from(COMPILER_PATH),
            );
        }

        let now = now_ms();
        if let Some(timeout_ms) = operation.timeout_ms {
            let deadline_ms = operation_started_at_ms.saturating_add(timeout_ms);
            operation.timeout_ms = Some(deadline_ms.saturating_sub(now).max(1));
        }
        let (output_limit_bytes, output_limit_setting) = {
            let vm = self
                .vms
                .get(&vm_id)
                .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
            match operation.command.as_str() {
                "node" | "npm" | "npx" | "__agentos_typescript_file" => (
                    vm.limits.js_runtime.captured_output_limit_bytes,
                    "limits.jsRuntime.capturedOutputLimitBytes",
                ),
                "python" | "python3" | "pip" | "pip3" => (
                    vm.limits.python.output_buffer_max_bytes,
                    "limits.python.outputBufferMaxBytes",
                ),
                _ => (
                    vm.limits.wasm.captured_output_limit_bytes,
                    "limits.wasm.capturedOutputLimitBytes",
                ),
            }
        };
        let execution_id = {
            let vm = self
                .vms
                .get_mut(&vm_id)
                .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
            if operation.package_mutation {
                let active_mutation = vm
                    .package_mutation_execution_id
                    .as_ref()
                    .filter(|execution_id| {
                        vm.executions.get(*execution_id).is_some_and(|execution| {
                            execution.descriptor.state == ExecutionState::Running
                        })
                    })
                    .cloned();
                if let Some(active_mutation) = active_mutation {
                    return Ok(typed_rejection(
                        request,
                        "execution_busy",
                        format!(
                            "package mutation execution {active_mutation} is already running in this VM; package installs are serialized at VM scope"
                        ),
                    ));
                }
                vm.package_mutation_execution_id = None;
            }
            if operation.background {
                operation
                    .operation_id
                    .take()
                    .expect("background operation id validated above")
            } else {
                match operation.identity.context_id.take() {
                    Some(execution_id) => {
                        if let Some(existing) = vm.executions.get(&execution_id) {
                            if !existing.context {
                                return Ok(typed_rejection(
                                    request,
                                    "context_not_found",
                                    format!("context {execution_id} does not exist"),
                                ));
                            }
                            if existing.descriptor.state == ExecutionState::Running {
                                return Ok(typed_rejection(
                                    request,
                                    "execution_busy",
                                    format!(
                                        "execution {execution_id} already has an active operation"
                                    ),
                                ));
                            }
                            if existing.descriptor.state == ExecutionState::Failed {
                                return Ok(typed_rejection(
                                    request,
                                    "execution_failed",
                                    format!("execution {execution_id} must be reset or deleted"),
                                ));
                            }
                            if let (Some(existing), Some(requested)) = (
                                existing.descriptor.retained_language.as_ref(),
                                operation.retained_language.as_ref(),
                            ) {
                                if existing != requested {
                                    return Ok(typed_rejection(
                                        request,
                                        "context_language_mismatch",
                                        format!(
                                            "execution {execution_id} is retained for {existing:?}"
                                        ),
                                    ));
                                }
                            }
                        } else {
                            return Ok(typed_rejection(
                                request,
                                "context_not_found",
                                format!("context {execution_id} does not exist"),
                            ));
                        }
                        execution_id
                    }
                    None => loop {
                        // Process-wide, not per-VM. One sidecar process fans out to
                        // many VMs whose events reach the host over a single shared
                        // client that routes execution events by id alone, so a
                        // per-VM counter makes K VMs mint the identical
                        // `operation-{ms}-1` in the same millisecond and cross-talk.
                        let candidate = format!(
                            "operation-{now:x}-{:x}",
                            NEXT_PUBLIC_EXECUTION_ID.fetch_add(1, Ordering::Relaxed)
                        );
                        if !vm.executions.contains_key(&candidate) {
                            break candidate;
                        }
                    },
                }
            }
        };

        if let Some(vm) = self.vms.get(&vm_id) {
            let live_count = vm
                .executions
                .values()
                .filter(|item| item.descriptor.state == ExecutionState::Running)
                .count()
                .saturating_add(1);
            let warning_threshold = vm.limits.execution.live_execution_warning_threshold;
            if live_count == warning_threshold {
                eprintln!(
                    "agentos VM {vm_id} reached limits.execution.liveExecutionWarningThreshold ({warning_threshold}) with {live_count} live executions"
                );
            }
        }

        let (process_id, generation, descriptor, reused_resident) = {
            let vm = self.vms.get_mut(&vm_id).expect("owned VM checked above");
            let resident_process_id = operation
                .retained_source
                .as_ref()
                .and_then(|_| vm.executions.get(&execution_id))
                .and_then(|execution| execution.resident_process_id.clone())
                .filter(|process_id| vm.active_processes.contains_key(process_id));
            let resident_pid = resident_process_id
                .as_ref()
                .and_then(|process_id| vm.active_processes.get(process_id))
                .map(|process| process.kernel_pid);
            let reused_resident = resident_process_id.is_some();
            let execution = vm
                .executions
                .entry(execution_id.clone())
                .or_insert_with(|| ManagedLanguageExecution {
                    public: context_execution || operation.background,
                    context: context_execution,
                    descriptor: ExecutionDescriptor {
                        execution_id: execution_id.clone(),
                        generation: 0,
                        state: ExecutionState::Creating,
                        retained_language: None,
                        process_id: None,
                        pid: None,
                        created_at_ms: now,
                        last_started_at_ms: None,
                        last_completed_at_ms: None,
                        last_outcome: None,
                        last_exit_code: None,
                    },
                    result: None,
                    events: VecDeque::new(),
                    retained_event_bytes: 0,
                    output_truncated: false,
                    next_sequence: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    output_limit_bytes,
                    output_limit_setting,
                    capture: operation
                        .output
                        .capture
                        .clone()
                        .unwrap_or(ExecutionOutputCapture::None),
                    retain_events: operation.output.retain_events.unwrap_or(false),
                    event_limit: self.config.runtime.protocol.max_process_events.max(1),
                    event_bytes_limit: EXECUTION_EVENT_BYTES_LIMIT,
                    uses_pty: false,
                    value_kind: ExecutionValueKind::None,
                    semantic_result_path: None,
                    pending_outcome: None,
                    deadline_ms: None,
                    expires_at_ms: None,
                    deadline_task: None,
                    resident_process_id: None,
                });
            if let Some(task) = execution.deadline_task.take() {
                task.abort();
            }
            execution.descriptor.generation = execution.descriptor.generation.saturating_add(1);
            execution.descriptor.state = ExecutionState::Running;
            execution.descriptor.retained_language = execution
                .descriptor
                .retained_language
                .clone()
                .or(operation.retained_language.clone());
            execution.descriptor.last_started_at_ms = Some(now);
            execution.descriptor.last_completed_at_ms = None;
            execution.descriptor.last_outcome = None;
            execution.descriptor.last_exit_code = None;
            execution.result = None;
            execution.events.clear();
            execution.retained_event_bytes = 0;
            execution.output_truncated = false;
            execution.next_sequence = 0;
            execution.stdout.clear();
            execution.stderr.clear();
            execution.stdout_truncated = false;
            execution.stderr_truncated = false;
            execution.output_limit_bytes = output_limit_bytes;
            execution.output_limit_setting = output_limit_setting;
            execution.capture = operation
                .output
                .capture
                .clone()
                .unwrap_or(ExecutionOutputCapture::None);
            execution.retain_events = operation.output.retain_events.unwrap_or(false);
            execution.uses_pty = operation.pty.is_some();
            execution.value_kind = operation.value_kind;
            execution.semantic_result_path = operation.semantic_result_path.clone();
            execution.pending_outcome = None;
            execution.expires_at_ms = None;
            execution.deadline_ms = operation
                .timeout_ms
                .map(|timeout| now.saturating_add(timeout));
            let generation = execution.descriptor.generation;
            let process_id = resident_process_id
                .unwrap_or_else(|| format!("execution:{execution_id}:{generation}"));
            execution.descriptor.process_id = Some(process_id.clone());
            execution.descriptor.pid = resident_pid;
            if operation.retained_source.is_some() {
                execution.resident_process_id = Some(process_id.clone());
            }
            vm.execution_processes
                .insert(process_id.clone(), execution_id.clone());
            if operation.package_mutation {
                vm.package_mutation_execution_id = Some(execution_id.clone());
            }
            (
                process_id,
                generation,
                execution.descriptor.clone(),
                reused_resident,
            )
        };

        if let Some(timeout_ms) = operation.timeout_ms {
            let notify = Arc::clone(&self.process_event_notify);
            let runtime = self
                .vms
                .get(&vm_id)
                .expect("owned VM checked above")
                .runtime_context
                .clone();
            let task = runtime
                .spawn(agentos_runtime::TaskClass::Timer, async move {
                    tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
                    notify.notify_one();
                })
                .map_err(|error| SidecarError::Execution(error.to_string()))?;
            self.vms
                .get_mut(&vm_id)
                .and_then(|vm| vm.executions.get_mut(&execution_id))
                .expect("admitted execution exists")
                .deadline_task = Some(task);
        }

        if let Some(pty) = &operation.pty {
            operation
                .env
                .insert(String::from(TTY_ENV), String::from("1"));
            if let Some(cols) = pty.cols {
                operation
                    .env
                    .insert(String::from(TTY_COLS_ENV), cols.to_string());
            }
            if let Some(rows) = pty.rows {
                operation
                    .env
                    .insert(String::from(TTY_ROWS_ENV), rows.to_string());
            }
        }
        if let Some(timeout_ms) = operation.timeout_ms {
            if matches!(operation.command.as_str(), "node" | "npm" | "npx") {
                operation.env.insert(
                    String::from("AGENTOS_V8_WALL_CLOCK_LIMIT_MS"),
                    timeout_ms.to_string(),
                );
            } else if matches!(
                operation.command.as_str(),
                "python" | "python3" | "pip" | "pip3"
            ) {
                operation.env.insert(
                    String::from("AGENTOS_PYTHON_EXECUTION_TIMEOUT_MS"),
                    timeout_ms.to_string(),
                );
            }
        }
        if operation.retained_source.is_some() {
            operation
                .env
                .insert(String::from(RETAIN_LANGUAGE_CONTEXT_ENV), String::from("1"));
        }
        let execute_payload = ExecuteRequest {
            process_id: process_id.clone(),
            command: Some(operation.command),
            runtime: None,
            entrypoint: None,
            args: operation.args,
            env: operation.env.into_iter().collect(),
            cwd: operation.cwd,
            wasm_permission_tier: None,
        };
        let launch_result = if reused_resident {
            let language = operation
                .retained_language
                .clone()
                .expect("resident operations have a retained language");
            let source = operation
                .retained_source
                .clone()
                .expect("resident operations have retained source");
            let file_path = operation
                .retained_file_path
                .clone()
                .unwrap_or_else(|| String::from("/[agentos-retained]"));
            let vm = self.vms.get_mut(&vm_id).expect("owned VM checked above");
            let process = vm.active_processes.get_mut(&process_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "resident process {process_id} disappeared before execution"
                ))
            })?;
            process
                .execution
                .execute_retained_language(language, source, file_path, operation.retained_module)
                .map(|()| None)
        } else {
            self.execute(request, execute_payload).await.map(Some)
        };
        let launch = match launch_result {
            Ok(result) => result,
            Err(error) => {
                if reused_resident {
                    let _ = self.finish_active_process_exit(&vm_id, &process_id, 1);
                }
                if let Some(vm) = self.vms.get_mut(&vm_id) {
                    let completed_ttl_ms = vm.limits.execution.completed_ttl_ms;
                    vm.execution_processes.remove(&process_id);
                    if vm.package_mutation_execution_id.as_deref() == Some(&execution_id) {
                        vm.package_mutation_execution_id = None;
                    }
                    if let Some(execution) = vm.executions.get_mut(&execution_id) {
                        if let Some(task) = execution.deadline_task.take() {
                            task.abort();
                        }
                        execution.resident_process_id = None;
                        execution.descriptor.state = ExecutionState::Failed;
                        execution.descriptor.process_id = None;
                        execution.descriptor.pid = None;
                        execution.descriptor.last_completed_at_ms = Some(now_ms());
                        execution.descriptor.last_outcome = Some(ExecutionOutcome::Failed);
                        execution.expires_at_ms = (execution.public && !execution.context)
                            .then(|| now_ms().saturating_add(completed_ttl_ms));
                        execution.result = Some(failed_result(
                            execution.descriptor.clone(),
                            execution.public,
                            "execution_start_failed",
                            error.to_string(),
                        ));
                    }
                }
                let result = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&execution_id))
                    .and_then(|execution| execution.result.clone())
                    .expect("admitted start failure stores a result");
                self.schedule_execution_retention_wake(&vm_id);
                return Ok(DispatchResult {
                    response: self.respond(
                        request,
                        ResponsePayload::ExecutionAccepted(ExecutionAcceptedResponse {
                            operation_id: execution_id.clone(),
                            execution: result.execution.clone(),
                        }),
                    ),
                    events: vec![EventFrame::new(
                        request.ownership.clone(),
                        EventPayload::ExecutionCompleted(ExecutionCompletedEvent {
                            execution_id,
                            generation,
                            outcome: ExecutionOutcome::Failed,
                            exit_code: None,
                            error: result.error,
                        }),
                    )],
                });
            }
        };

        if let Some(launch) = &launch {
            if let ResponsePayload::ProcessStarted(started) = &launch.response.payload {
                if let Some(execution) = self
                    .vms
                    .get_mut(&vm_id)
                    .and_then(|vm| vm.executions.get_mut(&execution_id))
                {
                    execution.descriptor.pid = started.pid;
                }
            }
        }

        if let Some(stdin) = operation.stdin {
            self.write_stdin(
                request,
                WriteStdinRequest {
                    process_id: process_id.clone(),
                    chunk: stdin,
                },
            )
            .await?;
        }

        let descriptor = self
            .vms
            .get(&vm_id)
            .and_then(|vm| vm.executions.get(&execution_id))
            .map(|execution| execution.descriptor.clone())
            .unwrap_or(descriptor);
        debug_assert_eq!(descriptor.generation, generation);
        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ExecutionAccepted(ExecutionAcceptedResponse {
                    operation_id: execution_id,
                    execution: (context_execution || operation.background).then_some(descriptor),
                }),
            ),
            events: launch.map_or_else(Vec::new, |launch| launch.events),
        })
    }

    pub(crate) async fn handle_execution_lifecycle(
        &mut self,
        request: &RequestFrame,
        payload: RequestPayload,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        self.expire_public_execution_deadlines()?;

        let response = match payload {
            RequestPayload::CreateContext(payload) => {
                if payload.context_id.is_empty() {
                    return Ok(typed_rejection(
                        request,
                        "invalid_context_id",
                        "contextId must not be empty",
                    ));
                }
                let vm = self.vms.get(&vm_id).expect("owned VM checked above");
                if vm.executions.contains_key(&payload.context_id) {
                    return Ok(typed_rejection(
                        request,
                        "context_conflict",
                        format!("context {} already exists", payload.context_id),
                    ));
                }
                let context_count = vm
                    .executions
                    .values()
                    .filter(|execution| execution.context)
                    .count();
                if context_count >= vm.limits.execution.max_completed_executions {
                    let configured_limit = vm.limits.execution.max_completed_executions;
                    return Ok(DispatchResult {
                        response: self.respond(
                            request,
                            ResponsePayload::Rejected(RejectedResponse {
                                code: String::from("ERR_AGENTOS_RESOURCE_LIMIT"),
                                message: format!(
                                    "context count reached limits.execution.maxCompletedExecutions ({configured_limit}); delete an idle context or raise the limit"
                                ),
                                limit_name: Some(String::from(
                                    "limits.execution.maxCompletedExecutions",
                                )),
                                configured_limit: Some(
                                    u64::try_from(configured_limit).unwrap_or(u64::MAX),
                                ),
                                current_usage: Some(
                                    u64::try_from(context_count).unwrap_or(u64::MAX),
                                ),
                                requested: Some(1),
                                unit: Some(String::from("contexts")),
                                scope: Some(String::from("vm")),
                                vm_id: Some(vm_id.clone()),
                                session_generation: None,
                                capability_id: None,
                                operation: Some(String::from("createContext")),
                                configuration_path: Some(String::from(
                                    "limits.execution.maxCompletedExecutions",
                                )),
                                retryable: Some(false),
                                errno: Some(String::from("ENOBUFS")),
                            }),
                        ),
                        events: Vec::new(),
                    });
                }
                if context_count.saturating_add(1)
                    == vm.limits.execution.live_execution_warning_threshold
                {
                    eprintln!(
                        "agentos VM {vm_id} reached limits.execution.liveExecutionWarningThreshold ({}) with {} contexts",
                        vm.limits.execution.live_execution_warning_threshold,
                        context_count.saturating_add(1)
                    );
                }
                let vm = self.vms.get_mut(&vm_id).expect("owned VM checked above");
                let now = now_ms();
                let context_id = payload.context_id;
                let descriptor = ExecutionDescriptor {
                    execution_id: context_id.clone(),
                    generation: 0,
                    state: ExecutionState::Idle,
                    retained_language: None,
                    process_id: None,
                    pid: None,
                    created_at_ms: now,
                    last_started_at_ms: None,
                    last_completed_at_ms: None,
                    last_outcome: None,
                    last_exit_code: None,
                };
                vm.executions.insert(
                    context_id,
                    ManagedLanguageExecution {
                        public: true,
                        context: true,
                        descriptor: descriptor.clone(),
                        result: None,
                        events: VecDeque::new(),
                        retained_event_bytes: 0,
                        output_truncated: false,
                        next_sequence: 0,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        output_limit_bytes: 0,
                        output_limit_setting: "limits.execution.maxCompletedExecutions",
                        capture: ExecutionOutputCapture::None,
                        retain_events: false,
                        event_limit: self.config.runtime.protocol.max_process_events.max(1),
                        event_bytes_limit: EXECUTION_EVENT_BYTES_LIMIT,
                        uses_pty: false,
                        value_kind: ExecutionValueKind::None,
                        semantic_result_path: None,
                        pending_outcome: None,
                        deadline_ms: None,
                        expires_at_ms: None,
                        deadline_task: None,
                        resident_process_id: None,
                    },
                );
                ResponsePayload::ExecutionDescriptor(ExecutionDescriptorResponse {
                    execution: descriptor,
                })
            }
            RequestPayload::GetExecution(payload) => {
                let Some(execution) = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&payload.execution_id))
                    .filter(|execution| execution.context)
                else {
                    return Ok(typed_rejection(
                        request,
                        "context_not_found",
                        format!("context {} does not exist", payload.execution_id),
                    ));
                };
                ResponsePayload::ExecutionDescriptor(ExecutionDescriptorResponse {
                    execution: execution.descriptor.clone(),
                })
            }
            RequestPayload::ListExecutions(_) => {
                let executions = self
                    .vms
                    .get(&vm_id)
                    .map(|vm| {
                        vm.executions
                            .values()
                            .filter(|item| item.context)
                            .map(|item| item.descriptor.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                ResponsePayload::ExecutionList(ExecutionListResponse { executions })
            }
            RequestPayload::WaitExecution(payload) => {
                let Some((result, ephemeral)) = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&payload.execution_id))
                    .and_then(|execution| {
                        (execution.descriptor.state != ExecutionState::Running)
                            .then(|| {
                                execution
                                    .result
                                    .clone()
                                    .map(|result| (result, !execution.public))
                            })
                            .flatten()
                    })
                else {
                    if self
                        .vms
                        .get(&vm_id)
                        .and_then(|vm| vm.executions.get(&payload.execution_id))
                        .is_some_and(|execution| {
                            execution.descriptor.state == ExecutionState::Running
                        })
                    {
                        return Ok(typed_rejection(
                            request,
                            "execution_busy",
                            format!("execution {} is still running", payload.execution_id),
                        ));
                    }
                    return Ok(typed_rejection(
                        request,
                        "execution_result_not_found",
                        format!(
                            "execution {} does not exist or has no completed operation",
                            payload.execution_id
                        ),
                    ));
                };
                if ephemeral {
                    self.vms
                        .get_mut(&vm_id)
                        .expect("owned VM checked above")
                        .executions
                        .remove(&payload.execution_id);
                }
                ResponsePayload::ExecutionCompleted(result)
            }
            RequestPayload::CancelExecution(payload) => {
                let process_id = match active_process_id(self, &vm_id, &payload.execution_id) {
                    Ok(process_id) => process_id,
                    Err((code, message)) => return Ok(typed_rejection(request, code, message)),
                };
                if let Some(execution) = self
                    .vms
                    .get_mut(&vm_id)
                    .and_then(|vm| vm.executions.get_mut(&payload.execution_id))
                {
                    execution.pending_outcome = Some(ExecutionOutcome::Cancelled);
                    execution.deadline_ms =
                        Some(now_ms().saturating_add(EXECUTION_CANCEL_GRACE_MS));
                    if let Some(task) = execution.deadline_task.take() {
                        task.abort();
                    }
                }
                self.kill_process_internal(&vm_id, &process_id, "SIGTERM")?;
                let notify = Arc::clone(&self.process_event_notify);
                let runtime = self
                    .vms
                    .get(&vm_id)
                    .expect("execution VM exists")
                    .runtime_context
                    .clone();
                let task = runtime
                    .spawn(agentos_runtime::TaskClass::Timer, async move {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            EXECUTION_CANCEL_GRACE_MS,
                        ))
                        .await;
                        notify.notify_one();
                    })
                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
                self.vms
                    .get_mut(&vm_id)
                    .and_then(|vm| vm.executions.get_mut(&payload.execution_id))
                    .expect("execution checked above")
                    .deadline_task = Some(task);
                let descriptor = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&payload.execution_id))
                    .expect("execution checked above")
                    .descriptor
                    .clone();
                ResponsePayload::ExecutionDescriptor(ExecutionDescriptorResponse {
                    execution: descriptor,
                })
            }
            RequestPayload::SignalExecution(payload) => {
                let process_id = match active_process_id(self, &vm_id, &payload.execution_id) {
                    Ok(process_id) => process_id,
                    Err((code, message)) => return Ok(typed_rejection(request, code, message)),
                };
                self.kill_process_internal(&vm_id, &process_id, &payload.signal)?;
                let descriptor = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&payload.execution_id))
                    .expect("execution checked above")
                    .descriptor
                    .clone();
                ResponsePayload::ExecutionDescriptor(ExecutionDescriptorResponse {
                    execution: descriptor,
                })
            }
            RequestPayload::ResetExecution(payload) => {
                let Some(existing) = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&payload.execution_id))
                else {
                    return Ok(typed_rejection(
                        request,
                        "context_not_found",
                        format!("context {} does not exist", payload.execution_id),
                    ));
                };
                if existing.descriptor.state == ExecutionState::Running {
                    return Ok(typed_rejection(
                        request,
                        "execution_busy",
                        format!("execution {} is running", payload.execution_id),
                    ));
                }
                let resident_process_id = existing.resident_process_id.clone();
                if let Some(process_id) = resident_process_id {
                    self.finish_active_process_exit(&vm_id, &process_id, 0)?;
                    if let Some(vm) = self.vms.get_mut(&vm_id) {
                        vm.execution_processes.remove(&process_id);
                    }
                }
                let completed_ttl_ms = self
                    .vms
                    .get(&vm_id)
                    .expect("owned VM checked above")
                    .limits
                    .execution
                    .completed_ttl_ms;
                let execution = self
                    .vms
                    .get_mut(&vm_id)
                    .and_then(|vm| vm.executions.get_mut(&payload.execution_id))
                    .expect("execution checked above");
                execution.descriptor.state = ExecutionState::Resetting;
                execution.descriptor.generation = execution.descriptor.generation.saturating_add(1);
                execution.descriptor.retained_language = None;
                execution.descriptor.process_id = None;
                execution.descriptor.pid = None;
                execution.descriptor.last_started_at_ms = None;
                execution.descriptor.last_completed_at_ms = None;
                execution.descriptor.last_outcome = None;
                execution.descriptor.last_exit_code = None;
                execution.result = None;
                execution.events.clear();
                execution.retained_event_bytes = 0;
                execution.stdout.clear();
                execution.stderr.clear();
                execution.semantic_result_path = None;
                execution.value_kind = ExecutionValueKind::None;
                execution.deadline_ms = None;
                execution.expires_at_ms =
                    (!execution.context).then(|| now_ms().saturating_add(completed_ttl_ms));
                if let Some(task) = execution.deadline_task.take() {
                    task.abort();
                }
                execution.resident_process_id = None;
                execution.descriptor.state = ExecutionState::Idle;
                ResponsePayload::ExecutionDescriptor(ExecutionDescriptorResponse {
                    execution: execution.descriptor.clone(),
                })
            }
            RequestPayload::DeleteExecution(payload) => {
                let Some(execution) = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&payload.execution_id))
                else {
                    return Ok(typed_rejection(
                        request,
                        "context_not_found",
                        format!("context {} does not exist", payload.execution_id),
                    ));
                };
                if execution.descriptor.state == ExecutionState::Running {
                    return Ok(typed_rejection(
                        request,
                        "execution_busy",
                        format!("execution {} is running", payload.execution_id),
                    ));
                }
                let resident_process_id = execution.resident_process_id.clone();
                if let Some(process_id) = resident_process_id {
                    self.finish_active_process_exit(&vm_id, &process_id, 0)?;
                    if let Some(vm) = self.vms.get_mut(&vm_id) {
                        vm.execution_processes.remove(&process_id);
                    }
                }
                self.vms
                    .get_mut(&vm_id)
                    .expect("owned VM checked above")
                    .executions
                    .remove(&payload.execution_id);
                ResponsePayload::ExecutionDeleted(ExecutionDeletedResponse {
                    execution_id: payload.execution_id,
                })
            }
            RequestPayload::WriteExecutionStdin(payload) => {
                let process_id = match active_process_id(self, &vm_id, &payload.execution_id) {
                    Ok(process_id) => process_id,
                    Err((code, message)) => return Ok(typed_rejection(request, code, message)),
                };
                let accepted = payload.chunk.len() as u64;
                self.write_stdin(
                    request,
                    WriteStdinRequest {
                        process_id,
                        chunk: payload.chunk,
                    },
                )
                .await?;
                ResponsePayload::ExecutionIo(ExecutionIoResponse {
                    execution_id: payload.execution_id,
                    accepted_bytes: Some(accepted),
                })
            }
            RequestPayload::CloseExecutionStdin(payload) => {
                let process_id = match active_process_id(self, &vm_id, &payload.execution_id) {
                    Ok(process_id) => process_id,
                    Err((code, message)) => return Ok(typed_rejection(request, code, message)),
                };
                self.close_stdin(request, CloseStdinRequest { process_id })
                    .await?;
                ResponsePayload::ExecutionIo(ExecutionIoResponse {
                    execution_id: payload.execution_id,
                    accepted_bytes: None,
                })
            }
            RequestPayload::ResizeExecutionPty(payload) => {
                let process_id = match active_process_id(self, &vm_id, &payload.execution_id) {
                    Ok(process_id) => process_id,
                    Err((code, message)) => return Ok(typed_rejection(request, code, message)),
                };
                self.resize_pty(
                    request,
                    ResizePtyRequest {
                        process_id,
                        cols: payload.cols,
                        rows: payload.rows,
                    },
                )
                .await?;
                ResponsePayload::ExecutionIo(ExecutionIoResponse {
                    execution_id: payload.execution_id,
                    accepted_bytes: None,
                })
            }
            RequestPayload::ReadExecutionOutput(payload) => {
                let Some(execution) = self
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.executions.get(&payload.execution_id))
                else {
                    return Ok(typed_rejection(
                        request,
                        "execution_not_found",
                        format!("execution {} does not exist", payload.execution_id),
                    ));
                };
                if !execution.retain_events {
                    return Ok(typed_rejection(
                        request,
                        "execution_output_not_retained",
                        "process was not spawned with output.retainEvents enabled",
                    ));
                }
                let start = match payload.cursor.as_deref() {
                    None => 0,
                    Some(cursor) => {
                        let Some(start) = parse_cursor(cursor, execution.descriptor.generation)
                        else {
                            return Ok(typed_rejection(
                                request,
                                "execution_output_cursor_expired",
                                "the output cursor belongs to an earlier execution generation",
                            ));
                        };
                        start
                    }
                };
                let limit = payload
                    .limit
                    .unwrap_or(DEFAULT_EXECUTION_OUTPUT_PAGE_EVENTS)
                    .clamp(1, MAX_EXECUTION_OUTPUT_PAGE_EVENTS)
                    as usize;
                let events: Vec<_> = execution
                    .events
                    .iter()
                    .filter(|event| event.sequence >= start)
                    .take(limit)
                    .cloned()
                    .collect();
                let next_sequence = events
                    .last()
                    .map_or(start, |event| event.sequence.saturating_add(1));
                let has_more = execution
                    .events
                    .iter()
                    .any(|event| event.sequence >= next_sequence);
                ResponsePayload::ExecutionOutputPage(ExecutionOutputPageResponse {
                    execution_id: payload.execution_id,
                    generation: execution.descriptor.generation,
                    events,
                    next_cursor: format!("{}:{next_sequence}", execution.descriptor.generation),
                    has_more,
                    truncated: execution.output_truncated,
                })
            }
            _ => {
                return Err(SidecarError::InvalidState(String::from(
                    "request is not an execution lifecycle operation",
                )))
            }
        };
        self.schedule_execution_retention_wake(&vm_id);
        Ok(DispatchResult {
            response: self.respond(request, response),
            events: Vec::new(),
        })
    }

    pub(crate) fn is_public_execution_process(&self, vm_id: &str, process_id: &str) -> bool {
        self.vms
            .get(vm_id)
            .is_some_and(|vm| vm.execution_processes.contains_key(process_id))
    }

    pub(crate) fn should_park_public_execution_process(
        &self,
        vm_id: &str,
        process_id: &str,
    ) -> bool {
        self.vms
            .get(vm_id)
            .and_then(|vm| {
                let execution_id = vm.execution_processes.get(process_id)?;
                vm.executions.get(execution_id)
            })
            .is_some_and(|execution| {
                execution.resident_process_id.as_deref() == Some(process_id)
                    && execution.pending_outcome.is_none()
                    && !execution
                        .deadline_ms
                        .is_some_and(|deadline| now_ms() >= deadline)
            })
    }

    pub(crate) fn has_running_nonresident_processes(&self, vm_id: &str) -> bool {
        let Some(vm) = self.vms.get(vm_id) else {
            return false;
        };
        vm.active_processes.keys().any(|process_id| {
            !vm.executions
                .values()
                .any(|execution| execution.resident_process_id.as_deref() == Some(process_id))
        })
    }

    fn schedule_execution_retention_wake(&mut self, vm_id: &str) {
        let now = now_ms();
        let next_deadline = self.vms.get(vm_id).and_then(|vm| {
            let completed_count = vm
                .executions
                .values()
                .filter(|execution| {
                    execution.public
                        && !execution.context
                        && execution.descriptor.state != ExecutionState::Running
                        && execution.result.is_some()
                })
                .count();
            if completed_count > vm.limits.execution.max_completed_executions {
                return Some(now);
            }
            vm.executions
                .values()
                .filter_map(|execution| execution.expires_at_ms)
                .min()
        });
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return;
        };
        if vm.execution_retention_wake_deadline_ms == next_deadline
            && vm
                .execution_retention_wake_task
                .as_ref()
                .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        if let Some(task) = vm.execution_retention_wake_task.take() {
            task.abort();
        }
        vm.execution_retention_wake_deadline_ms = next_deadline;
        let Some(deadline) = next_deadline else {
            return;
        };
        let notify = Arc::clone(&self.process_event_notify);
        let delay_ms = deadline.saturating_sub(now);
        match vm
            .runtime_context
            .spawn(agentos_runtime::TaskClass::Timer, async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                notify.notify_one();
            }) {
            Ok(task) => vm.execution_retention_wake_task = Some(task),
            Err(error) => {
                eprintln!("agentos VM {vm_id} failed to schedule execution retention wake: {error}")
            }
        }
    }

    pub(crate) fn expire_public_execution_deadlines(&mut self) -> Result<(), SidecarError> {
        let now = now_ms();
        for vm in self.vms.values_mut() {
            if vm
                .execution_retention_wake_task
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                vm.execution_retention_wake_task = None;
                vm.execution_retention_wake_deadline_ms = None;
            }
        }
        // Idle retention uses the existing coalesced reactor turn. Bound each
        // drain so a large simultaneous expiry cannot monopolize the sidecar.
        let expired = self
            .vms
            .iter()
            .flat_map(|(vm_id, vm)| {
                vm.executions
                    .iter()
                    .filter_map(move |(execution_id, execution)| {
                        (execution.public
                            && !execution.context
                            && execution.descriptor.state != ExecutionState::Running
                            && execution
                                .expires_at_ms
                                .is_some_and(|expires_at| now >= expires_at))
                        .then(|| {
                            (
                                vm_id.clone(),
                                execution_id.clone(),
                                execution.resident_process_id.clone(),
                            )
                        })
                    })
            })
            .take(64)
            .collect::<Vec<_>>();
        for (vm_id, execution_id, resident_process_id) in expired {
            if let Some(process_id) = resident_process_id {
                self.finish_active_process_exit(&vm_id, &process_id, 0)?;
                if let Some(vm) = self.vms.get_mut(&vm_id) {
                    vm.execution_processes.remove(&process_id);
                }
            }
            if let Some(vm) = self.vms.get_mut(&vm_id) {
                vm.executions.remove(&execution_id);
                if vm.package_mutation_execution_id.as_deref() == Some(&execution_id) {
                    vm.package_mutation_execution_id = None;
                }
            }
        }
        let over_limit = self
            .vms
            .iter()
            .flat_map(|(vm_id, vm)| {
                let mut completed = vm
                    .executions
                    .iter()
                    .filter(|(_, execution)| {
                        execution.public
                            && !execution.context
                            && execution.descriptor.state != ExecutionState::Running
                            && execution.result.is_some()
                    })
                    .map(|(execution_id, execution)| {
                        (
                            execution_id.clone(),
                            execution.descriptor.last_completed_at_ms.unwrap_or(0),
                            execution.resident_process_id.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                completed.sort_by_key(|(_, completed_at, _)| *completed_at);
                let excess = completed
                    .len()
                    .saturating_sub(vm.limits.execution.max_completed_executions);
                completed
                    .into_iter()
                    .take(excess)
                    .map(move |(execution_id, _, resident)| (vm_id.clone(), execution_id, resident))
            })
            .take(64)
            .collect::<Vec<_>>();
        for (vm_id, execution_id, resident_process_id) in over_limit {
            eprintln!(
                "agentos VM {vm_id} evicted completed execution {execution_id} to enforce limits.execution.maxCompletedExecutions"
            );
            if let Some(process_id) = resident_process_id {
                self.finish_active_process_exit(&vm_id, &process_id, 0)?;
                if let Some(vm) = self.vms.get_mut(&vm_id) {
                    vm.execution_processes.remove(&process_id);
                }
            }
            if let Some(vm) = self.vms.get_mut(&vm_id) {
                vm.executions.remove(&execution_id);
            }
        }
        let due = self
            .vms
            .iter()
            .flat_map(|(vm_id, vm)| {
                vm.executions.iter().filter_map(move |(_, execution)| {
                    (execution.descriptor.state == ExecutionState::Running
                        && execution
                            .deadline_ms
                            .is_some_and(|deadline| now >= deadline))
                    .then(|| {
                        execution
                            .descriptor
                            .process_id
                            .as_ref()
                            .map(|process_id| (vm_id.clone(), process_id.clone()))
                    })
                    .flatten()
                })
            })
            .collect::<Vec<_>>();
        for (vm_id, process_id) in due {
            if let Some(execution_id) = self
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.execution_processes.get(&process_id))
                .cloned()
            {
                if let Some(execution) = self
                    .vms
                    .get_mut(&vm_id)
                    .and_then(|vm| vm.executions.get_mut(&execution_id))
                {
                    if execution.pending_outcome != Some(ExecutionOutcome::Cancelled) {
                        execution.pending_outcome = Some(ExecutionOutcome::TimedOut);
                    }
                    execution.deadline_ms = None;
                }
                // A deadline is already terminal. Force the process tree so a
                // CPU-bound guest cannot defer timeout handling indefinitely.
                self.kill_process_internal(&vm_id, &process_id, "SIGKILL")?;
            }
        }
        let vm_ids = self.vms.keys().cloned().collect::<Vec<_>>();
        for vm_id in vm_ids {
            self.schedule_execution_retention_wake(&vm_id);
        }
        Ok(())
    }

    pub(crate) fn record_public_execution_output(
        &mut self,
        vm_id: &str,
        process_id: &str,
        channel: ExecutionStreamChannel,
        chunk: Vec<u8>,
    ) -> Option<EventPayload> {
        let vm = self.vms.get_mut(vm_id)?;
        let execution_id = vm.execution_processes.get(process_id)?.clone();
        let execution = vm.executions.get_mut(&execution_id)?;
        if vm.package_mutation_execution_id.as_deref() == Some(&execution_id) {
            vm.package_mutation_execution_id = None;
        }
        let channel = if execution.uses_pty {
            ExecutionStreamChannel::Pty
        } else {
            channel
        };
        let capture_channel = match channel {
            ExecutionStreamChannel::Stderr => matches!(
                execution.capture,
                ExecutionOutputCapture::All | ExecutionOutputCapture::Stderr
            ),
            ExecutionStreamChannel::Stdout | ExecutionStreamChannel::Pty => {
                matches!(execution.capture, ExecutionOutputCapture::All)
                    || execution.value_kind == ExecutionValueKind::Python
            }
        };
        if capture_channel {
            let target = if matches!(channel, ExecutionStreamChannel::Stderr) {
                &mut execution.stderr
            } else {
                &mut execution.stdout
            };
            let previous_len = target.len();
            let available = execution.output_limit_bytes.saturating_sub(target.len());
            let retained_len = chunk.len().min(available);
            target.extend_from_slice(&chunk[..retained_len]);
            let warning_threshold = execution.output_limit_bytes.saturating_mul(4) / 5;
            if previous_len < warning_threshold && target.len() >= warning_threshold {
                eprintln!(
                    "agentos execution {} {:?} output reached {} of {} bytes; raise {} for more retained output",
                    execution.descriptor.execution_id,
                    channel,
                    target.len(),
                    execution.output_limit_bytes,
                    execution.output_limit_setting,
                );
            }
            if retained_len < chunk.len() {
                if matches!(channel, ExecutionStreamChannel::Stderr) {
                    execution.stderr_truncated = true;
                } else {
                    execution.stdout_truncated = true;
                }
            }
        }

        let event = ExecutionOutputEvent {
            execution_id,
            generation: execution.descriptor.generation,
            process_id: Some(process_id.to_owned()),
            sequence: execution.next_sequence,
            channel,
            chunk,
            timestamp_ms: now_ms(),
        };
        execution.next_sequence = execution.next_sequence.saturating_add(1);
        let event_bytes = event.chunk.len();
        if execution.retain_events {
            while execution.events.len() >= execution.event_limit
                || execution.retained_event_bytes.saturating_add(event_bytes)
                    > execution.event_bytes_limit
            {
                let Some(expired) = execution.events.pop_front() else {
                    break;
                };
                execution.retained_event_bytes = execution
                    .retained_event_bytes
                    .saturating_sub(expired.chunk.len());
                execution.output_truncated = true;
            }
            if event_bytes <= execution.event_bytes_limit {
                execution.retained_event_bytes =
                    execution.retained_event_bytes.saturating_add(event_bytes);
                execution.events.push_back(event.clone());
            } else {
                execution.output_truncated = true;
            }
        }
        Some(EventPayload::ExecutionOutput(event))
    }

    pub(crate) fn complete_public_execution(
        &mut self,
        vm_id: &str,
        process_id: &str,
        exit_code: i32,
    ) -> Option<EventPayload> {
        let vm = self.vms.get_mut(vm_id)?;
        let execution_id = vm.execution_processes.get(process_id)?.clone();
        let semantic_result = vm
            .executions
            .get(&execution_id)
            .and_then(|execution| execution.semantic_result_path.as_deref())
            .map(|path| {
                let result = vm
                    .kernel
                    .read_file(path)
                    .map_err(|error| format!("evaluation produced no structured result: {error}"));
                if result.is_ok() {
                    if let Err(error) = vm.kernel.remove_file(path) {
                        eprintln!(
                            "agentos VM {vm_id} execution {execution_id} could not remove semantic result {path}: {error}"
                        );
                    }
                }
                result
            });
        let resident_process_id = vm
            .executions
            .get(&execution_id)
            .and_then(|execution| execution.resident_process_id.clone())
            .filter(|resident_id| vm.active_processes.contains_key(resident_id));
        let completing_resident = resident_process_id.as_deref() == Some(process_id);
        if !completing_resident {
            vm.execution_processes.remove(process_id);
        }
        let execution = vm.executions.get_mut(&execution_id)?;
        if execution.resident_process_id.is_some() && resident_process_id.is_none() {
            execution.resident_process_id = None;
        }
        let deadline_expired = execution
            .deadline_ms
            .take()
            .is_some_and(|deadline| now_ms() >= deadline);
        if let Some(task) = execution.deadline_task.take() {
            task.abort();
        }
        let mut outcome = execution.pending_outcome.take().unwrap_or_else(|| {
            if deadline_expired {
                return ExecutionOutcome::TimedOut;
            }
            if exit_code == 0 {
                ExecutionOutcome::Succeeded
            } else {
                ExecutionOutcome::Failed
            }
        });
        let (semantic_result, evaluation_error) = if outcome == ExecutionOutcome::Succeeded {
            match extract_semantic_result(execution, semantic_result) {
                Ok(result) => (result, None),
                Err(message) => {
                    outcome = ExecutionOutcome::Failed;
                    (ExecutionSemanticResult::None, Some(message))
                }
            }
        } else {
            execution.semantic_result_path = None;
            (ExecutionSemanticResult::None, None)
        };
        let (evaluation_value, type_script_check_result) = match semantic_result {
            ExecutionSemanticResult::None => (None, None),
            ExecutionSemanticResult::Evaluation(value) => (Some(value), None),
            ExecutionSemanticResult::TypeScriptCheck(value) => (None, Some(value)),
        };

        execution.descriptor.state = ExecutionState::Idle;
        execution.descriptor.process_id = None;
        execution.descriptor.pid = None;
        execution.descriptor.last_completed_at_ms = Some(now_ms());
        execution.descriptor.last_outcome = Some(outcome.clone());
        execution.descriptor.last_exit_code = Some(exit_code);
        execution.expires_at_ms = (execution.public && !execution.context)
            .then(|| now_ms().saturating_add(vm.limits.execution.completed_ttl_ms));

        let error = if let Some(message) = evaluation_error {
            Some(ExecutionErrorData {
                code: String::from("evaluation_serialization_failed"),
                name: String::from("ExecutionEvaluationError"),
                message,
                stack: None,
                details: None,
            })
        } else if outcome == ExecutionOutcome::Succeeded {
            None
        } else {
            Some(ExecutionErrorData {
                code: match outcome {
                    ExecutionOutcome::Cancelled => String::from("execution_cancelled"),
                    ExecutionOutcome::TimedOut => String::from("execution_timed_out"),
                    ExecutionOutcome::Failed | ExecutionOutcome::Succeeded => {
                        String::from("execution_failed")
                    }
                },
                name: String::from("ExecutionError"),
                message: match outcome {
                    ExecutionOutcome::Cancelled => String::from("execution was cancelled"),
                    ExecutionOutcome::TimedOut => String::from("execution timed out"),
                    ExecutionOutcome::Failed | ExecutionOutcome::Succeeded => {
                        format!("execution exited with code {exit_code}")
                    }
                },
                stack: None,
                details: None,
            })
        };
        execution.result = Some(ExecutionCompletedResponse {
            execution: execution.public.then(|| execution.descriptor.clone()),
            outcome: outcome.clone(),
            exit_code: Some(exit_code),
            error: error.clone(),
            stdout: matches!(execution.capture, ExecutionOutputCapture::All)
                .then(|| execution.stdout.clone()),
            stderr: matches!(
                execution.capture,
                ExecutionOutputCapture::All | ExecutionOutputCapture::Stderr
            )
            .then(|| execution.stderr.clone()),
            stdout_truncated: matches!(execution.capture, ExecutionOutputCapture::All)
                .then_some(execution.stdout_truncated),
            stderr_truncated: matches!(
                execution.capture,
                ExecutionOutputCapture::All | ExecutionOutputCapture::Stderr
            )
            .then_some(execution.stderr_truncated),
            evaluation_value,
            type_script_check_result,
        });
        let event = EventPayload::ExecutionCompleted(ExecutionCompletedEvent {
            execution_id,
            generation: execution.descriptor.generation,
            outcome,
            exit_code: Some(exit_code),
            error,
        });
        self.schedule_execution_retention_wake(vm_id);
        Some(event)
    }
}

enum ExecutionSemanticResult {
    None,
    Evaluation(String),
    TypeScriptCheck(String),
}

fn extract_semantic_result(
    execution: &mut ManagedLanguageExecution,
    result: Option<Result<Vec<u8>, String>>,
) -> Result<ExecutionSemanticResult, String> {
    let Some(_path) = execution.semantic_result_path.take() else {
        return Ok(ExecutionSemanticResult::None);
    };
    let value = match result {
        Some(Ok(value)) => value,
        Some(Err(_)) if execution.value_kind == ExecutionValueKind::Python => {
            let stdout = String::from_utf8_lossy(&execution.stdout);
            let start = stdout
                .rfind(&_path)
                .ok_or_else(|| String::from("evaluation produced no structured result"))?;
            let value_start = start.saturating_add(_path.len());
            let value_end = stdout[value_start..]
                .find('\n')
                .map_or(stdout.len(), |offset| value_start.saturating_add(offset));
            let value = stdout.as_bytes()[value_start..value_end].to_vec();
            let remove_end = if value_end < execution.stdout.len() {
                value_end.saturating_add(1)
            } else {
                value_end
            };
            execution.stdout.drain(start..remove_end);
            value
        }
        Some(Err(error)) => return Err(error),
        None => return Err(String::from("evaluation produced no structured result")),
    };
    let value = serde_json::from_slice::<serde_json::Value>(&value)
        .map_err(|error| format!("evaluation returned an invalid structured result: {error}"))?;
    match execution.value_kind {
        ExecutionValueKind::JavaScript | ExecutionValueKind::Python => {
            let object = value.as_object().ok_or_else(|| {
                String::from("evaluation returned an invalid internal result envelope")
            })?;
            if object.get("__agentosEvaluation") != Some(&serde_json::Value::Bool(true)) {
                return Err(String::from(
                    "evaluation returned an invalid internal result envelope",
                ));
            }
            if object.get("ok") == Some(&serde_json::Value::Bool(false)) {
                return Err(object
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("evaluation result must be JSON-serializable")
                    .to_owned());
            }
            let value = object.get("value").cloned().ok_or_else(|| {
                String::from(
                    "AgentOS evaluation result must be JSON-serializable; undefined, functions, and symbols are not supported",
                )
            })?;
            serde_json::to_string(&value)
                .map(ExecutionSemanticResult::Evaluation)
                .map_err(|error| format!("failed to serialize evaluation result: {error}"))
        }
        ExecutionValueKind::TypeScriptCheck => serde_json::to_string(&value)
            .map(ExecutionSemanticResult::TypeScriptCheck)
            .map_err(|error| format!("failed to serialize TypeScript check result: {error}")),
        ExecutionValueKind::None => Ok(ExecutionSemanticResult::None),
    }
}

fn active_process_id<B: NativeSidecarBridge>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    execution_id: &str,
) -> Result<String, (&'static str, String)> {
    let Some(execution) = sidecar
        .vms
        .get(vm_id)
        .and_then(|vm| vm.executions.get(execution_id))
    else {
        return Err((
            "execution_not_found",
            format!("execution {execution_id} does not exist"),
        ));
    };
    if execution.descriptor.state != ExecutionState::Running {
        return Err((
            "execution_not_running",
            format!("execution {execution_id} is not running"),
        ));
    }
    execution.descriptor.process_id.clone().ok_or_else(|| {
        (
            "execution_not_running",
            format!("execution {execution_id} has no active process"),
        )
    })
}

fn parse_cursor(cursor: &str, generation: u64) -> Option<u64> {
    let (cursor_generation, sequence) = cursor.split_once(':')?;
    (cursor_generation.parse::<u64>().ok()? == generation)
        .then(|| sequence.parse::<u64>().ok())
        .flatten()
}

fn failed_result(
    execution: ExecutionDescriptor,
    public: bool,
    code: impl Into<String>,
    message: impl Into<String>,
) -> ExecutionCompletedResponse {
    ExecutionCompletedResponse {
        execution: public.then_some(execution),
        outcome: ExecutionOutcome::Failed,
        exit_code: None,
        error: Some(ExecutionErrorData {
            code: code.into(),
            name: String::from("ExecutionError"),
            message: message.into(),
            stack: None,
            details: None,
        }),
        stdout: None,
        stderr: None,
        stdout_truncated: None,
        stderr_truncated: None,
        evaluation_value: None,
        type_script_check_result: None,
    }
}
