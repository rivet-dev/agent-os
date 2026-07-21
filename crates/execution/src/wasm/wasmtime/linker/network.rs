//! AgentOS host-network ABI codecs.
//!
//! The sidecar kernel owns socket descriptions and guest fd allocation. These
//! codecs translate only the owned libc wire format; they do not project or
//! synchronize a second executor-local socket table.

use super::preview1::{
    call, commit, errno, json_reply, reply_bytes, simple_call, value_u64, ERRNO_2BIG, ERRNO_FAULT,
    ERRNO_INVAL, ERRNO_IO, SUCCESS,
};
use super::{i32_arg, set_i32_result};
use crate::abi::{AbiBinding, ImportId};
use crate::backend::HostCallReply;
use crate::wasm::wasmtime::{memory, store::WasmtimeStoreState};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::HashMap;
use wasmtime::{Caller, Val};

const ERRNO_AGAIN: i32 = 6;
const ERRNO_BADF: i32 = 8;
const ERRNO_NOBUFS: i32 = 42;
const ERRNO_NOTSUP: i32 = 58;
const ERRNO_TIMEDOUT: i32 = 73;
const MAX_POLL_FDS: usize = 1024;
const MAX_DNS_RECORDS: usize = 4096;
const MAX_DNS_PAYLOAD: usize = 64 * 1024;
const SOCK_TYPE_MASK: u32 = 0xf;
const SOCK_DGRAM: u32 = 5;
const SOCK_STREAM: u32 = 6;
const SOCK_CLOEXEC: u32 = 0x2000;
const SOCK_NONBLOCK: u32 = 0x4000;
const KERNEL_O_NONBLOCK: u32 = 0x800;
const MSG_TRUNC: u32 = 0x20;

pub async fn dispatch(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<bool> {
    use ImportId::*;
    let status = match abi.id {
        HostNetNetSocket => socket(caller, params).await,
        HostNetNetSetNonblock => set_nonblock(caller, params).await,
        HostNetNetConnect => address_call(caller, params, "process.hostnet_connect").await,
        HostNetNetBind => address_call(caller, params, "process.hostnet_bind").await,
        HostNetNetGetaddrinfo => getaddrinfo(caller, params).await,
        HostNetNetDnsQueryRrV1 => dns_query(caller, params).await,
        HostNetNetListen => listen(caller, params).await,
        HostNetNetAccept => accept(caller, params).await,
        HostNetNetValidateSocket => validate(caller, params, false).await,
        HostNetNetValidateAccept => validate(caller, params, true).await,
        HostNetNetGetsockname => address_output(caller, params, false).await,
        HostNetNetGetpeername => address_output(caller, params, true).await,
        HostNetNetSend => send(caller, params, false).await,
        HostNetNetSendto => send(caller, params, true).await,
        HostNetNetRecv => receive(caller, params, false).await,
        HostNetNetRecvfrom => receive(caller, params, true).await,
        HostNetNetSetsockopt => set_option(caller, params).await,
        HostNetNetGetsockopt => get_option(caller, params).await,
        HostNetNetClose => close(caller, params).await,
        HostNetNetTlsConnect => tls_connect(caller, params).await,
        HostNetNetPoll => poll(caller, params).await,
        _ => return Ok(false),
    };
    set_i32_result(results, status)?;
    Ok(true)
}

async fn socket(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(domain), Ok(socket_type), Ok(_protocol), Ok(output)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
    ) else {
        return ERRNO_INVAL;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    let kind = socket_type & SOCK_TYPE_MASK;
    if !matches!(kind, SOCK_DGRAM | SOCK_STREAM) {
        return ERRNO_NOTSUP;
    }
    if domain == 3 && kind != SOCK_STREAM {
        return ERRNO_NOTSUP;
    }
    match call(
        caller,
        "process.hostnet_fd_open",
        vec![
            json!(domain),
            json!(kind),
            json!(socket_type & SOCK_NONBLOCK != 0),
            json!(socket_type & SOCK_CLOEXEC != 0),
        ],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(fd) = value
                .get("fd")
                .and_then(value_u64)
                .and_then(|value| u32::try_from(value).ok())
            else {
                return ERRNO_IO;
            };
            if commit(caller, output, &fd.to_le_bytes()) == SUCCESS {
                SUCCESS
            } else {
                log_fd_rollback(caller, fd, "socket output commit").await;
                ERRNO_FAULT
            }
        }
        Err(error) => errno(&error),
    }
}

async fn set_nonblock(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(enable)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    let flags = match call(caller, "process.fd_stat", vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => match json_reply(reply)
            .ok()
            .and_then(|value| value.get("flags").and_then(value_u64))
            .and_then(|value| u32::try_from(value).ok())
        {
            Some(value) => value,
            None => return ERRNO_IO,
        },
        Err(error) => return errno(&error),
    };
    let flags = if enable != 0 {
        flags | KERNEL_O_NONBLOCK
    } else {
        flags & !KERNEL_O_NONBLOCK
    };
    simple_call(
        caller,
        "process.fd_set_flags",
        vec![json!(fd), json!(flags)],
    )
    .await
}

async fn address_call(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    method: &str,
) -> i32 {
    let (Ok(fd), Ok(pointer), Ok(length)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_INVAL;
    };
    let Ok(mut text) = memory::read_string(caller, pointer, length as usize) else {
        return ERRNO_FAULT;
    };
    if let Some(index) = text.find('\0') {
        text.truncate(index);
    }
    let Ok(address) = decode_address(&text) else {
        return ERRNO_INVAL;
    };
    let args = if method.ends_with("connect") {
        vec![json!(fd), address, Value::Null]
    } else {
        vec![json!(fd), address]
    };
    simple_call(caller, method, args).await
}

fn decode_address(text: &str) -> Result<Value, ()> {
    if text == "unix-autobind" {
        return Ok(json!({"type": "unix-autobind"}));
    }
    if let Some(hex) = text.strip_prefix("unix-abstract:") {
        if hex.len().is_multiple_of(2) && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(json!({"type": "unix-abstract", "hex": hex.to_ascii_lowercase()}));
        }
        return Err(());
    }
    if let Some(hex) = text.strip_prefix("unix-path-hex:") {
        let bytes = decode_hex(hex)?;
        let path = String::from_utf8(bytes).map_err(|_| ())?;
        return Ok(json!({"type": "unix-path", "path": path}));
    }
    if let Some(path) = text.strip_prefix("unix:") {
        return Ok(json!({"type": "unix-path", "path": path}));
    }
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let (host, port) = rest.split_once("]:").ok_or(())?;
        (host, port)
    } else {
        text.rsplit_once(':').ok_or(())?
    };
    if host.is_empty() {
        return Err(());
    }
    let port = port.parse::<u16>().map_err(|_| ())?;
    Ok(json!({"type": "inet", "host": host, "port": port}))
}

fn decode_hex(text: &str) -> Result<Vec<u8>, ()> {
    if !text.len().is_multiple_of(2) {
        return Err(());
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|_| ())?;
            u8::from_str_radix(pair, 16).map_err(|_| ())
        })
        .collect()
}

async fn getaddrinfo(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(host_pointer), Ok(host_length), Ok(family), Ok(output), Ok(length_output)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 4),
        i32_arg(params, 5),
        i32_arg(params, 6),
    ) else {
        return ERRNO_FAULT;
    };
    let Ok(hostname) = memory::read_string(caller, host_pointer, host_length as usize) else {
        return ERRNO_FAULT;
    };
    let Ok(capacity) = memory::read_u32(caller, length_output) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, capacity as usize).is_err()
        || memory::validate_range(caller, length_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    if !matches!(family, 0 | 4 | 6) {
        return ERRNO_INVAL;
    }
    let mut options = serde_json::Map::from_iter([
        ("hostname".to_owned(), json!(hostname)),
        ("all".to_owned(), json!(true)),
    ]);
    if family != 0 {
        options.insert("family".to_owned(), json!(family));
    }
    match call(
        caller,
        "dns.lookup",
        vec![Value::Object(options)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(records) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(records) = records.as_array() else {
                return ERRNO_IO;
            };
            let mut normalized = Vec::with_capacity(records.len());
            for record in records {
                let Some(family) = record.get("family").and_then(value_u64) else {
                    return ERRNO_IO;
                };
                let Some(address) = record.get("address").and_then(Value::as_str) else {
                    return ERRNO_IO;
                };
                if !matches!(family, 4 | 6) {
                    return ERRNO_IO;
                }
                normalized.push(json!({"addr": address, "family": family}));
            }
            let Ok(bytes) = serde_json::to_vec(&normalized) else {
                return ERRNO_IO;
            };
            publish_bytes(caller, output, capacity, length_output, &bytes, false)
        }
        Err(error) => errno(&error),
    }
}

async fn dns_query(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (
        Ok(name_pointer),
        Ok(name_length),
        Ok(record_type),
        Ok(output),
        Ok(capacity),
        Ok(length_output),
        Ok(ttl_output),
        Ok(flags_output),
    ) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
        i32_arg(params, 5),
        i32_arg(params, 6),
        i32_arg(params, 7),
    )
    else {
        return ERRNO_FAULT;
    };
    let Ok(name) = memory::read_string(caller, name_pointer, name_length as usize) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, capacity as usize).is_err()
        || [length_output, ttl_output, flags_output]
            .into_iter()
            .any(|pointer| memory::validate_range(caller, pointer, 4).is_err())
    {
        return ERRNO_FAULT;
    }
    let requested = match record_type {
        12 => "PTR",
        44 => "SSHFP",
        _ => return ERRNO_NOTSUP,
    };
    match call(
        caller,
        "dns.resolveRawRr",
        vec![json!({"hostname": name, "rrtype": requested})],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(status) = value.get("status").and_then(Value::as_str) else {
                return ERRNO_IO;
            };
            if !matches!(status, "ok" | "nxdomain" | "nodata") {
                return ERRNO_IO;
            }
            let Some(records) = value.get("records").and_then(Value::as_array) else {
                return ERRNO_IO;
            };
            if records.len() > MAX_DNS_RECORDS {
                return ERRNO_NOBUFS;
            }
            let mut payload = Vec::from((records.len() as u32).to_le_bytes());
            let mut ttl: Option<u32> = None;
            for record in records {
                let Some(encoded) = record.get("data").and_then(Value::as_str) else {
                    return ERRNO_IO;
                };
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                    return ERRNO_IO;
                };
                if (requested == "PTR" && bytes.is_empty())
                    || (requested == "SSHFP" && bytes.len() < 2)
                {
                    return ERRNO_IO;
                }
                let Some(record_ttl) = record
                    .get("ttl")
                    .and_then(value_u64)
                    .and_then(|value| u32::try_from(value).ok())
                else {
                    return ERRNO_IO;
                };
                ttl = Some(ttl.map_or(record_ttl, |prior| prior.min(record_ttl)));
                let Ok(length) = u32::try_from(bytes.len()) else {
                    return ERRNO_NOBUFS;
                };
                payload.extend_from_slice(&length.to_le_bytes());
                payload.extend_from_slice(&bytes);
                if payload.len() > MAX_DNS_PAYLOAD {
                    return ERRNO_NOBUFS;
                }
            }
            if commit(caller, length_output, &(payload.len() as u32).to_le_bytes()) != SUCCESS {
                return ERRNO_FAULT;
            }
            if payload.len() > capacity as usize {
                return ERRNO_NOBUFS;
            }
            if commit(caller, output, &payload) != SUCCESS
                || commit(caller, ttl_output, &ttl.unwrap_or(0).to_le_bytes()) != SUCCESS
                || commit(
                    caller,
                    flags_output,
                    &(if status == "nxdomain" {
                        2u32
                    } else if status == "nodata" {
                        4
                    } else {
                        0
                    })
                    .to_le_bytes(),
                ) != SUCCESS
            {
                ERRNO_FAULT
            } else {
                SUCCESS
            }
        }
        Err(error) => errno(&error),
    }
}

async fn listen(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(backlog)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    simple_call(
        caller,
        "process.hostnet_listen",
        vec![json!(fd), json!(backlog)],
    )
    .await
}

async fn validate(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    listening: bool,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_BADF;
    };
    simple_call(
        caller,
        "process.hostnet_validate",
        vec![json!(fd), json!(listening)],
    )
    .await
}

async fn accept(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(fd_output), Ok(address_output), Ok(length_output)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
    ) else {
        return ERRNO_FAULT;
    };
    let Ok(capacity) = memory::read_u32(caller, length_output) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, fd_output, 4).is_err()
        || memory::validate_range(caller, address_output, capacity as usize).is_err()
        || memory::validate_range(caller, length_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.hostnet_accept",
        vec![json!(fd), json!(false), json!(false), Value::Null],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            if value.is_null() || value.get("kind").and_then(Value::as_str) == Some("wouldBlock") {
                return ERRNO_AGAIN;
            }
            let Some(accepted_fd) = value
                .get("fd")
                .and_then(value_u64)
                .and_then(|value| u32::try_from(value).ok())
            else {
                return ERRNO_IO;
            };
            let address = encode_address(value.get("info").unwrap_or(&value), true);
            let copied = if commit(caller, fd_output, &accepted_fd.to_le_bytes()) == SUCCESS {
                publish_bytes(
                    caller,
                    address_output,
                    capacity,
                    length_output,
                    address.as_bytes(),
                    false,
                )
            } else {
                ERRNO_FAULT
            };
            if copied == SUCCESS {
                SUCCESS
            } else {
                log_fd_rollback(caller, accepted_fd, "accept output commit").await;
                copied
            }
        }
        Err(error) => errno(&error),
    }
}

async fn log_fd_rollback(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    fd: u32,
    context: &'static str,
) {
    let error = simple_call(caller, "process.fd_close", vec![json!(fd)]).await;
    if error != SUCCESS {
        eprintln!("ERR_AGENTOS_WASMTIME_FD_ROLLBACK: context={context} fd={fd} errno={error}");
    }
}

async fn address_output(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    peer: bool,
) -> i32 {
    let (Ok(fd), Ok(output), Ok(length_output)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    let Ok(capacity) = memory::read_u32(caller, length_output) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, capacity as usize).is_err()
        || memory::validate_range(caller, length_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    let method = if peer {
        "process.hostnet_peer_address"
    } else {
        "process.hostnet_local_address"
    };
    match call(caller, method, vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let text = encode_address(&value, false);
            publish_bytes(
                caller,
                output,
                capacity,
                length_output,
                text.as_bytes(),
                false,
            )
        }
        Err(error) => errno(&error),
    }
}

fn encode_address(value: &Value, peer_fields: bool) -> String {
    let prefix = if peer_fields { "remote" } else { "" };
    let field = |plain: &str, prefixed: &str| {
        value
            .get(if peer_fields { prefixed } else { plain })
            .or_else(|| value.get(plain))
    };
    if let (Some(address), Some(port)) = (
        field("address", "remoteAddress").and_then(Value::as_str),
        field("port", "remotePort").and_then(value_u64),
    ) {
        let _ = prefix;
        return if address.contains(':') {
            format!("[{address}]:{port}")
        } else {
            format!("{address}:{port}")
        };
    }
    if let Some(hex) = field("abstractPathHex", "remoteAbstractPathHex").and_then(Value::as_str) {
        return format!("unix-abstract:{hex}");
    }
    if let Some(path) = field("path", "remotePath").and_then(Value::as_str) {
        return format!("unix:{path}");
    }
    "unix-unnamed".to_owned()
}

async fn send(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], to: bool) -> i32 {
    let (Ok(fd), Ok(pointer), Ok(length), Ok(flags)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
    ) else {
        return ERRNO_FAULT;
    };
    let output_index = if to { 6 } else { 4 };
    let Ok(output) = i32_arg(params, output_index) else {
        return ERRNO_FAULT;
    };
    let Ok(bytes) = memory::read_bytes(caller, pointer, length as usize) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    let address = if to {
        let (Ok(pointer), Ok(length)) = (i32_arg(params, 4), i32_arg(params, 5)) else {
            return ERRNO_FAULT;
        };
        let Ok(text) = memory::read_string(caller, pointer, length as usize) else {
            return ERRNO_FAULT;
        };
        match decode_address(&text) {
            Ok(value) => value,
            Err(_) => return ERRNO_INVAL,
        }
    } else {
        Value::Null
    };
    let mut raw = HashMap::new();
    raw.insert(1, bytes);
    match call(
        caller,
        "process.hostnet_send",
        vec![json!(fd), Value::Null, json!(flags), address, Value::Null],
        raw,
    )
    .await
    {
        Ok(reply) => {
            let written = match reply {
                HostCallReply::Json(value) => {
                    value_u64(&value).or_else(|| value.get("bytes").and_then(value_u64))
                }
                _ => None,
            };
            let Some(written) = written.and_then(|value| u32::try_from(value).ok()) else {
                return ERRNO_IO;
            };
            commit(caller, output, &written.to_le_bytes())
        }
        Err(error) => errno(&error),
    }
}

async fn receive(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], from: bool) -> i32 {
    let (Ok(fd), Ok(output), Ok(capacity), Ok(flags)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
    ) else {
        return ERRNO_FAULT;
    };
    let Ok(length_output) = i32_arg(params, 4) else {
        return ERRNO_FAULT;
    };
    let address_outputs = if from {
        let (Ok(address), Ok(length)) = (i32_arg(params, 5), i32_arg(params, 6)) else {
            return ERRNO_FAULT;
        };
        let Ok(address_capacity) = memory::read_u32(caller, length) else {
            return ERRNO_FAULT;
        };
        Some((address, address_capacity, length))
    } else {
        None
    };
    if memory::validate_range(caller, output, capacity as usize).is_err()
        || memory::validate_range(caller, length_output, 4).is_err()
        || address_outputs.is_some_and(|(pointer, capacity, length)| {
            memory::validate_range(caller, pointer, capacity as usize).is_err()
                || memory::validate_range(caller, length, 4).is_err()
        })
    {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.hostnet_recv",
        vec![json!(fd), json!(capacity), json!(flags), Value::Null],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            if matches!(&reply, HostCallReply::Json(Value::Null)) {
                return commit(caller, length_output, &0u32.to_le_bytes());
            }
            let (bytes, address) = match reply {
                HostCallReply::Json(value)
                    if value.get("kind").and_then(Value::as_str) == Some("wouldBlock") =>
                {
                    return ERRNO_AGAIN;
                }
                HostCallReply::Json(value)
                    if value.get("type").and_then(Value::as_str) == Some("message") =>
                {
                    let bytes = value
                        .get("data")
                        .cloned()
                        .map(HostCallReply::Json)
                        .and_then(|value| reply_bytes(value).ok())
                        .ok_or(ERRNO_IO);
                    let address = encode_address(&value, true);
                    match bytes {
                        Ok(bytes) => (bytes, Some(address)),
                        Err(error) => return error,
                    }
                }
                reply => match reply_bytes(reply) {
                    Ok(bytes) => (bytes, None),
                    Err(error) => return error,
                },
            };
            let written = bytes.len().min(capacity as usize);
            if commit(caller, output, &bytes[..written]) != SUCCESS {
                return ERRNO_FAULT;
            }
            let reported = if flags & MSG_TRUNC != 0 {
                bytes.len()
            } else {
                written
            };
            let Ok(reported) = u32::try_from(reported) else {
                return ERRNO_2BIG;
            };
            if commit(caller, length_output, &reported.to_le_bytes()) != SUCCESS {
                return ERRNO_FAULT;
            }
            if let Some((address_output, address_capacity, address_length_output)) = address_outputs
            {
                let Some(address) = address else {
                    return ERRNO_IO;
                };
                return publish_bytes(
                    caller,
                    address_output,
                    address_capacity,
                    address_length_output,
                    address.as_bytes(),
                    false,
                );
            }
            SUCCESS
        }
        Err(error) => errno(&error),
    }
}

fn timeval_ms(bytes: &[u8]) -> Result<Option<u64>, ()> {
    if bytes.len() != 16 {
        return Err(());
    }
    let seconds = i64::from_le_bytes(bytes[0..8].try_into().map_err(|_| ())?);
    let micros = i64::from_le_bytes(bytes[8..16].try_into().map_err(|_| ())?);
    if seconds < 0 || !(0..1_000_000).contains(&micros) {
        return Err(());
    }
    if seconds == 0 && micros == 0 {
        return Ok(None);
    }
    Ok(Some(
        (seconds as u64)
            .saturating_mul(1000)
            .saturating_add((micros as u64).div_ceil(1000)),
    ))
}

fn option_kind(level: u32, name: u32, length: u32) -> Option<&'static str> {
    let socket_level = matches!(level, 1 | 0x7fff_ffff);
    if socket_level && matches!(name, 20 | 66) && length == 16 {
        Some("receive-timeout")
    } else if (socket_level && name == 9)
        || (level == 6 && name == 1)
        || (level == 0 && name == 1)
        || (level == 41 && name == 67)
    {
        Some("ignore")
    } else {
        None
    }
}

async fn set_option(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(level), Ok(name), Ok(pointer), Ok(length)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
    ) else {
        return ERRNO_INVAL;
    };
    let Some(kind) = option_kind(level, name, length) else {
        return ERRNO_INVAL;
    };
    if kind == "ignore" {
        return SUCCESS;
    }
    let Ok(bytes) = memory::read_bytes(caller, pointer, length as usize) else {
        return ERRNO_FAULT;
    };
    let Ok(duration) = timeval_ms(&bytes) else {
        return ERRNO_INVAL;
    };
    simple_call(
        caller,
        "process.hostnet_set_option",
        vec![json!(fd), json!(kind), json!({"durationMs": duration})],
    )
    .await
}

async fn get_option(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(level), Ok(name), Ok(output), Ok(length_output)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
    ) else {
        return ERRNO_FAULT;
    };
    let Ok(capacity) = memory::read_u32(caller, length_output) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, capacity as usize).is_err()
        || memory::validate_range(caller, length_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    if !matches!(level, 1 | 0x7fff_ffff) || name != 4 || capacity < 4 {
        return ERRNO_INVAL;
    }
    match call(
        caller,
        "process.hostnet_get_option",
        vec![json!(fd), json!("error")],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(value) = value.as_i64().and_then(|value| i32::try_from(value).ok()) else {
                return ERRNO_IO;
            };
            if commit(caller, output, &value.to_le_bytes()) != SUCCESS {
                ERRNO_FAULT
            } else {
                commit(caller, length_output, &4u32.to_le_bytes())
            }
        }
        Err(error) => errno(&error),
    }
}

async fn close(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_BADF;
    };
    simple_call(caller, "process.fd_close", vec![json!(fd)]).await
}

async fn tls_connect(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(pointer), Ok(length)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    let Ok(hostname) = memory::read_string(caller, pointer, length as usize) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.hostnet_tls_connect",
        vec![json!(fd), json!(hostname), json!([]), Value::Null],
    )
    .await
}

async fn poll(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(pointer), Ok(count), Ok(timeout), Ok(ready_output)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
    ) else {
        return ERRNO_FAULT;
    };
    let count = count as usize;
    let limit = super::check_fixed_request_limit(
        caller,
        "wasm.abi.maxPollFds",
        count,
        MAX_POLL_FDS,
        ERRNO_INVAL,
    )
    .await;
    if limit != SUCCESS {
        return limit;
    }
    if memory::validate_range(caller, pointer, count.saturating_mul(8)).is_err()
        || memory::validate_range(caller, ready_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let base = pointer + (index * 8) as u32;
        let Ok(fd) = memory::read_u32(caller, base) else {
            return ERRNO_FAULT;
        };
        let Ok(events) = memory::read_bytes(caller, base + 4, 2) else {
            return ERRNO_FAULT;
        };
        entries.push(json!({"fd": fd, "events": u16::from_le_bytes([events[0], events[1]])}));
    }
    let requested_timeout = timeout as i32;
    let blocking_limit_ms = caller.data().max_blocking_read_ms;
    let safeguard_applies = requested_timeout < 0
        || u64::try_from(requested_timeout).is_ok_and(|timeout| timeout > blocking_limit_ms);
    let first_wait_ms = if safeguard_applies {
        blocking_limit_ms.saturating_mul(4) / 5
    } else {
        u64::try_from(requested_timeout).unwrap_or_default()
    };
    let mut reply = call(
        caller,
        "process.posix_poll",
        vec![
            Value::Array(entries.clone()),
            json!(first_wait_ms),
            Value::Null,
        ],
        HashMap::new(),
    )
    .await;
    if safeguard_applies
        && reply
            .as_ref()
            .ok()
            .and_then(|reply| json_reply(reply.clone()).ok())
            .and_then(|value| value.get("readyCount").and_then(value_u64))
            .unwrap_or_default()
            == 0
    {
        let warning = format!(
            "[agentos] blocking poll is nearing limits.resources.maxBlockingReadMs ({blocking_limit_ms} ms)\n"
        );
        if caller
            .data()
            .host
            .publish_stderr(warning.into_bytes())
            .await
            .is_err()
        {
            return ERRNO_IO;
        }
        reply = call(
            caller,
            "process.posix_poll",
            vec![
                Value::Array(entries),
                json!(blocking_limit_ms.saturating_sub(first_wait_ms)),
                Value::Null,
            ],
            HashMap::new(),
        )
        .await;
        if reply
            .as_ref()
            .ok()
            .and_then(|reply| json_reply(reply.clone()).ok())
            .and_then(|value| value.get("readyCount").and_then(value_u64))
            .unwrap_or_default()
            == 0
        {
            let warning = format!(
                "[agentos] blocking poll exceeded limits.resources.maxBlockingReadMs ({blocking_limit_ms} ms); raise limits.resources.maxBlockingReadMs if needed\n"
            );
            if caller
                .data()
                .host
                .publish_stderr(warning.into_bytes())
                .await
                .is_err()
            {
                return ERRNO_IO;
            }
            return if commit(caller, ready_output, &0u32.to_le_bytes()) == SUCCESS {
                ERRNO_TIMEDOUT
            } else {
                ERRNO_FAULT
            };
        }
    }
    match reply {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(fds) = value.get("fds").and_then(Value::as_array) else {
                return ERRNO_IO;
            };
            for entry in fds {
                let (Some(fd), Some(revents)) = (
                    entry.get("fd").and_then(value_u64),
                    entry.get("revents").and_then(value_u64),
                ) else {
                    return ERRNO_IO;
                };
                if let Some(index) = (0..count).find(|index| {
                    memory::read_u32(caller, pointer + (*index * 8) as u32).ok() == Some(fd as u32)
                }) {
                    if commit(
                        caller,
                        pointer + (index * 8 + 6) as u32,
                        &(revents as u16).to_le_bytes(),
                    ) != SUCCESS
                    {
                        return ERRNO_FAULT;
                    }
                }
            }
            let ready = value
                .get("readyCount")
                .and_then(value_u64)
                .unwrap_or_else(|| {
                    fds.iter()
                        .filter(|entry| entry.get("revents").and_then(value_u64).unwrap_or(0) != 0)
                        .count() as u64
                });
            let Ok(ready) = u32::try_from(ready) else {
                return ERRNO_2BIG;
            };
            commit(caller, ready_output, &ready.to_le_bytes())
        }
        Err(error) => errno(&error),
    }
}

fn publish_bytes(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    output: u32,
    capacity: u32,
    length_output: u32,
    bytes: &[u8],
    require_capacity: bool,
) -> i32 {
    let written = bytes.len().min(capacity as usize);
    if require_capacity && written != bytes.len() {
        return ERRNO_NOBUFS;
    }
    if memory::validate_range(caller, output, written).is_err()
        || memory::validate_range(caller, length_output, 4).is_err()
        || memory::write_bytes(caller, output, &bytes[..written]).is_err()
        || memory::write_u32(caller, length_output, written as u32).is_err()
    {
        ERRNO_FAULT
    } else {
        SUCCESS
    }
}
