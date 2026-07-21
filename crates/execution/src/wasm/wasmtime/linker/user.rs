//! Identity and account database ABI codecs.

use super::preview1::{
    call, commit, errno, json_reply, simple_call, value_u64, ERRNO_2BIG, ERRNO_FAULT, ERRNO_INVAL,
    ERRNO_IO, ERRNO_NAMETOOLONG, ERRNO_RANGE, SUCCESS,
};
use super::{i32_arg, set_i32_result};
use crate::abi::{AbiBinding, ImportId};
use crate::wasm::wasmtime::{memory, store::WasmtimeStoreState};
use serde_json::{json, Value};
use std::collections::HashMap;
use wasmtime::{Caller, Val};

const MAX_GROUPS: usize = 64;
const MAX_ACCOUNT_NAME_BYTES: usize = 4096;

pub async fn dispatch(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<bool> {
    use ImportId::*;
    let fixed_limit = match abi.id {
        HostUserGetgroups | HostUserSetgroups => i32_arg(params, 0).ok().map(|count| {
            (
                "wasm.abi.maxSupplementaryGroups",
                count as usize,
                MAX_GROUPS,
                ERRNO_INVAL,
            )
        }),
        HostUserGetpwnam | HostUserGetgrnam => i32_arg(params, 1).ok().map(|length| {
            (
                "wasm.abi.maxAccountNameBytes",
                length as usize,
                MAX_ACCOUNT_NAME_BYTES,
                ERRNO_NAMETOOLONG,
            )
        }),
        _ => None,
    };
    if let Some((name, observed, maximum, error)) = fixed_limit {
        let status = super::check_fixed_request_limit(caller, name, observed, maximum, error).await;
        if status != SUCCESS {
            set_i32_result(results, status)?;
            return Ok(true);
        }
    }
    let status = match abi.id {
        HostUserGetuid => scalar(caller, params, "process.getuid").await,
        HostUserGetgid => scalar(caller, params, "process.getgid").await,
        HostUserGeteuid => scalar(caller, params, "process.geteuid").await,
        HostUserGetegid => scalar(caller, params, "process.getegid").await,
        HostUserGetresuid => triple(caller, params, "process.getresuid").await,
        HostUserGetresgid => triple(caller, params, "process.getresgid").await,
        HostUserSetuid => set_one(caller, params, "process.setuid").await,
        HostUserSeteuid => set_one(caller, params, "process.seteuid").await,
        HostUserSetgid => set_one(caller, params, "process.setgid").await,
        HostUserSetegid => set_one(caller, params, "process.setegid").await,
        HostUserSetreuid => set_optional(caller, params, "process.setreuid", 2).await,
        HostUserSetregid => set_optional(caller, params, "process.setregid", 2).await,
        HostUserSetresuid => set_optional(caller, params, "process.setresuid", 3).await,
        HostUserSetresgid => set_optional(caller, params, "process.setresgid", 3).await,
        HostUserGetgroups => getgroups(caller, params).await,
        HostUserSetgroups => setgroups(caller, params).await,
        HostUserIsatty => isatty(caller, params).await,
        HostUserGetpwuid => account(caller, params, "process.getpwuid", AccountKey::Scalar).await,
        HostUserGetpwent => account(caller, params, "process.getpwent", AccountKey::Scalar).await,
        HostUserGetgrgid => account(caller, params, "process.getgrgid", AccountKey::Scalar).await,
        HostUserGetgrent => account(caller, params, "process.getgrent", AccountKey::Scalar).await,
        HostUserGetpwnam => account(caller, params, "process.getpwnam", AccountKey::Name).await,
        HostUserGetgrnam => account(caller, params, "process.getgrnam", AccountKey::Name).await,
        _ => return Ok(false),
    };
    set_i32_result(results, status)?;
    Ok(true)
}

async fn scalar(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], method: &str) -> i32 {
    let Ok(output) = i32_arg(params, 0) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, method, vec![], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(value) = value_u64(&value).and_then(|value| u32::try_from(value).ok()) else {
                return ERRNO_INVAL;
            };
            if memory::validate_range(caller, output, 4).is_err() {
                return ERRNO_FAULT;
            }
            memory::write_u32(caller, output, value).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

async fn triple(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], method: &str) -> i32 {
    let pointers = match (0..3)
        .map(|index| i32_arg(params, index))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(value) => value,
        Err(_) => return ERRNO_FAULT,
    };
    if pointers
        .iter()
        .any(|pointer| memory::validate_range(caller, *pointer, 4).is_err())
    {
        return ERRNO_FAULT;
    }
    match call(caller, method, vec![], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(values) = value.as_array().filter(|values| values.len() == 3) else {
                return ERRNO_IO;
            };
            let decoded = values
                .iter()
                .map(value_u64)
                .map(|value| value.and_then(|value| u32::try_from(value).ok()))
                .collect::<Option<Vec<_>>>();
            let Some(decoded) = decoded else {
                return ERRNO_IO;
            };
            if pointers
                .iter()
                .any(|pointer| memory::validate_range(caller, *pointer, 4).is_err())
            {
                return ERRNO_FAULT;
            }
            for (pointer, value) in pointers.into_iter().zip(decoded) {
                if memory::write_u32(caller, pointer, value).is_err() {
                    return ERRNO_FAULT;
                }
            }
            SUCCESS
        }
        Err(error) => errno(&error),
    }
}

async fn set_one(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], method: &str) -> i32 {
    let Ok(value) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    simple_call(caller, method, vec![json!(value)]).await
}

fn optional_id(value: u32) -> Value {
    if value == u32::MAX {
        Value::Null
    } else {
        json!(value)
    }
}

async fn set_optional(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    method: &str,
    count: usize,
) -> i32 {
    let mut args = Vec::with_capacity(count);
    for index in 0..count {
        let Ok(value) = i32_arg(params, index) else {
            return ERRNO_INVAL;
        };
        args.push(optional_id(value));
    }
    simple_call(caller, method, args).await
}

async fn getgroups(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(capacity), Ok(groups), Ok(count_output)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    let capacity = capacity as usize;
    if capacity > MAX_GROUPS {
        return ERRNO_2BIG;
    }
    if memory::validate_range(caller, count_output, 4).is_err()
        || (capacity != 0
            && memory::validate_range(caller, groups, capacity.saturating_mul(4)).is_err())
    {
        return ERRNO_FAULT;
    }
    match call(caller, "process.getgroups", vec![], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(values) = value.as_array().filter(|values| values.len() <= MAX_GROUPS) else {
                return ERRNO_INVAL;
            };
            if capacity != 0 && capacity < values.len() {
                return ERRNO_INVAL;
            }
            let decoded = values
                .iter()
                .map(value_u64)
                .map(|value| value.and_then(|value| u32::try_from(value).ok()))
                .collect::<Option<Vec<_>>>();
            let Some(decoded) = decoded else {
                return ERRNO_INVAL;
            };
            if memory::validate_range(caller, count_output, 4).is_err()
                || (capacity != 0
                    && memory::validate_range(caller, groups, decoded.len().saturating_mul(4))
                        .is_err())
            {
                return ERRNO_FAULT;
            }
            if capacity != 0 {
                for (index, value) in decoded.iter().enumerate() {
                    if memory::write_u32(caller, groups + (index * 4) as u32, *value).is_err() {
                        return ERRNO_FAULT;
                    }
                }
            }
            memory::write_u32(caller, count_output, decoded.len() as u32)
                .map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

async fn setgroups(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(count), Ok(pointer)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_FAULT;
    };
    let count = count as usize;
    if count > MAX_GROUPS {
        return ERRNO_2BIG;
    }
    if memory::validate_range(caller, pointer, count.saturating_mul(4)).is_err() {
        return ERRNO_FAULT;
    }
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let Ok(value) = memory::read_u32(caller, pointer + (index * 4) as u32) else {
            return ERRNO_FAULT;
        };
        values.push(json!(value));
    }
    simple_call(caller, "process.setgroups", vec![Value::Array(values)]).await
}

async fn isatty(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(output)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, "__kernel_isatty", vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let value = u32::from(value.as_bool().unwrap_or(false));
            memory::write_u32(caller, output, value).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

enum AccountKey {
    Scalar,
    Name,
}

async fn account(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    method: &str,
    key: AccountKey,
) -> i32 {
    let (args, buffer_index) = match key {
        AccountKey::Scalar => {
            let Ok(value) = i32_arg(params, 0) else {
                return ERRNO_INVAL;
            };
            (vec![json!(value)], 1)
        }
        AccountKey::Name => {
            let (Ok(pointer), Ok(length)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
                return ERRNO_FAULT;
            };
            if length as usize > MAX_ACCOUNT_NAME_BYTES {
                return ERRNO_NAMETOOLONG;
            }
            let Ok(name) = memory::read_string(caller, pointer, length as usize) else {
                return ERRNO_FAULT;
            };
            (vec![json!(name)], 2)
        }
    };
    let (Ok(buffer), Ok(capacity), Ok(required_output)) = (
        i32_arg(params, buffer_index),
        i32_arg(params, buffer_index + 1),
        i32_arg(params, buffer_index + 2),
    ) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, required_output, 4).is_err()
        || memory::validate_range(caller, buffer, capacity as usize).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(record) = value.as_str() else {
                return ERRNO_IO;
            };
            let bytes = record.as_bytes();
            if memory::validate_range(caller, required_output, 4).is_err()
                || (capacity as usize >= bytes.len()
                    && memory::validate_range(caller, buffer, bytes.len()).is_err())
            {
                return ERRNO_FAULT;
            }
            if memory::write_u32(caller, required_output, bytes.len() as u32).is_err() {
                return ERRNO_FAULT;
            }
            if (capacity as usize) < bytes.len() {
                return ERRNO_RANGE;
            }
            commit(caller, buffer, bytes)
        }
        Err(error) => errno(&error),
    }
}
