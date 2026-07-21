//! System identity and terminal ABI codecs.

use super::preview1::{
    call, commit, errno, json_reply, simple_call, value_u64, ERRNO_FAULT, ERRNO_INVAL, ERRNO_IO,
    ERRNO_NAMETOOLONG, SUCCESS,
};
use super::{i32_arg, set_i32_result};
use crate::abi::{AbiBinding, ImportId};
use crate::wasm::wasmtime::{memory, store::WasmtimeStoreState};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::HashMap;
use wasmtime::{Caller, Val};

pub async fn dispatch(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<bool> {
    use ImportId::*;
    let value = match abi.id {
        HostSystemGetIdentity => identity(caller, params).await,
        HostTtyRead => tty_read(caller, params).await,
        HostTtyIsatty => tty_isatty(caller, params).await,
        HostTtyGetSize => tty_size(caller, params).await,
        HostTtySetSize => tty_set_size(caller, params).await,
        HostTtyGetAttr => tty_get_attr(caller, params).await,
        HostTtySetAttr => tty_set_attr(caller, params).await,
        HostTtyGetPgrp => tty_scalar_output(caller, params, "__kernel_tcgetpgrp").await,
        HostTtyGetSid => tty_scalar_output(caller, params, "__kernel_tcgetsid").await,
        HostTtySetPgrp => {
            let (Ok(fd), Ok(pgid)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
                set_i32_result(results, ERRNO_INVAL)?;
                return Ok(true);
            };
            simple_call(caller, "__kernel_tcsetpgrp", vec![json!(fd), json!(pgid)]).await
        }
        HostTtySetRawMode => {
            let Ok(enabled) = i32_arg(params, 0) else {
                set_i32_result(results, ERRNO_INVAL)?;
                return Ok(true);
            };
            simple_call(caller, "__pty_set_raw_mode", vec![json!(enabled != 0)]).await
        }
        _ => return Ok(false),
    };
    set_i32_result(results, value)?;
    Ok(true)
}

async fn identity(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(field), Ok(output), Ok(capacity)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    let fields = [
        "hostname",
        "type",
        "release",
        "version",
        "machine",
        "domainName",
    ];
    let Some(field) = fields.get(field as usize) else {
        return ERRNO_INVAL;
    };
    if capacity == 0 {
        return ERRNO_NAMETOOLONG;
    }
    if memory::validate_range(caller, output, capacity as usize).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, "process.system_identity", vec![], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(value) = value.get(*field).and_then(Value::as_str) else {
                return ERRNO_IO;
            };
            if value.len().saturating_add(1) > capacity as usize {
                return ERRNO_NAMETOOLONG;
            }
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            commit(caller, output, &bytes)
        }
        Err(error) => errno(&error),
    }
}

async fn tty_read(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(output), Ok(capacity), Ok(timeout)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return 0;
    };
    if capacity == 0 || memory::validate_range(caller, output, capacity as usize).is_err() {
        return 0;
    }
    let timeout = if timeout == u32::MAX {
        Value::Null
    } else {
        json!(timeout)
    };
    match call(
        caller,
        "__kernel_stdin_read",
        vec![json!(capacity), timeout],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let bytes = match reply {
                crate::backend::HostCallReply::Raw(bytes) => bytes,
                crate::backend::HostCallReply::Json(value) => value
                    .get("dataBase64")
                    .and_then(Value::as_str)
                    .and_then(|encoded| {
                        base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .ok()
                    })
                    .unwrap_or_default(),
                crate::backend::HostCallReply::Empty => Vec::new(),
            };
            let length = bytes.len().min(capacity as usize);
            if memory::validate_range(caller, output, capacity as usize).is_err()
                || memory::write_bytes(caller, output, &bytes[..length]).is_err()
            {
                0
            } else {
                length as i32
            }
        }
        Err(_) => 0,
    }
}

async fn tty_isatty(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else { return 0 };
    match call(caller, "__kernel_isatty", vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => json_reply(reply)
            .ok()
            .and_then(|value| value.as_bool())
            .map(i32::from)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

async fn tty_size(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(columns), Ok(rows)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, columns, 2).is_err()
        || memory::validate_range(caller, rows, 2).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(caller, "__kernel_tty_size", vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(cols) = value
                .get("cols")
                .and_then(value_u64)
                .and_then(|value| u16::try_from(value).ok())
            else {
                return 25;
            };
            let Some(row_count) = value
                .get("rows")
                .and_then(value_u64)
                .and_then(|value| u16::try_from(value).ok())
            else {
                return 25;
            };
            if commit(caller, columns, &cols.to_le_bytes()) != SUCCESS {
                return ERRNO_FAULT;
            }
            commit(caller, rows, &row_count.to_le_bytes())
        }
        Err(error) if error.code == "EBADF" => 9,
        Err(error) if error.code == "ENOTTY" => 25,
        Err(_) => 5,
    }
}

async fn tty_set_size(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(columns), Ok(rows)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_INVAL;
    };
    if columns > u16::MAX.into() || rows > u16::MAX.into() {
        return ERRNO_INVAL;
    }
    simple_call(
        caller,
        "__kernel_tty_set_size",
        vec![json!(fd), json!(columns), json!(rows)],
    )
    .await
}

async fn tty_get_attr(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(flags_output), Ok(cc_output)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, flags_output, 4).is_err()
        || memory::validate_range(caller, cc_output, 7).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "__kernel_tcgetattr",
        vec![json!(fd)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(flags) = value
                .get("flags")
                .and_then(value_u64)
                .and_then(|value| u32::try_from(value).ok())
            else {
                return ERRNO_IO;
            };
            let Some(cc) = value
                .get("cc")
                .and_then(Value::as_array)
                .filter(|cc| cc.len() == 7)
            else {
                return ERRNO_IO;
            };
            let Some(cc) = cc
                .iter()
                .map(value_u64)
                .map(|value| value.and_then(|value| u8::try_from(value).ok()))
                .collect::<Option<Vec<_>>>()
            else {
                return ERRNO_IO;
            };
            if memory::validate_range(caller, flags_output, 4).is_err()
                || memory::validate_range(caller, cc_output, 7).is_err()
            {
                return ERRNO_FAULT;
            }
            if memory::write_u32(caller, flags_output, flags).is_err()
                || memory::write_bytes(caller, cc_output, &cc).is_err()
            {
                ERRNO_FAULT
            } else {
                SUCCESS
            }
        }
        Err(error) => errno(&error),
    }
}

async fn tty_set_attr(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(flags), Ok(cc_pointer)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    let Ok(cc) = memory::read_bytes(caller, cc_pointer, 7) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "__kernel_tcsetattr",
        vec![json!(fd), json!(flags), json!(cc)],
    )
    .await
}

async fn tty_scalar_output(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    method: &str,
) -> i32 {
    let (Ok(fd), Ok(output)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, method, vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(value) = value_u64(&value).and_then(|value| u32::try_from(value).ok()) else {
                return ERRNO_IO;
            };
            if memory::validate_range(caller, output, 4).is_err() {
                return ERRNO_FAULT;
            }
            memory::write_u32(caller, output, value).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}
