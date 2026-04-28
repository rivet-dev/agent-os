use crate::protocol::{
    PermissionMode, PermissionsPolicy, RegisterToolkitRequest, RegisteredToolDefinition,
    RequestFrame, ResponsePayload, ToolInvocationRequest, ToolkitRegisteredResponse,
};
use crate::service::{evaluate_permissions_policy, kernel_error, normalize_path, DispatchResult};
use crate::state::{BridgeError, VmState, TOOL_DRIVER_NAME, TOOL_MASTER_COMMAND};
use crate::{NativeSidecar, NativeSidecarBridge, SidecarError};
use agent_os_kernel::command_registry::CommandDriver;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const DEFAULT_TOOL_TIMEOUT_MS: u64 = 30_000;
pub(crate) const MAX_TOOL_DESCRIPTION_LENGTH: usize = 200;
const TOOL_INVOKE_CAPABILITY: &str = "tool.invoke";

#[derive(Debug)]
pub(crate) enum ToolCommandResolution {
    Immediate {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: i32,
    },
    Invoke {
        request: ToolInvocationRequest,
        timeout: Duration,
    },
}

pub(crate) fn format_tool_failure_output(message: &str) -> Vec<u8> {
    let mut output = message.as_bytes().to_vec();
    if !output.ends_with(b"\n") {
        output.push(b'\n');
    }
    output
}

pub(crate) fn register_toolkit<B>(
    sidecar: &mut NativeSidecar<B>,
    request: &RequestFrame,
    payload: RegisterToolkitRequest,
) -> Result<DispatchResult, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let (connection_id, session_id, vm_id) = sidecar.vm_scope_for(&request.ownership)?;
    sidecar.require_owned_vm(&connection_id, &session_id, &vm_id)?;

    validate_toolkit_registration(&payload)?;

    let registered_name = payload.name.clone();
    let (original_permissions, original_toolkits, original_command_guest_paths) = {
        let vm = sidecar.vms.get(&vm_id).expect("owned VM should exist");
        (
            vm.configuration.permissions.clone(),
            vm.toolkits.clone(),
            vm.command_guest_paths.clone(),
        )
    };
    sidecar
        .bridge
        .set_vm_permissions(&vm_id, &PermissionsPolicy::allow_all())?;
    let registration_result = (|| -> Result<_, SidecarError> {
        let vm = sidecar.vms.get_mut(&vm_id).expect("owned VM should exist");
        ensure_toolkit_name_available(&vm.toolkits, &registered_name)?;
        vm.toolkits.insert(registered_name.clone(), payload);
        refresh_tool_registry(vm)?;
        Ok::<_, SidecarError>((
            tool_command_names(vm).len() as u32,
            generate_tool_reference(vm.toolkits.values()),
        ))
    })();
    let (command_count, prompt_markdown) = match registration_result {
        Ok(result) => {
            sidecar
                .bridge
                .set_vm_permissions(&vm_id, &original_permissions)?;
            result
        }
        Err(error) => {
            let vm = sidecar.vms.get_mut(&vm_id).expect("owned VM should exist");
            vm.toolkits = original_toolkits;
            vm.command_guest_paths = original_command_guest_paths;
            match sidecar.bridge.restore_vm_permissions_fail_closed(
                &vm_id,
                &original_permissions,
                "toolkit registration rollback",
                &error,
            ) {
                Ok(()) => return Err(error),
                Err(rollback_error) => {
                    vm.configuration.permissions = PermissionsPolicy::deny_all();
                    return Err(rollback_error);
                }
            }
        }
    };

    Ok(DispatchResult {
        response: sidecar.respond(
            request,
            ResponsePayload::ToolkitRegistered(ToolkitRegisteredResponse {
                toolkit: registered_name,
                command_count,
                prompt_markdown,
            }),
        ),
        events: Vec::new(),
    })
}

fn refresh_tool_registry(vm: &mut VmState) -> Result<(), SidecarError> {
    let commands = tool_command_names(vm);
    vm.kernel
        .register_driver(CommandDriver::new(
            TOOL_DRIVER_NAME,
            commands.iter().cloned(),
        ))
        .map_err(kernel_error)?;

    for command in commands {
        vm.command_guest_paths
            .insert(command.clone(), format!("/bin/{command}"));
    }
    Ok(())
}

pub(crate) fn resolve_tool_command(
    vm: &mut VmState,
    command: &str,
    args: &[String],
    cwd: Option<&str>,
) -> Result<Option<ToolCommandResolution>, SidecarError> {
    let Some(kind) = identify_tool_command(vm, command) else {
        return Ok(None);
    };
    let guest_cwd = cwd
        .map(normalize_path)
        .unwrap_or_else(|| vm.guest_cwd.clone());
    let resolution = match kind {
        ToolCommand::Master => resolve_master_command(vm, args, &guest_cwd)?,
        ToolCommand::Toolkit(toolkit_name) => {
            resolve_toolkit_command(vm, &toolkit_name, args, &guest_cwd)?
        }
    };
    Ok(Some(resolution))
}

pub(crate) fn is_tool_command(vm: &VmState, command: &str) -> bool {
    identify_tool_command(vm, command).is_some()
}

pub(crate) fn normalized_tool_command_name(command: &str) -> Option<String> {
    tool_command_name_from_specifier(command).map(ToOwned::to_owned)
}

fn identify_tool_command(vm: &VmState, command: &str) -> Option<ToolCommand> {
    let command_name = tool_command_name_from_specifier(command).unwrap_or(command);

    if command_name == TOOL_MASTER_COMMAND {
        return Some(ToolCommand::Master);
    }

    command_name
        .strip_prefix(&format!("{TOOL_MASTER_COMMAND}-"))
        .filter(|toolkit_name| vm.toolkits.contains_key(*toolkit_name))
        .map(|toolkit_name| ToolCommand::Toolkit(toolkit_name.to_owned()))
}

fn tool_command_name_from_specifier<'a>(command: &'a str) -> Option<&'a str> {
    let file_name = Path::new(command).file_name()?.to_str()?;
    let normalized = normalize_path(command);
    let registered_internal_path = normalized
        .strip_prefix("/__agentos/commands/")
        .and_then(|suffix| suffix.rsplit('/').next())
        .is_some_and(|name| name == file_name);
    if !matches!(
        normalized.as_str(),
        path if path == format!("/bin/{file_name}")
            || path == format!("/usr/bin/{file_name}")
            || path == format!("/usr/local/bin/{file_name}")
    ) && !registered_internal_path
    {
        return None;
    }
    Some(file_name)
}

fn resolve_master_command(
    vm: &mut VmState,
    args: &[String],
    guest_cwd: &str,
) -> Result<ToolCommandResolution, SidecarError> {
    if args.is_empty() || is_help_flag(&args[0]) {
        return Ok(ToolCommandResolution::Immediate {
            stdout: master_help_text().into_bytes(),
            stderr: Vec::new(),
            exit_code: 0,
        });
    }

    if args[0] == "list-tools" {
        return if let Some(toolkit_name) = args.get(1) {
            Ok(ToolCommandResolution::Immediate {
                stdout: serialize_json_output(list_toolkit_payload(vm, toolkit_name)?),
                stderr: Vec::new(),
                exit_code: 0,
            })
        } else {
            Ok(ToolCommandResolution::Immediate {
                stdout: serialize_json_output(list_toolkits_payload(vm)),
                stderr: Vec::new(),
                exit_code: 0,
            })
        };
    }

    let toolkit_name = &args[0];
    if !vm.toolkits.contains_key(toolkit_name) {
        return Ok(ToolCommandResolution::Immediate {
            stdout: Vec::new(),
            stderr: format_tool_failure_output(&format!(
                "No toolkit \"{toolkit_name}\". Available: {}",
                toolkit_names(vm)
            )),
            exit_code: 1,
        });
    }

    if args.len() == 1 || is_help_flag(&args[1]) {
        return Ok(ToolCommandResolution::Immediate {
            stdout: serialize_json_output(describe_toolkit_payload(vm, toolkit_name)?),
            stderr: Vec::new(),
            exit_code: 0,
        });
    }

    if args.len() >= 3 && is_help_flag(&args[2]) {
        return Ok(ToolCommandResolution::Immediate {
            stdout: serialize_json_output(describe_tool_payload(vm, toolkit_name, &args[1])?),
            stderr: Vec::new(),
            exit_code: 0,
        });
    }

    Ok(build_invocation_resolution(
        vm,
        toolkit_name,
        &args[1],
        &args[2..],
        guest_cwd,
    ))
}

fn resolve_toolkit_command(
    vm: &mut VmState,
    toolkit_name: &str,
    args: &[String],
    guest_cwd: &str,
) -> Result<ToolCommandResolution, SidecarError> {
    if args.is_empty() || is_help_flag(&args[0]) {
        return Ok(ToolCommandResolution::Immediate {
            stdout: serialize_json_output(describe_toolkit_payload(vm, toolkit_name)?),
            stderr: Vec::new(),
            exit_code: 0,
        });
    }

    if args.len() >= 2 && is_help_flag(&args[1]) {
        return Ok(ToolCommandResolution::Immediate {
            stdout: serialize_json_output(describe_tool_payload(vm, toolkit_name, &args[0])?),
            stderr: Vec::new(),
            exit_code: 0,
        });
    }

    Ok(build_invocation_resolution(
        vm,
        toolkit_name,
        &args[0],
        &args[1..],
        guest_cwd,
    ))
}

fn build_invocation_resolution(
    vm: &mut VmState,
    toolkit_name: &str,
    tool_name: &str,
    cli_args: &[String],
    guest_cwd: &str,
) -> ToolCommandResolution {
    let Some(toolkit) = vm.toolkits.get(toolkit_name).cloned() else {
        return ToolCommandResolution::Immediate {
            stdout: Vec::new(),
            stderr: format_tool_failure_output(&format!(
                "No toolkit \"{toolkit_name}\". Available: {}",
                toolkit_names(vm)
            )),
            exit_code: 1,
        };
    };
    let Some(tool) = toolkit.tools.get(tool_name).cloned() else {
        return ToolCommandResolution::Immediate {
            stdout: Vec::new(),
            stderr: format_tool_failure_output(&format!(
                "No tool \"{tool_name}\" in toolkit \"{toolkit_name}\". Available: {}",
                tool_names(&toolkit)
            )),
            exit_code: 1,
        };
    };
    let permission_mode =
        tool_invocation_permission_mode(&vm.configuration.permissions, toolkit_name, tool_name);
    if permission_mode != PermissionMode::Allow {
        return denied_tool_invocation_resolution(permission_mode, toolkit_name, tool_name);
    }
    let input = match resolve_invocation_input(vm, &tool, cli_args, guest_cwd) {
        Ok(input) => input,
        Err(message) => {
            return ToolCommandResolution::Immediate {
                stdout: Vec::new(),
                stderr: format_tool_failure_output(&message),
                exit_code: 1,
            };
        }
    };
    let timeout_ms = tool.timeout_ms.unwrap_or(DEFAULT_TOOL_TIMEOUT_MS);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    ToolCommandResolution::Invoke {
        request: ToolInvocationRequest {
            invocation_id: format!("{toolkit_name}:{tool_name}:{nonce}"),
            tool_key: format!("{toolkit_name}:{tool_name}"),
            input,
            timeout_ms,
        },
        timeout: Duration::from_millis(timeout_ms),
    }
}

fn ensure_toolkit_name_available(
    toolkits: &BTreeMap<String, RegisterToolkitRequest>,
    toolkit_name: &str,
) -> Result<(), SidecarError> {
    if toolkits.contains_key(toolkit_name) {
        return Err(SidecarError::Conflict(format!(
            "toolkit already registered: {toolkit_name}"
        )));
    }
    Ok(())
}

pub(crate) fn tool_invocation_permission_mode(
    permissions: &PermissionsPolicy,
    toolkit_name: &str,
    tool_name: &str,
) -> PermissionMode {
    let tool_key = tool_invocation_resource(toolkit_name, tool_name);
    evaluate_permissions_policy(permissions, "tool", TOOL_INVOKE_CAPABILITY, Some(&tool_key))
}

fn denied_tool_invocation_resolution(
    permission_mode: PermissionMode,
    toolkit_name: &str,
    tool_name: &str,
) -> ToolCommandResolution {
    let tool_key = tool_invocation_resource(toolkit_name, tool_name);
    let message = match permission_mode {
        PermissionMode::Allow => unreachable!("allowed tool invocations should not be denied"),
        PermissionMode::Ask => {
            format!("EACCES: permission prompt required for {TOOL_INVOKE_CAPABILITY} on {tool_key}")
        }
        PermissionMode::Deny => {
            format!("EACCES: blocked by {TOOL_INVOKE_CAPABILITY} policy for {tool_key}")
        }
    };
    ToolCommandResolution::Immediate {
        stdout: Vec::new(),
        stderr: format_tool_failure_output(&message),
        exit_code: 1,
    }
}

fn tool_invocation_resource(toolkit_name: &str, tool_name: &str) -> String {
    format!("{toolkit_name}:{tool_name}")
}

fn resolve_invocation_input(
    vm: &mut VmState,
    tool: &RegisteredToolDefinition,
    cli_args: &[String],
    guest_cwd: &str,
) -> Result<Value, String> {
    if cli_args.first().is_some_and(|arg| arg == "--json") {
        let value = cli_args
            .get(1)
            .ok_or_else(|| String::from("Flag --json requires a value"))?;
        let input = serde_json::from_str(value)
            .map_err(|error| format!("Invalid JSON for --json: {error}"))?;
        validate_tool_input(&tool.input_schema, &input).map_err(|error| error.to_string())?;
        return Ok(input);
    }

    if cli_args.first().is_some_and(|arg| arg == "--json-file") {
        let path = cli_args
            .get(1)
            .ok_or_else(|| String::from("Flag --json-file requires a value"))?;
        let guest_path = if path.starts_with('/') {
            normalize_path(path)
        } else {
            normalize_path(&format!("{guest_cwd}/{path}"))
        };
        let bytes = vm
            .kernel
            .read_file(&guest_path)
            .map_err(|error| format!("Invalid JSON file: {error}"))?;
        let text =
            String::from_utf8(bytes).map_err(|error| format!("Invalid JSON file: {error}"))?;
        let input =
            serde_json::from_str(&text).map_err(|error| format!("Invalid JSON file: {error}"))?;
        validate_tool_input(&tool.input_schema, &input).map_err(|error| error.to_string())?;
        return Ok(input);
    }

    parse_argv(&tool.input_schema, cli_args)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolInputSchemaViolation {
    path: String,
    expected: String,
    actual: String,
}

impl ToolInputSchemaViolation {
    fn new(
        path: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            expected: expected.into(),
            actual: actual.into(),
        }
    }
}

impl fmt::Display for ToolInputSchemaViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ToolInputSchemaViolation at {}: expected {}, got {}",
            self.path, self.expected, self.actual
        )
    }
}

fn validate_tool_input(schema: &Value, input: &Value) -> Result<(), ToolInputSchemaViolation> {
    validate_tool_input_at_path(schema, input, "$")
}

fn validate_tool_input_at_path(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), ToolInputSchemaViolation> {
    if schema.is_null() || schema.as_object().is_some_and(|object| object.is_empty()) {
        return Ok(());
    }

    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        return validate_schema_branches(branches, input, path, "anyOf");
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        return validate_schema_branches(branches, input, path, "oneOf");
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if enum_values.iter().any(|candidate| candidate == input) {
            return Ok(());
        }
        return Err(ToolInputSchemaViolation::new(
            path,
            format!(
                "one of {}",
                enum_values
                    .iter()
                    .map(compact_json)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            describe_value(input),
        ));
    }

    if let Some(expected) = schema.get("const") {
        if expected == input {
            return Ok(());
        }
        return Err(ToolInputSchemaViolation::new(
            path,
            format!("constant {}", compact_json(expected)),
            describe_value(input),
        ));
    }

    match schema.get("type") {
        Some(Value::String(expected_type)) => {
            validate_typed_tool_input(schema, input, path, expected_type)
        }
        Some(Value::Array(expected_types)) => {
            let mut first_error = None;
            for expected_type in expected_types.iter().filter_map(Value::as_str) {
                match validate_typed_tool_input(schema, input, path, expected_type) {
                    Ok(()) => return Ok(()),
                    Err(error) if first_error.is_none() => first_error = Some(error),
                    Err(_) => {}
                }
            }
            Err(first_error.unwrap_or_else(|| {
                ToolInputSchemaViolation::new(
                    path,
                    describe_expected(schema),
                    describe_value(input),
                )
            }))
        }
        Some(_) => Ok(()),
        None if has_object_keywords(schema) => {
            validate_typed_tool_input(schema, input, path, "object")
        }
        None => Ok(()),
    }
}

fn validate_schema_branches(
    branches: &[Value],
    input: &Value,
    path: &str,
    keyword: &str,
) -> Result<(), ToolInputSchemaViolation> {
    let mut first_error = None;
    for branch in branches {
        match validate_tool_input_at_path(branch, input, path) {
            Ok(()) => return Ok(()),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }

    Err(first_error.unwrap_or_else(|| {
        ToolInputSchemaViolation::new(
            path,
            format!(
                "{keyword} branch ({})",
                branches
                    .iter()
                    .map(describe_expected)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            describe_value(input),
        )
    }))
}

fn validate_typed_tool_input(
    schema: &Value,
    input: &Value,
    path: &str,
    expected_type: &str,
) -> Result<(), ToolInputSchemaViolation> {
    match expected_type {
        "null" => {
            if input.is_null() {
                Ok(())
            } else {
                Err(type_violation(path, expected_type, input))
            }
        }
        "boolean" => {
            if input.is_boolean() {
                Ok(())
            } else {
                Err(type_violation(path, expected_type, input))
            }
        }
        "string" => validate_string_tool_input(schema, input, path),
        "number" => validate_number_tool_input(schema, input, path, false),
        "integer" => validate_number_tool_input(schema, input, path, true),
        "array" => validate_array_tool_input(schema, input, path),
        "object" => validate_object_tool_input(schema, input, path),
        _ => Ok(()),
    }
}

fn validate_string_tool_input(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), ToolInputSchemaViolation> {
    let Some(value) = input.as_str() else {
        return Err(type_violation(path, "string", input));
    };

    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64) {
        if value.chars().count() < min_length as usize {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!("string with minLength {min_length}"),
                format!("string length {}", value.chars().count()),
            ));
        }
    }

    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64) {
        if value.chars().count() > max_length as usize {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!("string with maxLength {max_length}"),
                format!("string length {}", value.chars().count()),
            ));
        }
    }

    Ok(())
}

fn validate_number_tool_input(
    schema: &Value,
    input: &Value,
    path: &str,
    expect_integer: bool,
) -> Result<(), ToolInputSchemaViolation> {
    let Some(number) = input.as_f64() else {
        return Err(type_violation(
            path,
            if expect_integer { "integer" } else { "number" },
            input,
        ));
    };

    if expect_integer && number.fract() != 0.0 {
        return Err(type_violation(path, "integer", input));
    }

    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if number < minimum {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!(
                    "{} >= {}",
                    if expect_integer { "integer" } else { "number" },
                    minimum
                ),
                compact_json(input),
            ));
        }
    }

    if let Some(minimum) = schema.get("exclusiveMinimum").and_then(Value::as_f64) {
        if number <= minimum {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!(
                    "{} > {}",
                    if expect_integer { "integer" } else { "number" },
                    minimum
                ),
                compact_json(input),
            ));
        }
    }

    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if number > maximum {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!(
                    "{} <= {}",
                    if expect_integer { "integer" } else { "number" },
                    maximum
                ),
                compact_json(input),
            ));
        }
    }

    if let Some(maximum) = schema.get("exclusiveMaximum").and_then(Value::as_f64) {
        if number >= maximum {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!(
                    "{} < {}",
                    if expect_integer { "integer" } else { "number" },
                    maximum
                ),
                compact_json(input),
            ));
        }
    }

    Ok(())
}

fn validate_array_tool_input(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), ToolInputSchemaViolation> {
    let Some(items) = input.as_array() else {
        return Err(type_violation(path, "array", input));
    };

    if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64) {
        if items.len() < min_items as usize {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!("array with minItems {min_items}"),
                format!("array length {}", items.len()),
            ));
        }
    }

    if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
        if items.len() > max_items as usize {
            return Err(ToolInputSchemaViolation::new(
                path,
                format!("array with maxItems {max_items}"),
                format!("array length {}", items.len()),
            ));
        }
    }

    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            validate_tool_input_at_path(item_schema, item, &format!("{path}[{index}]"))?;
        }
    }

    Ok(())
}

fn validate_object_tool_input(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), ToolInputSchemaViolation> {
    let Some(object) = input.as_object() else {
        return Err(type_violation(path, "object", input));
    };

    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for field in required.iter().filter_map(Value::as_str) {
        if !object.contains_key(field) {
            let field_path = format!("{path}.{field}");
            let expected = properties
                .get(field)
                .map(describe_expected)
                .unwrap_or_else(|| String::from("required value"));
            return Err(ToolInputSchemaViolation::new(
                field_path,
                expected,
                "missing value",
            ));
        }
    }

    for (field, value) in object {
        let field_path = format!("{path}.{field}");
        if let Some(field_schema) = properties.get(field) {
            validate_tool_input_at_path(field_schema, value, &field_path)?;
            continue;
        }

        match schema.get("additionalProperties") {
            Some(Value::Bool(false)) => {
                return Err(ToolInputSchemaViolation::new(
                    field_path,
                    "no additional properties",
                    describe_value(value),
                ));
            }
            Some(additional_schema) => {
                validate_tool_input_at_path(additional_schema, value, &field_path)?;
            }
            None => {}
        }
    }

    Ok(())
}

fn has_object_keywords(schema: &Value) -> bool {
    schema.get("properties").is_some()
        || schema.get("required").is_some()
        || schema.get("additionalProperties").is_some()
}

fn type_violation(path: &str, expected: &str, input: &Value) -> ToolInputSchemaViolation {
    ToolInputSchemaViolation::new(path, expected, describe_value(input))
}

fn describe_expected(schema: &Value) -> String {
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        return format!(
            "one of {}",
            enum_values
                .iter()
                .map(compact_json)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if let Some(expected) = schema.get("const") {
        return format!("constant {}", compact_json(expected));
    }

    match schema.get("type") {
        Some(Value::String(expected_type)) => expected_type.clone(),
        Some(Value::Array(expected_types)) => expected_types
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" | "),
        _ if has_object_keywords(schema) => String::from("object"),
        _ => String::from("value"),
    }
}

fn describe_value(value: &Value) -> String {
    match value {
        Value::Null => String::from("null"),
        Value::Bool(_) => String::from("boolean"),
        Value::Number(number) => {
            let is_integer = number.as_i64().is_some()
                || number.as_u64().is_some()
                || number.as_f64().is_some_and(|float| float.fract() == 0.0);
            if is_integer {
                String::from("integer")
            } else {
                String::from("number")
            }
        }
        Value::String(_) => String::from("string"),
        Value::Array(_) => String::from("array"),
        Value::Object(_) => String::from("object"),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("<invalid json>"))
}

fn parse_argv(schema: &Value, argv: &[String]) -> Result<Value, String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    if properties.is_empty() && argv.is_empty() {
        return Ok(Value::Object(Map::new()));
    }

    let mut flag_to_field = BTreeMap::new();
    for (field_name, field_schema) in &properties {
        flag_to_field.insert(
            camel_to_kebab(field_name),
            (field_name.clone(), field_schema),
        );
    }

    let mut input = Map::new();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if !arg.starts_with("--") {
            return Err(format!("Unexpected positional argument: \"{arg}\""));
        }

        let raw_flag = &arg[2..];
        if let Some(flag_name) = raw_flag.strip_prefix("no-") {
            if let Some((field_name, field_schema)) = flag_to_field.get(flag_name) {
                if json_schema_type(field_schema) == Some("boolean") {
                    input.insert(field_name.clone(), Value::Bool(false));
                    index += 1;
                    continue;
                }
            }
            if !flag_to_field.contains_key(flag_name) {
                return Err(format!("Unknown flag: --{raw_flag}"));
            }
        }

        let Some((field_name, field_schema)) = flag_to_field.get(raw_flag) else {
            return Err(format!("Unknown flag: --{raw_flag}"));
        };

        match json_schema_type(field_schema) {
            Some("boolean") => {
                input.insert(field_name.clone(), Value::Bool(true));
                index += 1;
            }
            Some("number") | Some("integer") => {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| format!("Flag --{raw_flag} requires a value"))?;
                let number = value
                    .parse::<f64>()
                    .map_err(|_| format!("Flag --{raw_flag} expects a number, got \"{value}\""))?;
                let number = serde_json::Number::from_f64(number).ok_or_else(|| {
                    format!("Flag --{raw_flag} expects a finite number, got \"{value}\"")
                })?;
                input.insert(field_name.clone(), Value::Number(number));
                index += 2;
            }
            Some("array") => {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| format!("Flag --{raw_flag} requires a value"))?;
                let item_schema = field_schema.get("items").unwrap_or(&Value::Null);
                let parsed_value = match json_schema_type(item_schema) {
                    Some("number") | Some("integer") => {
                        let number = value.parse::<f64>().map_err(|_| {
                            format!("Flag --{raw_flag} expects a number value, got \"{value}\"")
                        })?;
                        let number = serde_json::Number::from_f64(number).ok_or_else(|| {
                            format!(
                                "Flag --{raw_flag} expects a finite number value, got \"{value}\""
                            )
                        })?;
                        Value::Number(number)
                    }
                    _ => Value::String(value.clone()),
                };
                input
                    .entry(field_name.clone())
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .expect("array field should always contain an array")
                    .push(parsed_value);
                index += 2;
            }
            _ => {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| format!("Flag --{raw_flag} requires a value"))?;
                input.insert(field_name.clone(), Value::String(value.clone()));
                index += 2;
            }
        }
    }

    for field_name in required {
        if !input.contains_key(&field_name) {
            return Err(format!(
                "Missing required flag: --{}",
                camel_to_kebab(&field_name)
            ));
        }
    }

    Ok(Value::Object(input))
}

fn json_schema_type(schema: &Value) -> Option<&str> {
    schema.get("type").and_then(Value::as_str)
}

fn list_toolkits_payload(vm: &VmState) -> Value {
    json!({
        "ok": true,
        "result": {
            "toolkits": vm.toolkits.values().map(|toolkit| {
                json!({
                    "name": toolkit.name,
                    "description": toolkit.description,
                    "tools": toolkit.tools.keys().cloned().collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        }
    })
}

fn list_toolkit_payload(vm: &VmState, toolkit_name: &str) -> Result<Value, SidecarError> {
    let toolkit = vm.toolkits.get(toolkit_name).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "No toolkit \"{toolkit_name}\". Available: {}",
            toolkit_names(vm)
        ))
    })?;

    Ok(json!({
        "ok": true,
        "result": {
            "name": toolkit.name,
            "description": toolkit.description,
            "tools": toolkit.tools.iter().map(|(name, tool)| (
                name.clone(),
                json!({
                    "description": tool.description,
                    "flags": describe_flags(&tool.input_schema),
                })
            )).collect::<BTreeMap<_, _>>(),
        }
    }))
}

fn describe_toolkit_payload(vm: &VmState, toolkit_name: &str) -> Result<Value, SidecarError> {
    let toolkit = vm.toolkits.get(toolkit_name).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "No toolkit \"{toolkit_name}\". Available: {}",
            toolkit_names(vm)
        ))
    })?;

    Ok(json!({
        "ok": true,
        "result": {
            "name": toolkit.name,
            "description": toolkit.description,
            "tools": toolkit.tools.iter().map(|(name, tool)| (
                name.clone(),
                json!({
                    "description": tool.description,
                    "flags": describe_flags(&tool.input_schema),
                    "examples": tool.examples.iter().map(|example| {
                        json!({
                            "description": example.description,
                            "input": example.input,
                        })
                    }).collect::<Vec<_>>(),
                })
            )).collect::<BTreeMap<_, _>>(),
        }
    }))
}

fn describe_tool_payload(
    vm: &VmState,
    toolkit_name: &str,
    tool_name: &str,
) -> Result<Value, SidecarError> {
    let toolkit = vm.toolkits.get(toolkit_name).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "No toolkit \"{toolkit_name}\". Available: {}",
            toolkit_names(vm)
        ))
    })?;
    let tool = toolkit.tools.get(tool_name).ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "No tool \"{tool_name}\" in toolkit \"{toolkit_name}\". Available: {}",
            tool_names(toolkit)
        ))
    })?;

    Ok(json!({
        "ok": true,
        "result": {
            "toolkit": toolkit_name,
            "tool": tool_name,
            "description": tool.description,
            "flags": describe_flags(&tool.input_schema),
            "examples": tool.examples.iter().map(|example| {
                json!({
                    "description": example.description,
                    "input": example.input,
                })
            }).collect::<Vec<_>>(),
        }
    }))
}

fn describe_flags(schema: &Value) -> Vec<Value> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    properties
        .into_iter()
        .map(|(field_name, field_schema)| {
            let field_type = match json_schema_type(&field_schema) {
                Some("array") => {
                    let item_type =
                        json_schema_type(field_schema.get("items").unwrap_or(&Value::Null))
                            .unwrap_or("string");
                    format!("{item_type}[]")
                }
                Some("string") => {
                    if let Some(enum_values) = field_schema.get("enum").and_then(Value::as_array) {
                        let values = enum_values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>();
                        if values.is_empty() {
                            String::from("string")
                        } else {
                            values.join("|")
                        }
                    } else {
                        String::from("string")
                    }
                }
                Some(other) => other.to_owned(),
                None => String::from("string"),
            };

            json!({
                "flag": format!("--{}", camel_to_kebab(&field_name)),
                "type": field_type,
                "required": required.contains(&field_name),
                "description": field_schema.get("description").and_then(Value::as_str),
            })
        })
        .collect()
}

pub(crate) fn generate_tool_reference<'a>(
    toolkits: impl IntoIterator<Item = &'a RegisterToolkitRequest>,
) -> String {
    let toolkits = toolkits.into_iter().collect::<Vec<_>>();
    if toolkits.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        String::from("## Available Host Tools"),
        String::new(),
        String::from("Run `agentos list-tools` to see all available tools."),
        String::new(),
    ];

    for toolkit in toolkits {
        lines.push(format!("### {}", toolkit.name));
        lines.push(String::new());
        lines.push(toolkit.description.clone());
        lines.push(String::new());
        for (tool_name, tool) in &toolkit.tools {
            let signature = build_flag_signature(&tool.input_schema);
            let suffix = if signature.is_empty() {
                String::new()
            } else {
                format!(" {signature}")
            };
            lines.push(format!(
                "- `{} {}{}` — {}",
                toolkit_command_name(&toolkit.name),
                tool_name,
                suffix,
                tool.description
            ));
        }
        lines.push(String::new());

        let tools_with_examples = toolkit
            .tools
            .iter()
            .filter(|(_, tool)| !tool.examples.is_empty())
            .collect::<Vec<_>>();
        if !tools_with_examples.is_empty() {
            lines.push(String::from("**Examples:**"));
            lines.push(String::new());
            for (tool_name, tool) in tools_with_examples {
                for example in &tool.examples {
                    let args = input_to_flags(&example.input);
                    let suffix = if args.is_empty() {
                        String::new()
                    } else {
                        format!(" {args}")
                    };
                    lines.push(format!(
                        "- {}: `{} {}{}`",
                        example.description,
                        toolkit_command_name(&toolkit.name),
                        tool_name,
                        suffix
                    ));
                }
            }
            lines.push(String::new());
        }

        lines.push(format!(
            "Run `{} <tool> --help` for details.",
            toolkit_command_name(&toolkit.name)
        ));
        lines.push(String::new());
    }

    lines.join("\n")
}

fn build_flag_signature(schema: &Value) -> String {
    describe_flags(schema)
        .into_iter()
        .map(|flag| {
            let name = flag["flag"].as_str().unwrap_or("--arg");
            let field_type = flag["type"].as_str().unwrap_or("string");
            if flag["required"].as_bool().unwrap_or(false) {
                format!("{name} <{field_type}>")
            } else {
                format!("[{name} <{field_type}>]")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn input_to_flags(input: &Value) -> String {
    let Some(object) = input.as_object() else {
        return String::new();
    };

    let mut flags = Vec::new();
    for (key, value) in object {
        let flag = format!("--{}", camel_to_kebab(key));
        match value {
            Value::Bool(true) => flags.push(flag),
            Value::Bool(false) => flags.push(format!("--no-{}", camel_to_kebab(key))),
            Value::Array(values) => {
                for item in values {
                    flags.push(format!("{flag} {}", cli_string(item)));
                }
            }
            other => flags.push(format!("{flag} {}", cli_string(other))),
        }
    }
    flags.join(" ")
}

fn cli_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn serialize_json_output(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).expect("tool metadata payload should serialize")
}

fn toolkit_command_name(toolkit_name: &str) -> String {
    format!("{TOOL_MASTER_COMMAND}-{toolkit_name}")
}

fn tool_command_names(vm: &VmState) -> Vec<String> {
    let mut commands = vec![String::from(TOOL_MASTER_COMMAND)];
    commands.extend(
        vm.toolkits
            .keys()
            .map(|toolkit_name| toolkit_command_name(toolkit_name)),
    );
    commands
}

fn toolkit_names(vm: &VmState) -> String {
    vm.toolkits.keys().cloned().collect::<Vec<_>>().join(", ")
}

fn tool_names(toolkit: &RegisterToolkitRequest) -> String {
    toolkit.tools.keys().cloned().collect::<Vec<_>>().join(", ")
}

fn master_help_text() -> String {
    String::from(
        "Usage: agentos <command>\n\nCommands:\n  list-tools [toolkit]   List available toolkits and tools\n  <toolkit> --help       Describe one toolkit\n  <toolkit> <tool> ...   Run a host tool\n",
    )
}

fn is_help_flag(value: &str) -> bool {
    matches!(value, "--help" | "-h")
}

fn camel_to_kebab(value: &str) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() && index > 0 {
            output.push('-');
        }
        output.push(ch.to_ascii_lowercase());
    }
    output
}

fn validate_toolkit_name(name: &str) -> Result<(), SidecarError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(SidecarError::InvalidState(format!(
            "invalid toolkit name {name}; expected lowercase alphanumeric characters plus hyphens"
        )));
    }
    Ok(())
}

fn validate_tool_name(name: &str) -> Result<(), SidecarError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(SidecarError::InvalidState(format!(
            "invalid tool name {name}; expected lowercase alphanumeric characters plus hyphens"
        )));
    }
    Ok(())
}

fn validate_toolkit_registration(payload: &RegisterToolkitRequest) -> Result<(), SidecarError> {
    validate_toolkit_name(&payload.name)?;
    if payload.description.is_empty() {
        return Err(SidecarError::InvalidState(format!(
            "toolkit {} is missing a description",
            payload.name
        )));
    }
    validate_description_length(
        &format!("Toolkit \"{}\"", payload.name),
        &payload.description,
    )?;
    if payload.tools.is_empty() {
        return Err(SidecarError::InvalidState(format!(
            "toolkit {} must define at least one tool",
            payload.name
        )));
    }
    for (tool_name, tool) in &payload.tools {
        validate_tool_name(tool_name)?;
        if tool.description.is_empty() {
            return Err(SidecarError::InvalidState(format!(
                "tool {} in toolkit {} is missing a description",
                tool_name, payload.name
            )));
        }
        validate_description_length(
            &format!("Tool \"{}/{}\"", payload.name, tool_name),
            &tool.description,
        )?;
    }
    Ok(())
}

fn validate_description_length(label: &str, description: &str) -> Result<(), SidecarError> {
    if description.len() > MAX_TOOL_DESCRIPTION_LENGTH {
        return Err(SidecarError::InvalidState(format!(
            "{label} description is {} characters, max is {MAX_TOOL_DESCRIPTION_LENGTH}",
            description.len()
        )));
    }
    Ok(())
}

enum ToolCommand {
    Master,
    Toolkit(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn screenshot_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" },
                "fullPage": { "type": "boolean" },
                "width": { "type": "number" },
                "format": { "type": "string", "enum": ["png", "jpg"] },
                "tags": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["url"]
        })
    }

    #[test]
    fn parses_cli_flags_from_json_schema() {
        let parsed = parse_argv(
            &screenshot_schema(),
            &[
                String::from("--url"),
                String::from("https://example.com"),
                String::from("--full-page"),
                String::from("--width"),
                String::from("1920"),
                String::from("--tags"),
                String::from("hero"),
                String::from("--tags"),
                String::from("landing"),
            ],
        )
        .expect("parse argv");

        assert_eq!(
            parsed,
            json!({
                "url": "https://example.com",
                "fullPage": true,
                "width": 1920.0,
                "tags": ["hero", "landing"]
            })
        );
    }

    #[test]
    fn validates_json_tool_input_against_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 0 },
                "label": { "type": "string" }
            },
            "required": ["count", "label"],
            "additionalProperties": false,
        });

        validate_tool_input(&schema, &json!({ "count": 2, "label": "ok" }))
            .expect("valid input should pass");

        let error = validate_tool_input(&schema, &json!({ "count": "oops", "label": 4 }))
            .expect_err("wrong types should fail");
        assert_eq!(
            error.to_string(),
            "ToolInputSchemaViolation at $.count: expected integer, got string"
        );
    }

    #[test]
    fn rejects_numeric_bounds_and_additional_properties_for_json_tool_input() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 0 }
            },
            "required": ["count"],
            "additionalProperties": false,
        });

        let negative = validate_tool_input(&schema, &json!({ "count": -1 }))
            .expect_err("minimum should reject negative numbers");
        assert_eq!(
            negative.to_string(),
            "ToolInputSchemaViolation at $.count: expected integer >= 0, got -1"
        );

        let extra = validate_tool_input(&schema, &json!({ "count": 1, "extra": true }))
            .expect_err("unexpected properties should fail");
        assert_eq!(
            extra.to_string(),
            "ToolInputSchemaViolation at $.extra: expected no additional properties, got boolean"
        );
    }

    #[test]
    fn generates_prompt_markdown() {
        let markdown = generate_tool_reference([&RegisterToolkitRequest {
            name: String::from("browser"),
            description: String::from("Browser automation"),
            tools: BTreeMap::from([(
                String::from("screenshot"),
                RegisteredToolDefinition {
                    description: String::from("Take a screenshot"),
                    input_schema: screenshot_schema(),
                    timeout_ms: None,
                    examples: Vec::new(),
                },
            )]),
        }]);

        assert!(markdown.contains("## Available Host Tools"));
        assert!(markdown.contains("agentos list-tools"));
        assert!(markdown.contains("agentos-browser screenshot"));
        assert!(markdown.contains("--url <string>"));
    }

    fn registered_tool(description: String) -> RegisteredToolDefinition {
        RegisteredToolDefinition {
            description,
            input_schema: screenshot_schema(),
            timeout_ms: None,
            examples: Vec::new(),
        }
    }

    fn toolkit_with_descriptions(
        toolkit_description: String,
        tool_description: String,
    ) -> RegisterToolkitRequest {
        RegisterToolkitRequest {
            name: String::from("browser"),
            description: toolkit_description,
            tools: BTreeMap::from([(
                String::from("screenshot"),
                registered_tool(tool_description),
            )]),
        }
    }

    #[test]
    fn accepts_toolkit_and_tool_descriptions_at_length_limit() {
        let description = "a".repeat(MAX_TOOL_DESCRIPTION_LENGTH);
        let payload = toolkit_with_descriptions(description.clone(), description);

        validate_toolkit_registration(&payload).expect("description at limit should pass");
    }

    #[test]
    fn rejects_toolkit_description_longer_than_limit() {
        let payload = toolkit_with_descriptions(
            "a".repeat(MAX_TOOL_DESCRIPTION_LENGTH + 1),
            String::from("Take a screenshot"),
        );

        let error = validate_toolkit_registration(&payload).expect_err("long toolkit rejected");
        assert_eq!(
            error.to_string(),
            format!(
                "Toolkit \"browser\" description is {} characters, max is {}",
                MAX_TOOL_DESCRIPTION_LENGTH + 1,
                MAX_TOOL_DESCRIPTION_LENGTH
            )
        );
    }

    #[test]
    fn rejects_tool_description_longer_than_limit() {
        let payload = toolkit_with_descriptions(
            String::from("Browser automation"),
            "a".repeat(MAX_TOOL_DESCRIPTION_LENGTH + 1),
        );

        let error = validate_toolkit_registration(&payload).expect_err("long tool rejected");
        assert_eq!(
            error.to_string(),
            format!(
                "Tool \"browser/screenshot\" description is {} characters, max is {}",
                MAX_TOOL_DESCRIPTION_LENGTH + 1,
                MAX_TOOL_DESCRIPTION_LENGTH
            )
        );
    }

    #[test]
    fn tools_reject_duplicate_toolkit_registration() {
        let toolkits = BTreeMap::from([(
            String::from("browser"),
            toolkit_with_descriptions(
                String::from("Browser automation"),
                String::from("Take a screenshot"),
            ),
        )]);

        let error =
            ensure_toolkit_name_available(&toolkits, "browser").expect_err("duplicate rejected");
        assert_eq!(
            error,
            SidecarError::Conflict(String::from("toolkit already registered: browser"))
        );
    }

    #[test]
    fn tools_deny_tool_invocation_without_explicit_permission() {
        let permissions = PermissionsPolicy::deny_all();

        assert_eq!(
            tool_invocation_permission_mode(&permissions, "browser", "screenshot"),
            PermissionMode::Deny
        );
    }

    #[test]
    fn tools_allow_tool_invocation_with_matching_permission() {
        let permissions = PermissionsPolicy {
            fs: None,
            network: None,
            child_process: None,
            process: None,
            env: None,
            tool: Some(crate::protocol::PatternPermissionScope::Rules(
                crate::protocol::PatternPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![crate::protocol::PatternPermissionRule {
                        mode: PermissionMode::Allow,
                        operations: vec![String::from("invoke")],
                        patterns: vec![String::from("browser:screenshot")],
                    }],
                },
            )),
        };

        assert_eq!(
            tool_invocation_permission_mode(&permissions, "browser", "screenshot"),
            PermissionMode::Allow
        );
        assert_eq!(
            tool_invocation_permission_mode(&permissions, "browser", "click"),
            PermissionMode::Deny
        );
    }
}
