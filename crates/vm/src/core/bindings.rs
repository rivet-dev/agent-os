use agentos_sidecar_protocol::protocol::RegisterHostCallbacksRequest;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

pub const DEFAULT_BINDING_TIMEOUT_MS: u64 = 30_000;
pub const MAX_BINDING_TIMEOUT_MS: u64 = 300_000;
pub const MAX_REGISTERED_BINDING_COLLECTIONS: usize = 64;
pub const MAX_REGISTERED_BINDINGS_PER_VM: usize = 256;
pub const MAX_BINDINGS_PER_COLLECTION: usize = 64;
pub const MAX_BINDING_COLLECTION_NAME_LENGTH: usize = 64;
pub const MAX_BINDING_NAME_LENGTH: usize = 64;
pub const MAX_BINDING_DESCRIPTION_LENGTH: usize = 200;
pub const MAX_BINDING_SCHEMA_BYTES: usize = 16 * 1024;
pub const MAX_BINDING_SCHEMA_DEPTH: usize = 32;
pub const MAX_EXAMPLES_PER_BINDING: usize = 16;
pub const MAX_BINDING_EXAMPLE_INPUT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingRegistrationError {
    InvalidState(String),
    Conflict(String),
}

impl fmt::Display for BindingRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) | Self::Conflict(message) => f.write_str(message),
        }
    }
}

impl Error for BindingRegistrationError {}

pub fn validate_bindings_registration(
    payload: &RegisterHostCallbacksRequest,
) -> Result<(), BindingRegistrationError> {
    validate_collection_name(&payload.name)?;
    if payload.description.is_empty() {
        return Err(BindingRegistrationError::InvalidState(format!(
            "collection {} is missing a description",
            payload.name
        )));
    }
    validate_description_length(
        &format!("Binding collection \"{}\"", payload.name),
        &payload.description,
    )?;
    validate_command_aliases("command alias", &payload.command_aliases)?;
    validate_command_aliases("registry command alias", &payload.registry_command_aliases)?;
    for alias in &payload.command_aliases {
        if payload.registry_command_aliases.contains(alias) {
            return Err(BindingRegistrationError::InvalidState(format!(
                "host callback command alias must not also be a registry command alias: {alias}"
            )));
        }
    }
    if payload.callbacks.is_empty() {
        return Err(BindingRegistrationError::InvalidState(format!(
            "collection {} must define at least one binding",
            payload.name
        )));
    }
    if payload.callbacks.len() > MAX_BINDINGS_PER_COLLECTION {
        return Err(BindingRegistrationError::InvalidState(format!(
            "collection {} defines {} bindings, max is {MAX_BINDINGS_PER_COLLECTION}",
            payload.name,
            payload.callbacks.len()
        )));
    }
    for (binding_name, binding) in &payload.callbacks {
        validate_binding_name(binding_name)?;
        if binding.description.is_empty() {
            return Err(BindingRegistrationError::InvalidState(format!(
                "binding {} in collection {} is missing a description",
                binding_name, payload.name
            )));
        }
        validate_description_length(
            &format!("Binding \"{}/{}\"", payload.name, binding_name),
            &binding.description,
        )?;
        let binding_input_schema: Value =
            serde_json::from_str(&binding.input_schema).map_err(|error| {
                BindingRegistrationError::InvalidState(format!(
                    "Binding \"{}/{}\" input schema is invalid JSON: {error}",
                    payload.name, binding_name
                ))
            })?;
        validate_binding_schema_shape(
            &format!("Binding \"{}/{}\" input schema", payload.name, binding_name),
            &binding_input_schema,
        )?;
        if let Some(timeout_ms) = binding.timeout_ms {
            if timeout_ms > MAX_BINDING_TIMEOUT_MS {
                return Err(BindingRegistrationError::InvalidState(format!(
                    "Binding \"{}/{}\" timeout is {timeout_ms}ms, max is {MAX_BINDING_TIMEOUT_MS}ms",
                    payload.name, binding_name
                )));
            }
        }
        if binding.examples.len() > MAX_EXAMPLES_PER_BINDING {
            return Err(BindingRegistrationError::InvalidState(format!(
                "Binding \"{}/{}\" defines {} examples, max is {MAX_EXAMPLES_PER_BINDING}",
                payload.name,
                binding_name,
                binding.examples.len()
            )));
        }
        for (index, example) in binding.examples.iter().enumerate() {
            validate_description_length(
                &format!(
                    "Binding \"{}/{}\" example {index}",
                    payload.name, binding_name
                ),
                &example.description,
            )?;
            let example_input: Value = serde_json::from_str(&example.input).map_err(|error| {
                BindingRegistrationError::InvalidState(format!(
                    "Binding \"{}/{}\" example {index} input is invalid JSON: {error}",
                    payload.name, binding_name
                ))
            })?;
            validate_json_byte_length(
                &format!(
                    "Binding \"{}/{}\" example {index} input",
                    payload.name, binding_name
                ),
                &example_input,
                MAX_BINDING_EXAMPLE_INPUT_BYTES,
            )?;
        }
    }
    Ok(())
}

pub fn ensure_collection_name_available(
    bindings: &BTreeMap<String, RegisterHostCallbacksRequest>,
    collection_name: &str,
) -> Result<(), BindingRegistrationError> {
    if bindings.contains_key(collection_name) {
        return Err(BindingRegistrationError::Conflict(format!(
            "binding collection already registered: {collection_name}"
        )));
    }
    Ok(())
}

pub fn ensure_command_aliases_available(
    bindings: &BTreeMap<String, RegisterHostCallbacksRequest>,
    payload: &RegisterHostCallbacksRequest,
) -> Result<(), BindingRegistrationError> {
    let requested_command_aliases = payload.command_aliases.iter().collect::<BTreeSet<_>>();
    let requested_registry_aliases = payload
        .registry_command_aliases
        .iter()
        .collect::<BTreeSet<_>>();
    for collection in bindings.values() {
        for alias in &collection.command_aliases {
            if requested_command_aliases.contains(alias)
                || requested_registry_aliases.contains(alias)
            {
                return Err(BindingRegistrationError::Conflict(format!(
                    "host callback command alias already registered: {alias}"
                )));
            }
        }
        for alias in &collection.registry_command_aliases {
            if requested_command_aliases.contains(alias) {
                return Err(BindingRegistrationError::Conflict(format!(
                    "host callback command alias already registered: {alias}"
                )));
            }
        }
    }
    Ok(())
}

pub fn ensure_binding_registry_capacity(
    bindings: &BTreeMap<String, RegisterHostCallbacksRequest>,
    payload: &RegisterHostCallbacksRequest,
) -> Result<(), BindingRegistrationError> {
    if bindings.len() >= MAX_REGISTERED_BINDING_COLLECTIONS {
        return Err(BindingRegistrationError::InvalidState(format!(
            "VM already has {} registered binding collections, max is {MAX_REGISTERED_BINDING_COLLECTIONS}",
            bindings.len()
        )));
    }

    let registered_bindings = bindings
        .values()
        .map(|collection| collection.callbacks.len())
        .sum::<usize>();
    let total_bindings = registered_bindings
        .checked_add(payload.callbacks.len())
        .ok_or_else(|| {
            BindingRegistrationError::InvalidState(String::from(
                "registered host callback count overflow",
            ))
        })?;
    if total_bindings > MAX_REGISTERED_BINDINGS_PER_VM {
        return Err(BindingRegistrationError::InvalidState(format!(
            "VM would have {total_bindings} registered host callbacks, max is {MAX_REGISTERED_BINDINGS_PER_VM}"
        )));
    }

    Ok(())
}

pub fn registered_binding_command_names(
    bindings: &BTreeMap<String, RegisterHostCallbacksRequest>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut commands = Vec::new();
    for collection in bindings.values() {
        for alias in collection
            .registry_command_aliases
            .iter()
            .chain(collection.command_aliases.iter())
        {
            if seen.insert(alias.clone()) {
                commands.push(alias.clone());
            }
        }
    }
    commands
}

fn validate_collection_name(name: &str) -> Result<(), BindingRegistrationError> {
    if name.len() > MAX_BINDING_COLLECTION_NAME_LENGTH {
        return Err(BindingRegistrationError::InvalidState(format!(
            "invalid collection name {name}; max length is {MAX_BINDING_COLLECTION_NAME_LENGTH}"
        )));
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(BindingRegistrationError::InvalidState(format!(
            "invalid collection name {name}; expected lowercase alphanumeric characters plus hyphens"
        )));
    }
    Ok(())
}

fn validate_binding_name(name: &str) -> Result<(), BindingRegistrationError> {
    if name.len() > MAX_BINDING_NAME_LENGTH {
        return Err(BindingRegistrationError::InvalidState(format!(
            "invalid binding name {name}; max length is {MAX_BINDING_NAME_LENGTH}"
        )));
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(BindingRegistrationError::InvalidState(format!(
            "invalid binding name {name}; expected lowercase alphanumeric characters plus hyphens"
        )));
    }
    Ok(())
}

fn validate_command_aliases(
    label: &str,
    aliases: &[String],
) -> Result<(), BindingRegistrationError> {
    let mut seen = BTreeSet::new();
    for alias in aliases {
        validate_command_alias(label, alias)?;
        if !seen.insert(alias) {
            return Err(BindingRegistrationError::InvalidState(format!(
                "duplicate host callback {label}: {alias}"
            )));
        }
    }
    Ok(())
}

fn validate_command_alias(label: &str, alias: &str) -> Result<(), BindingRegistrationError> {
    if alias.is_empty()
        || alias == "."
        || alias == ".."
        || alias.contains('/')
        || alias.contains('\0')
    {
        return Err(BindingRegistrationError::InvalidState(format!(
            "invalid host callback {label}: {alias:?}"
        )));
    }
    Ok(())
}

fn validate_description_length(
    label: &str,
    description: &str,
) -> Result<(), BindingRegistrationError> {
    if description.len() > MAX_BINDING_DESCRIPTION_LENGTH {
        return Err(BindingRegistrationError::InvalidState(format!(
            "{label} description is {} characters, max is {MAX_BINDING_DESCRIPTION_LENGTH}",
            description.len()
        )));
    }
    Ok(())
}

fn validate_binding_schema_shape(
    label: &str,
    schema: &Value,
) -> Result<(), BindingRegistrationError> {
    validate_json_byte_length(label, schema, MAX_BINDING_SCHEMA_BYTES)?;
    validate_json_depth(label, schema, 0)
}

fn validate_json_byte_length(
    label: &str,
    value: &Value,
    limit: usize,
) -> Result<(), BindingRegistrationError> {
    let length = serde_json::to_vec(value)
        .map_err(|error| {
            BindingRegistrationError::InvalidState(format!("{label} is invalid JSON: {error}"))
        })?
        .len();
    if length > limit {
        return Err(BindingRegistrationError::InvalidState(format!(
            "{label} is {length} bytes, max is {limit}"
        )));
    }
    Ok(())
}

fn validate_json_depth(
    label: &str,
    value: &Value,
    depth: usize,
) -> Result<(), BindingRegistrationError> {
    if depth > MAX_BINDING_SCHEMA_DEPTH {
        return Err(BindingRegistrationError::InvalidState(format!(
            "{label} exceeds max JSON depth {MAX_BINDING_SCHEMA_DEPTH}"
        )));
    }

    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
        Value::Array(values) => {
            for value in values {
                validate_json_depth(label, value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(object) => {
            for value in object.values() {
                validate_json_depth(label, value, depth + 1)?;
            }
            Ok(())
        }
    }
}
