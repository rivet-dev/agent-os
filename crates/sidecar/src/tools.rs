use crate::protocol::{
    RegisterToolkitRequest, RegisteredToolDefinition, RequestFrame, ResponsePayload,
    ToolInvocationRequest, ToolkitRegisteredResponse,
};
use crate::service::{kernel_error, normalize_path, DispatchResult};
use crate::state::{BridgeError, VmState, TOOL_DRIVER_NAME, TOOL_MASTER_COMMAND};
use crate::{NativeSidecar, NativeSidecarBridge, SidecarError};
use agent_os_kernel::command_registry::CommandDriver;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const DEFAULT_TOOL_TIMEOUT_MS: u64 = 30_000;

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

    validate_toolkit_name(&payload.name)?;
    if payload.description.is_empty() {
        return Err(SidecarError::InvalidState(format!(
            "toolkit {} is missing a description",
            payload.name
        )));
    }
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
    }

    let registered_name = payload.name.clone();
    let (command_count, prompt_markdown) = {
        let vm = sidecar.vms.get_mut(&vm_id).expect("owned VM should exist");
        vm.toolkits.insert(registered_name.clone(), payload);
        refresh_tool_registry(vm)?;
        (
            tool_command_names(vm).len() as u32,
            generate_tool_reference(vm.toolkits.values()),
        )
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
    let guest_cwd = cwd.map(normalize_path).unwrap_or_else(|| vm.guest_cwd.clone());
    let resolution = match kind {
        ToolCommand::Master => resolve_master_command(vm, args, &guest_cwd)?,
        ToolCommand::Toolkit(toolkit_name) => {
            resolve_toolkit_command(vm, &toolkit_name, args, &guest_cwd)?
        }
    };
    Ok(Some(resolution))
}

fn identify_tool_command(vm: &VmState, command: &str) -> Option<ToolCommand> {
    if command == TOOL_MASTER_COMMAND {
        return Some(ToolCommand::Master);
    }
    command
        .strip_prefix(&format!("{TOOL_MASTER_COMMAND}-"))
        .filter(|toolkit_name| vm.toolkits.contains_key(*toolkit_name))
        .map(|toolkit_name| ToolCommand::Toolkit(toolkit_name.to_owned()))
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
    let input = match resolve_invocation_input(vm, &tool, cli_args, guest_cwd) {
        Ok(input) => input,
        Err(message) => {
            return ToolCommandResolution::Immediate {
                stdout: Vec::new(),
                stderr: format_tool_failure_output(&message),
                exit_code: 1,
            }
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
        return serde_json::from_str(value)
            .map_err(|error| format!("Invalid JSON for --json: {error}"));
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
        let text = String::from_utf8(bytes)
            .map_err(|error| format!("Invalid JSON file: {error}"))?;
        return serde_json::from_str(&text).map_err(|error| format!("Invalid JSON file: {error}"));
    }

    parse_argv(&tool.input_schema, cli_args)
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
        flag_to_field.insert(camel_to_kebab(field_name), (field_name.clone(), field_schema));
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
                let number = value.parse::<f64>().map_err(|_| {
                    format!("Flag --{raw_flag} expects a number, got \"{value}\"")
                })?;
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
                    let item_type = json_schema_type(field_schema.get("items").unwrap_or(&Value::Null))
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
    commands.extend(vm.toolkits.keys().map(|toolkit_name| toolkit_command_name(toolkit_name)));
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

enum ToolCommand {
    Master,
    Toolkit(String),
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
