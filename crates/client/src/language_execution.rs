//! First-class JavaScript, TypeScript, Python, and shared execution lifecycle.

use std::collections::HashMap;

use agentos_sidecar_client::wire;
use tokio::sync::{broadcast, watch};

use crate::agent_os::AgentOs;
use crate::agent_os::ProcessEntry;
use crate::error::{ClientError, ClientResult};
use crate::process::{ProcessOutput, ProcessStream};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextDescriptor {
    pub context_id: String,
    pub state: String,
    pub language: Option<String>,
    pub created_at_ms: u64,
    pub last_started_at_ms: Option<u64>,
    pub last_completed_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessDescriptor {
    pub pid: u32,
    pub state: String,
    pub language: Option<String>,
    pub started_at_ms: u64,
}

pub type CodeExecutionResult = wire::ExecutionCompletedResponse;
pub type ExecutionOutputEvent = wire::ExecutionOutputEvent;
pub type ExecutionCompletedEvent = wire::ExecutionCompletedEvent;
pub type TypeScriptDiagnostic = wire::TypeScriptDiagnostic;

#[derive(Debug, Clone, Default)]
pub struct LanguageExecutionOptions {
    pub context_id: Option<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout_ms: Option<u64>,
    pub pty: Option<ExecutionPtyOptions>,
    pub output: ExecutionOutputOptions,
}

#[derive(Debug, Clone, Default)]
pub struct LanguageSpawnOptions {
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout_ms: Option<u64>,
    pub pty: Option<ExecutionPtyOptions>,
    pub retain_events: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputCapture {
    #[default]
    None,
    Stderr,
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionOutputOptions {
    pub capture: OutputCapture,
    pub retain_events: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionPtyOptions {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

#[derive(Debug, Clone, Default)]
pub struct InlineExecutionOptions {
    pub process: LanguageExecutionOptions,
    pub inputs: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JavaScriptModuleFormat {
    /// Evaluate each call as an independent root ES module. A retained context
    /// preserves `globalThis`, not the module's lexical scope.
    #[default]
    Module,
    /// Use script/CommonJS semantics, including REPL-style top-level bindings
    /// that remain visible to later calls in a retained context.
    CommonJs,
}

#[derive(Debug, Clone, Default)]
pub struct JavaScriptExecutionOptions {
    pub inline: InlineExecutionOptions,
    pub format: JavaScriptModuleFormat,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TypeScriptExecutionOptions {
    pub inline: InlineExecutionOptions,
    pub file_path: Option<String>,
    pub tsconfig_path: Option<String>,
    pub compiler_options: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Default)]
pub struct TypeScriptCheckOptions {
    pub context_id: Option<String>,
    pub cwd: Option<String>,
    pub file_path: Option<String>,
    pub tsconfig_path: Option<String>,
    pub compiler_options: Option<serde_json::Map<String, serde_json::Value>>,
    pub timeout_ms: Option<u64>,
    pub output: ExecutionOutputOptions,
}

#[derive(Debug, Clone, Default)]
pub struct NpmProjectInstallOptions {
    pub context_id: Option<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: Option<u64>,
    pub frozen: Option<bool>,
    pub output: ExecutionOutputOptions,
}

#[derive(Debug, Clone, Default)]
pub struct NpmPackageInstallOptions {
    pub context_id: Option<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: Option<u64>,
    pub dev: Option<bool>,
    pub global: Option<bool>,
    pub output: ExecutionOutputOptions,
}

#[derive(Debug, Clone, Default)]
pub struct PythonInstallOptions {
    pub context_id: Option<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: Option<u64>,
    pub upgrade: Option<bool>,
    pub requirements_file: Option<String>,
    pub index_url: Option<String>,
    pub extra_index_urls: Vec<String>,
    pub output: ExecutionOutputOptions,
}

#[derive(Debug, Clone)]
pub struct TypeScriptCheckResult {
    pub result: CodeExecutionResult,
    pub has_errors: Option<bool>,
    pub diagnostics: Vec<TypeScriptDiagnostic>,
}

#[derive(Debug, Clone)]
enum ExecutionSubmission {
    Completed(CodeExecutionResult),
    Background(wire::ExecutionDescriptor),
}

#[derive(Debug, Clone)]
pub struct CodeEvaluationResult {
    pub result: CodeExecutionResult,
    pub value: Option<serde_json::Value>,
}

fn identity(options: &LanguageExecutionOptions) -> wire::ExecutionIdentityOptions {
    wire::ExecutionIdentityOptions {
        context_id: options.context_id.clone(),
    }
}

fn process(options: &LanguageExecutionOptions) -> wire::ProcessExecutionOptions {
    wire::ProcessExecutionOptions {
        identity: identity(options),
        output: output(options.output),
        operation_id: None,
        background: Some(false),
        cwd: options.cwd.clone(),
        env: (!options.env.is_empty()).then(|| options.env.clone()),
        args: options.args.clone(),
        stdin: options.stdin.clone(),
        timeout_ms: options.timeout_ms,
        pty: options.pty.map(|pty| wire::ExecutionPtyOptions {
            cols: pty.cols,
            rows: pty.rows,
        }),
    }
}

fn background_process(
    options: &LanguageSpawnOptions,
    operation_id: String,
) -> wire::ProcessExecutionOptions {
    wire::ProcessExecutionOptions {
        identity: wire::ExecutionIdentityOptions { context_id: None },
        output: wire::ExecutionOutputOptions {
            capture: Some(wire::ExecutionOutputCapture::None),
            retain_events: Some(options.retain_events),
        },
        operation_id: Some(operation_id),
        background: Some(true),
        cwd: options.cwd.clone(),
        env: (!options.env.is_empty()).then(|| options.env.clone()),
        args: options.args.clone(),
        stdin: options.stdin.clone(),
        timeout_ms: options.timeout_ms,
        pty: options.pty.map(|pty| wire::ExecutionPtyOptions {
            cols: pty.cols,
            rows: pty.rows,
        }),
    }
}

fn output(options: ExecutionOutputOptions) -> wire::ExecutionOutputOptions {
    wire::ExecutionOutputOptions {
        capture: Some(match options.capture {
            OutputCapture::None => wire::ExecutionOutputCapture::None,
            OutputCapture::Stderr => wire::ExecutionOutputCapture::Stderr,
            OutputCapture::All => wire::ExecutionOutputCapture::All,
        }),
        retain_events: Some(options.retain_events),
    }
}

fn json_inputs(options: &InlineExecutionOptions) -> ClientResult<Option<String>> {
    options
        .inputs
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| ClientError::Sidecar(format!("failed to serialize inputs: {error}")))
}

fn context_descriptor(descriptor: wire::ExecutionDescriptor) -> ContextDescriptor {
    ContextDescriptor {
        context_id: descriptor.execution_id,
        state: match descriptor.state {
            wire::ExecutionState::Creating | wire::ExecutionState::Idle => "idle",
            wire::ExecutionState::Running => "running",
            wire::ExecutionState::Resetting => "resetting",
            wire::ExecutionState::Deleting => "deleting",
            wire::ExecutionState::Failed => "failed",
        }
        .to_owned(),
        language: descriptor.retained_language.map(|language| match language {
            wire::RetainedExecutionLanguage::JavaScript => String::from("javascript"),
            wire::RetainedExecutionLanguage::Python => String::from("python"),
        }),
        created_at_ms: descriptor.created_at_ms,
        last_started_at_ms: descriptor.last_started_at_ms,
        last_completed_at_ms: descriptor.last_completed_at_ms,
    }
}

fn execution_request_options(
    payload: &wire::RequestPayload,
) -> Option<(
    &wire::ExecutionIdentityOptions,
    &wire::ExecutionOutputOptions,
)> {
    Some(match payload {
        wire::RequestPayload::ShellExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::ArgvExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::JavaScriptExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::JavaScriptEvaluationRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::JavaScriptFileExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::TypeScriptExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::TypeScriptEvaluationRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::TypeScriptFileExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::TypeScriptCheckRequest(request) => {
            (&request.identity, &request.output)
        }
        wire::RequestPayload::TypeScriptProjectCheckRequest(request) => {
            (&request.identity, &request.output)
        }
        wire::RequestPayload::NpmProjectInstallRequest(request) => {
            (&request.identity, &request.output)
        }
        wire::RequestPayload::NpmPackageInstallRequest(request) => {
            (&request.identity, &request.output)
        }
        wire::RequestPayload::NpmScriptExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::NpmPackageExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::PythonExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::PythonEvaluationRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::PythonFileExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::PythonModuleExecutionRequest(request) => {
            (&request.process.identity, &request.process.output)
        }
        wire::RequestPayload::PythonInstallRequest(request) => (&request.identity, &request.output),
        _ => return None,
    })
}

impl AgentOs {
    fn execution_ownership(&self) -> wire::OwnershipScope {
        let inner = self.inner();
        wire::OwnershipScope::VmOwnership(wire::VmOwnership {
            connection_id: inner.connection_id.clone(),
            session_id: inner.session_id.clone(),
            vm_id: inner.vm_id.clone(),
        })
    }

    async fn submit_execution(
        &self,
        payload: wire::RequestPayload,
        background: bool,
    ) -> ClientResult<ExecutionSubmission> {
        if let Some((identity, output)) = execution_request_options(&payload) {
            if background && identity.context_id.is_some() {
                return Err(ClientError::Sidecar(String::from(
                    "spawned language processes cannot use context_id",
                )));
            }
            if output.retain_events == Some(true) && identity.context_id.is_none() && !background {
                return Err(ClientError::Sidecar(String::from(
                    "retain_events requires a context or spawned language process",
                )));
            }
        }
        let mut events = self.transport().subscribe_wire_events();
        let accepted = match self
            .transport()
            .request_wire(self.execution_ownership(), payload)
            .await?
        {
            wire::ResponsePayload::ExecutionAcceptedResponse(response) => response,
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(ClientError::from_rejection(rejected))
            }
            response => {
                return Err(ClientError::Sidecar(format!(
                    "unexpected execution response: {response:?}"
                )))
            }
        };
        if background {
            return Ok(ExecutionSubmission::Background(
                accepted.execution.ok_or_else(|| {
                    ClientError::Sidecar(String::from(
                        "spawned process admission returned no descriptor",
                    ))
                })?,
            ));
        }
        wait_for_completion_event(&mut events, &accepted.operation_id).await?;
        Ok(ExecutionSubmission::Completed(
            self.wait_execution(&accepted.operation_id).await?,
        ))
    }

    pub async fn exec(
        &self,
        command: impl Into<String>,
        options: LanguageExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::ShellExecutionRequest(wire::ShellExecutionRequest {
                    process: process(&options),
                    command: command.into(),
                }),
                false,
            )
            .await?,
        )
    }

    pub async fn exec_argv(
        &self,
        command: impl Into<String>,
        args: Vec<String>,
        mut options: LanguageExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        options.args = args;
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::ArgvExecutionRequest(wire::ArgvExecutionRequest {
                    process: process(&options),
                    command: command.into(),
                }),
                false,
            )
            .await?,
        )
    }

    pub async fn execute_javascript(
        &self,
        source: impl Into<String>,
        options: JavaScriptExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        let inputs = json_inputs(&options.inline)?;
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::JavaScriptExecutionRequest(
                    wire::JavaScriptExecutionRequest {
                        process: process(&options.inline.process),
                        source: source.into(),
                        format: Some(match options.format {
                            JavaScriptModuleFormat::Module => wire::JavaScriptModuleFormat::Module,
                            JavaScriptModuleFormat::CommonJs => {
                                wire::JavaScriptModuleFormat::CommonJs
                            }
                        }),
                        file_path: options.file_path,
                        inputs,
                    },
                ),
                false,
            )
            .await?,
        )
    }

    pub async fn evaluate_javascript(
        &self,
        expression: impl Into<String>,
        options: JavaScriptExecutionOptions,
    ) -> ClientResult<CodeEvaluationResult> {
        let inputs = json_inputs(&options.inline)?;
        let submission = self
            .submit_execution(
                wire::RequestPayload::JavaScriptEvaluationRequest(
                    wire::JavaScriptEvaluationRequest {
                        process: process(&options.inline.process),
                        expression: expression.into(),
                        format: Some(match options.format {
                            JavaScriptModuleFormat::Module => wire::JavaScriptModuleFormat::Module,
                            JavaScriptModuleFormat::CommonJs => {
                                wire::JavaScriptModuleFormat::CommonJs
                            }
                        }),
                        file_path: options.file_path,
                        inputs,
                    },
                ),
                false,
            )
            .await?;
        evaluation_result(submission)
    }

    pub async fn execute_javascript_file(
        &self,
        path: impl Into<String>,
        options: LanguageExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::JavaScriptFileExecutionRequest(
                    wire::JavaScriptFileExecutionRequest {
                        process: process(&options),
                        path: path.into(),
                    },
                ),
                false,
            )
            .await?,
        )
    }

    pub async fn spawn_javascript(
        &self,
        source: impl Into<String>,
        options: LanguageSpawnOptions,
    ) -> ClientResult<ProcessDescriptor> {
        let operation_id = format!("process-{}", uuid::Uuid::new_v4());
        background_submission(
            self,
            self.submit_execution(
                wire::RequestPayload::JavaScriptExecutionRequest(
                    wire::JavaScriptExecutionRequest {
                        process: background_process(&options, operation_id),
                        source: source.into(),
                        format: Some(wire::JavaScriptModuleFormat::Module),
                        file_path: None,
                        inputs: None,
                    },
                ),
                true,
            )
            .await?,
            "javascript",
        )
    }

    pub async fn spawn_javascript_file(
        &self,
        path: impl Into<String>,
        options: LanguageSpawnOptions,
    ) -> ClientResult<ProcessDescriptor> {
        let operation_id = format!("process-{}", uuid::Uuid::new_v4());
        background_submission(
            self,
            self.submit_execution(
                wire::RequestPayload::JavaScriptFileExecutionRequest(
                    wire::JavaScriptFileExecutionRequest {
                        process: background_process(&options, operation_id),
                        path: path.into(),
                    },
                ),
                true,
            )
            .await?,
            "javascript",
        )
    }

    pub async fn execute_typescript(
        &self,
        source: impl Into<String>,
        options: TypeScriptExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        let inputs = json_inputs(&options.inline)?;
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::TypeScriptExecutionRequest(
                    wire::TypeScriptExecutionRequest {
                        process: process(&options.inline.process),
                        source: source.into(),
                        file_path: options.file_path,
                        tsconfig_path: options.tsconfig_path,
                        compiler_options: options
                            .compiler_options
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()
                            .map_err(|error| ClientError::Sidecar(error.to_string()))?,
                        inputs,
                    },
                ),
                false,
            )
            .await?,
        )
    }

    pub async fn evaluate_typescript(
        &self,
        expression: impl Into<String>,
        options: TypeScriptExecutionOptions,
    ) -> ClientResult<CodeEvaluationResult> {
        let inputs = json_inputs(&options.inline)?;
        let submission = self
            .submit_execution(
                wire::RequestPayload::TypeScriptEvaluationRequest(
                    wire::TypeScriptEvaluationRequest {
                        process: process(&options.inline.process),
                        expression: expression.into(),
                        file_path: options.file_path,
                        tsconfig_path: options.tsconfig_path,
                        compiler_options: options
                            .compiler_options
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()
                            .map_err(|error| ClientError::Sidecar(error.to_string()))?,
                        inputs,
                    },
                ),
                false,
            )
            .await?;
        evaluation_result(submission)
    }

    pub async fn execute_typescript_file(
        &self,
        path: impl Into<String>,
        options: TypeScriptExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::TypeScriptFileExecutionRequest(
                    wire::TypeScriptFileExecutionRequest {
                        process: process(&options.inline.process),
                        path: path.into(),
                        tsconfig_path: options.tsconfig_path,
                        compiler_options: options
                            .compiler_options
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()
                            .map_err(|error| ClientError::Sidecar(error.to_string()))?,
                    },
                ),
                false,
            )
            .await?,
        )
    }

    pub async fn spawn_typescript(
        &self,
        source: impl Into<String>,
        options: LanguageSpawnOptions,
    ) -> ClientResult<ProcessDescriptor> {
        let operation_id = format!("process-{}", uuid::Uuid::new_v4());
        background_submission(
            self,
            self.submit_execution(
                wire::RequestPayload::TypeScriptExecutionRequest(
                    wire::TypeScriptExecutionRequest {
                        process: background_process(&options, operation_id),
                        source: source.into(),
                        file_path: None,
                        tsconfig_path: None,
                        compiler_options: None,
                        inputs: None,
                    },
                ),
                true,
            )
            .await?,
            "javascript",
        )
    }

    pub async fn spawn_typescript_file(
        &self,
        path: impl Into<String>,
        options: LanguageSpawnOptions,
    ) -> ClientResult<ProcessDescriptor> {
        let operation_id = format!("process-{}", uuid::Uuid::new_v4());
        background_submission(
            self,
            self.submit_execution(
                wire::RequestPayload::TypeScriptFileExecutionRequest(
                    wire::TypeScriptFileExecutionRequest {
                        process: background_process(&options, operation_id),
                        path: path.into(),
                        tsconfig_path: None,
                        compiler_options: None,
                    },
                ),
                true,
            )
            .await?,
            "javascript",
        )
    }

    pub async fn check_typescript(
        &self,
        source: impl Into<String>,
        options: TypeScriptCheckOptions,
    ) -> ClientResult<TypeScriptCheckResult> {
        let compiler_options = options
            .compiler_options
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| ClientError::Sidecar(error.to_string()))?;
        let submission = self
            .submit_execution(
                wire::RequestPayload::TypeScriptCheckRequest(wire::TypeScriptCheckRequest {
                    identity: wire::ExecutionIdentityOptions {
                        context_id: options.context_id,
                    },
                    output: output(options.output),
                    source: source.into(),
                    cwd: options.cwd,
                    file_path: options.file_path,
                    tsconfig_path: options.tsconfig_path,
                    compiler_options,
                    timeout_ms: options.timeout_ms,
                }),
                false,
            )
            .await?;
        let result = completed_submission(submission)?;
        typescript_check_result(result)
    }

    pub async fn check_typescript_project(
        &self,
        options: TypeScriptCheckOptions,
    ) -> ClientResult<TypeScriptCheckResult> {
        let submission = self
            .submit_execution(
                wire::RequestPayload::TypeScriptProjectCheckRequest(
                    wire::TypeScriptProjectCheckRequest {
                        identity: wire::ExecutionIdentityOptions {
                            context_id: options.context_id,
                        },
                        output: output(options.output),
                        cwd: options.cwd,
                        tsconfig_path: options.tsconfig_path,
                        timeout_ms: options.timeout_ms,
                    },
                ),
                false,
            )
            .await?;
        let result = completed_submission(submission)?;
        typescript_check_result(result)
    }

    pub async fn install_npm_project(
        &self,
        options: NpmProjectInstallOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::NpmProjectInstallRequest(wire::NpmProjectInstallRequest {
                    identity: wire::ExecutionIdentityOptions {
                        context_id: options.context_id,
                    },
                    output: output(options.output),
                    cwd: options.cwd,
                    env: (!options.env.is_empty()).then_some(options.env),
                    timeout_ms: options.timeout_ms,
                    frozen: options.frozen,
                }),
                false,
            )
            .await?,
        )
    }

    pub async fn install_npm_packages(
        &self,
        packages: Vec<String>,
        options: NpmPackageInstallOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::NpmPackageInstallRequest(wire::NpmPackageInstallRequest {
                    identity: wire::ExecutionIdentityOptions {
                        context_id: options.context_id,
                    },
                    output: output(options.output),
                    cwd: options.cwd,
                    env: (!options.env.is_empty()).then_some(options.env),
                    timeout_ms: options.timeout_ms,
                    packages,
                    dev: options.dev,
                    global: options.global,
                }),
                false,
            )
            .await?,
        )
    }

    pub async fn execute_npm_script(
        &self,
        script: impl Into<String>,
        options: LanguageExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::NpmScriptExecutionRequest(wire::NpmScriptExecutionRequest {
                    process: process(&options),
                    script: script.into(),
                }),
                false,
            )
            .await?,
        )
    }

    pub async fn execute_npm_package(
        &self,
        package_spec: impl Into<String>,
        binary: Option<String>,
        options: LanguageExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::NpmPackageExecutionRequest(
                    wire::NpmPackageExecutionRequest {
                        process: process(&options),
                        package_spec: package_spec.into(),
                        binary,
                    },
                ),
                false,
            )
            .await?,
        )
    }

    pub async fn execute_python(
        &self,
        source: impl Into<String>,
        options: InlineExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        let inputs = json_inputs(&options)?;
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::PythonExecutionRequest(wire::PythonExecutionRequest {
                    process: process(&options.process),
                    source: source.into(),
                    inputs,
                }),
                false,
            )
            .await?,
        )
    }

    pub async fn evaluate_python(
        &self,
        expression: impl Into<String>,
        options: InlineExecutionOptions,
    ) -> ClientResult<CodeEvaluationResult> {
        let inputs = json_inputs(&options)?;
        let submission = self
            .submit_execution(
                wire::RequestPayload::PythonEvaluationRequest(wire::PythonEvaluationRequest {
                    process: process(&options.process),
                    expression: expression.into(),
                    inputs,
                }),
                false,
            )
            .await?;
        evaluation_result(submission)
    }

    pub async fn execute_python_file(
        &self,
        path: impl Into<String>,
        options: LanguageExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::PythonFileExecutionRequest(
                    wire::PythonFileExecutionRequest {
                        process: process(&options),
                        path: path.into(),
                    },
                ),
                false,
            )
            .await?,
        )
    }

    pub async fn execute_python_module(
        &self,
        module: impl Into<String>,
        options: LanguageExecutionOptions,
    ) -> ClientResult<CodeExecutionResult> {
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::PythonModuleExecutionRequest(
                    wire::PythonModuleExecutionRequest {
                        process: process(&options),
                        module: module.into(),
                    },
                ),
                false,
            )
            .await?,
        )
    }

    pub async fn spawn_python(
        &self,
        source: impl Into<String>,
        options: LanguageSpawnOptions,
    ) -> ClientResult<ProcessDescriptor> {
        let operation_id = format!("process-{}", uuid::Uuid::new_v4());
        background_submission(
            self,
            self.submit_execution(
                wire::RequestPayload::PythonExecutionRequest(wire::PythonExecutionRequest {
                    process: background_process(&options, operation_id),
                    source: source.into(),
                    inputs: None,
                }),
                true,
            )
            .await?,
            "python",
        )
    }

    pub async fn spawn_python_file(
        &self,
        path: impl Into<String>,
        options: LanguageSpawnOptions,
    ) -> ClientResult<ProcessDescriptor> {
        let operation_id = format!("process-{}", uuid::Uuid::new_v4());
        background_submission(
            self,
            self.submit_execution(
                wire::RequestPayload::PythonFileExecutionRequest(
                    wire::PythonFileExecutionRequest {
                        process: background_process(&options, operation_id),
                        path: path.into(),
                    },
                ),
                true,
            )
            .await?,
            "python",
        )
    }

    pub async fn spawn_python_module(
        &self,
        module: impl Into<String>,
        options: LanguageSpawnOptions,
    ) -> ClientResult<ProcessDescriptor> {
        let operation_id = format!("process-{}", uuid::Uuid::new_v4());
        background_submission(
            self,
            self.submit_execution(
                wire::RequestPayload::PythonModuleExecutionRequest(
                    wire::PythonModuleExecutionRequest {
                        process: background_process(&options, operation_id),
                        module: module.into(),
                    },
                ),
                true,
            )
            .await?,
            "python",
        )
    }

    pub async fn install_python_packages(
        &self,
        packages: Vec<String>,
        options: PythonInstallOptions,
    ) -> ClientResult<CodeExecutionResult> {
        if !packages.is_empty() && options.requirements_file.is_some() {
            return Err(ClientError::Sidecar(String::from(
                "install_python_packages cannot combine packages with requirements_file",
            )));
        }
        completed_submission(
            self.submit_execution(
                wire::RequestPayload::PythonInstallRequest(wire::PythonInstallRequest {
                    identity: wire::ExecutionIdentityOptions {
                        context_id: options.context_id,
                    },
                    output: output(options.output),
                    cwd: options.cwd,
                    env: (!options.env.is_empty()).then_some(options.env),
                    timeout_ms: options.timeout_ms,
                    packages,
                    upgrade: options.upgrade,
                    requirements_file: options.requirements_file,
                    index_url: options.index_url,
                    extra_index_urls: options.extra_index_urls,
                }),
                false,
            )
            .await?,
        )
    }

    pub async fn create_context(&self, context_id: &str) -> ClientResult<()> {
        match self
            .transport()
            .request_wire(
                self.execution_ownership(),
                wire::RequestPayload::CreateContextRequest(wire::CreateContextRequest {
                    context_id: context_id.to_owned(),
                }),
            )
            .await?
        {
            wire::ResponsePayload::ExecutionDescriptorResponse(_) => Ok(()),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected))
            }
            response => Err(ClientError::Sidecar(format!(
                "unexpected create_context response: {response:?}"
            ))),
        }
    }

    pub async fn get_context(&self, context_id: &str) -> ClientResult<ContextDescriptor> {
        match self
            .transport()
            .request_wire(
                self.execution_ownership(),
                wire::RequestPayload::GetExecutionRequest(wire::GetExecutionRequest {
                    execution_id: context_id.to_owned(),
                }),
            )
            .await?
        {
            wire::ResponsePayload::ExecutionDescriptorResponse(response) => {
                Ok(context_descriptor(response.execution))
            }
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected))
            }
            response => Err(ClientError::Sidecar(format!(
                "unexpected get_context response: {response:?}"
            ))),
        }
    }

    pub async fn list_contexts(&self) -> ClientResult<Vec<ContextDescriptor>> {
        match self
            .transport()
            .request_wire(
                self.execution_ownership(),
                wire::RequestPayload::ListExecutionsRequest,
            )
            .await?
        {
            wire::ResponsePayload::ExecutionListResponse(response) => Ok(response
                .executions
                .into_iter()
                .map(context_descriptor)
                .collect()),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected))
            }
            response => Err(ClientError::Sidecar(format!(
                "unexpected list_contexts response: {response:?}"
            ))),
        }
    }

    async fn wait_execution(&self, execution_id: &str) -> ClientResult<CodeExecutionResult> {
        let mut events = self.transport().subscribe_wire_events();
        let first = self
            .transport()
            .request_wire(
                self.execution_ownership(),
                wire::RequestPayload::WaitExecutionRequest(wire::WaitExecutionRequest {
                    execution_id: execution_id.to_owned(),
                }),
            )
            .await?;
        let response = match first {
            wire::ResponsePayload::RejectedResponse(rejected)
                if rejected.code == "execution_busy" =>
            {
                wait_for_completion_event(&mut events, execution_id).await?;
                self.transport()
                    .request_wire(
                        self.execution_ownership(),
                        wire::RequestPayload::WaitExecutionRequest(wire::WaitExecutionRequest {
                            execution_id: execution_id.to_owned(),
                        }),
                    )
                    .await?
            }
            response => response,
        };
        match response {
            wire::ResponsePayload::ExecutionCompletedResponse(response) => Ok(response),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected))
            }
            response => Err(ClientError::Sidecar(format!(
                "unexpected wait_execution response: {response:?}"
            ))),
        }
    }

    pub async fn reset_context(&self, context_id: &str) -> ClientResult<()> {
        self.execution_descriptor_request(wire::RequestPayload::ResetExecutionRequest(
            wire::ResetExecutionRequest {
                execution_id: context_id.to_owned(),
            },
        ))
        .await?;
        Ok(())
    }

    async fn execution_descriptor_request(
        &self,
        payload: wire::RequestPayload,
    ) -> ClientResult<wire::ExecutionDescriptor> {
        match self
            .transport()
            .request_wire(self.execution_ownership(), payload)
            .await?
        {
            wire::ResponsePayload::ExecutionDescriptorResponse(response) => Ok(response.execution),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected))
            }
            response => Err(ClientError::Sidecar(format!(
                "unexpected execution lifecycle response: {response:?}"
            ))),
        }
    }

    pub async fn delete_context(&self, context_id: &str) -> ClientResult<()> {
        match self
            .transport()
            .request_wire(
                self.execution_ownership(),
                wire::RequestPayload::DeleteExecutionRequest(wire::DeleteExecutionRequest {
                    execution_id: context_id.to_owned(),
                }),
            )
            .await?
        {
            wire::ResponsePayload::ExecutionDeletedResponse(_) => Ok(()),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected))
            }
            response => Err(ClientError::Sidecar(format!(
                "unexpected delete_context response: {response:?}"
            ))),
        }
    }
}

async fn wait_for_completion_event(
    events: &mut broadcast::Receiver<(wire::OwnershipScope, wire::EventPayload)>,
    execution_id: &str,
) -> ClientResult<()> {
    loop {
        match events.recv().await {
            Ok((_, wire::EventPayload::ExecutionCompletedEvent(event)))
                if event.execution_id == execution_id =>
            {
                return Ok(())
            }
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => {
                return Err(ClientError::Sidecar(String::from(
                    "execution event stream closed before completion",
                )))
            }
        }
    }
}

fn evaluation_result(submission: ExecutionSubmission) -> ClientResult<CodeEvaluationResult> {
    let ExecutionSubmission::Completed(result) = submission else {
        return Err(ClientError::Sidecar(String::from(
            "evaluation unexpectedly returned a background process",
        )));
    };
    let value = result
        .evaluation_value
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
        .map_err(|error| {
            ClientError::Sidecar(format!("failed to decode evaluation result: {error}"))
        })?;
    Ok(CodeEvaluationResult { result, value })
}

fn typescript_check_result(result: CodeExecutionResult) -> ClientResult<TypeScriptCheckResult> {
    if result.outcome != wire::ExecutionOutcome::Succeeded {
        return Ok(TypeScriptCheckResult {
            result,
            has_errors: None,
            diagnostics: Vec::new(),
        });
    }
    let data: serde_json::Value = result
        .type_script_check_result
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| {
            ClientError::Sidecar(format!(
                "failed to decode TypeScript checker result: {error}"
            ))
        })?
        .ok_or_else(|| {
            ClientError::Sidecar(String::from(
                "TypeScript checker returned no diagnostic result",
            ))
        })?;
    let data = data.as_object().ok_or_else(|| {
        ClientError::Sidecar(String::from(
            "TypeScript checker returned an invalid diagnostic result",
        ))
    })?;
    let has_errors = data
        .get("hasErrors")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            ClientError::Sidecar(String::from(
                "TypeScript checker returned an invalid hasErrors value",
            ))
        })?;
    let diagnostics = data
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ClientError::Sidecar(String::from(
                "TypeScript checker returned invalid diagnostics",
            ))
        })?
        .iter()
        .map(|diagnostic| {
            let code = diagnostic
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .and_then(|code| u32::try_from(code).ok())
                .ok_or_else(|| {
                    ClientError::Sidecar(String::from(
                        "TypeScript checker returned an invalid diagnostic code",
                    ))
                })?;
            let category = diagnostic
                .get("category")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ClientError::Sidecar(String::from(
                        "TypeScript checker returned an invalid diagnostic category",
                    ))
                })?
                .to_owned();
            let message = diagnostic
                .get("message")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ClientError::Sidecar(String::from(
                        "TypeScript checker returned an invalid diagnostic message",
                    ))
                })?
                .to_owned();
            Ok(wire::TypeScriptDiagnostic {
                code,
                category,
                message,
                file_path: diagnostic
                    .get("filePath")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                line: diagnostic
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|line| u32::try_from(line).ok()),
                column: diagnostic
                    .get("column")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|column| u32::try_from(column).ok()),
            })
        })
        .collect::<ClientResult<Vec<_>>>()?;
    Ok(TypeScriptCheckResult {
        result,
        has_errors: Some(has_errors),
        diagnostics,
    })
}

fn completed_submission(submission: ExecutionSubmission) -> ClientResult<CodeExecutionResult> {
    match submission {
        ExecutionSubmission::Completed(result) => Ok(result),
        ExecutionSubmission::Background(_) => Err(ClientError::Sidecar(String::from(
            "attached operation unexpectedly returned a background process",
        ))),
    }
}

fn background_submission(
    client: &AgentOs,
    submission: ExecutionSubmission,
    language: &str,
) -> ClientResult<ProcessDescriptor> {
    match submission {
        ExecutionSubmission::Background(descriptor) => {
            let pid = descriptor.pid.ok_or_else(|| {
                ClientError::Sidecar(String::from(
                    "spawned process admission returned no numeric pid",
                ))
            })?;
            let process_id = descriptor.process_id.clone().ok_or_else(|| {
                ClientError::Sidecar(String::from(
                    "spawned process admission returned no process routing id",
                ))
            })?;
            let (stdout_tx, _) = broadcast::channel::<Vec<u8>>(1024);
            let (stderr_tx, _) = broadcast::channel::<Vec<u8>>(1024);
            let (output_tx, _) = broadcast::channel::<ProcessOutput>(1024);
            let (exit_tx, _) = watch::channel::<Option<i32>>(None);
            let (kernel_pid_tx, _) = watch::channel(Some(pid));
            let entry = ProcessEntry {
                command: format!("{language} source"),
                args: Vec::new(),
                stdout_tx: stdout_tx.clone(),
                stderr_tx: stderr_tx.clone(),
                output_tx: output_tx.clone(),
                exit_tx: exit_tx.clone(),
                process_id: process_id.clone(),
                kernel_pid: kernel_pid_tx,
                output_tasks: Vec::new(),
                started_at: descriptor.created_at_ms as i64,
            };
            let _ = client.inner().processes.insert(pid, entry);
            let mut events = client.transport().subscribe_wire_events();
            let operation_id = descriptor.execution_id.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((_, event)) = events.recv().await else {
                        break;
                    };
                    match event {
                        wire::EventPayload::ExecutionOutputEvent(output)
                            if output.execution_id == operation_id =>
                        {
                            let (stream, tx) = match output.channel {
                                wire::ExecutionStreamChannel::Stdout => {
                                    (ProcessStream::Stdout, &stdout_tx)
                                }
                                wire::ExecutionStreamChannel::Stderr
                                | wire::ExecutionStreamChannel::Pty => {
                                    (ProcessStream::Stderr, &stderr_tx)
                                }
                            };
                            let _ = tx.send(output.chunk.clone());
                            let _ = output_tx.send(ProcessOutput {
                                pid,
                                stream,
                                data: output.chunk,
                            });
                        }
                        wire::EventPayload::ExecutionCompletedEvent(completed)
                            if completed.execution_id == operation_id =>
                        {
                            let _ = exit_tx.send(Some(completed.exit_code.unwrap_or(1)));
                            break;
                        }
                        _ => {}
                    }
                }
            });
            Ok(ProcessDescriptor {
                pid,
                state: String::from("running"),
                language: Some(language.to_owned()),
                started_at_ms: descriptor.created_at_ms,
            })
        }
        ExecutionSubmission::Completed(_) => Err(ClientError::Sidecar(String::from(
            "spawned process unexpectedly returned an attached result",
        ))),
    }
}
