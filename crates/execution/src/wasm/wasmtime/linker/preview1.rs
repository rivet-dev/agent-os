//! AgentOS-owned Preview1 codecs.
//!
//! These codecs deliberately use kernel descriptor numbers directly. Unlike
//! the V8 compatibility runner there is no ambient Node-WASI descriptor table
//! to project or synchronize: the sidecar kernel remains the sole source of
//! truth for fd identity, offsets, flags, rights, and lifecycle.

use super::{i32_arg, set_i32_result};
use crate::abi::{AbiBinding, ImportId};
use crate::backend::{HostCallReply, HostServiceError};
use crate::wasm::wasmtime::{memory, store::WasmtimeStoreState};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::HashMap;
use wasmtime::{Caller, Val};

pub(super) const SUCCESS: i32 = 0;
pub(super) const ERRNO_2BIG: i32 = 1;
const ERRNO_ACCES: i32 = 2;
const ERRNO_ADDRINUSE: i32 = 3;
const ERRNO_ADDRNOTAVAIL: i32 = 4;
const ERRNO_AFNOSUPPORT: i32 = 5;
const ERRNO_AGAIN: i32 = 6;
const ERRNO_ALREADY: i32 = 7;
const ERRNO_BADF: i32 = 8;
const ERRNO_BUSY: i32 = 10;
const ERRNO_CHILD: i32 = 12;
const ERRNO_CONNREFUSED: i32 = 14;
const ERRNO_CONNRESET: i32 = 15;
const ERRNO_DEADLK: i32 = 16;
const ERRNO_DESTADDRREQ: i32 = 17;
const ERRNO_EXIST: i32 = 20;
pub(super) const ERRNO_FAULT: i32 = 21;
const ERRNO_FBIG: i32 = 22;
const ERRNO_HOSTUNREACH: i32 = 23;
const ERRNO_ILSEQ: i32 = 25;
const ERRNO_INPROGRESS: i32 = 26;
const ERRNO_INTR: i32 = 27;
pub(super) const ERRNO_INVAL: i32 = 28;
pub(super) const ERRNO_IO: i32 = 29;
const ERRNO_ISCONN: i32 = 30;
const ERRNO_ISDIR: i32 = 31;
const ERRNO_LOOP: i32 = 32;
const ERRNO_MFILE: i32 = 33;
const ERRNO_MSGSIZE: i32 = 35;
pub(super) const ERRNO_NAMETOOLONG: i32 = 37;
const ERRNO_NETUNREACH: i32 = 40;
const ERRNO_NFILE: i32 = 41;
const ERRNO_NOBUFS: i32 = 42;
const ERRNO_NOENT: i32 = 44;
const ERRNO_NOEXEC: i32 = 45;
const ERRNO_NOSPC: i32 = 51;
const ERRNO_NOSYS: i32 = 52;
const ERRNO_NOTCONN: i32 = 53;
const ERRNO_NOTDIR: i32 = 54;
const ERRNO_NOTEMPTY: i32 = 55;
const ERRNO_NOTSOCK: i32 = 57;
const ERRNO_NOTSUP: i32 = 58;
const ERRNO_NXIO: i32 = 60;
const ERRNO_OVERFLOW: i32 = 61;
const ERRNO_PERM: i32 = 63;
const ERRNO_PIPE: i32 = 64;
const ERRNO_PROTONOSUPPORT: i32 = 66;
pub(super) const ERRNO_RANGE: i32 = 68;
const ERRNO_ROFS: i32 = 69;
const ERRNO_SPIPE: i32 = 70;
const ERRNO_SRCH: i32 = 71;
const ERRNO_TIMEDOUT: i32 = 73;
const ERRNO_XDEV: i32 = 75;
pub(super) const ERRNO_NODATA: i32 = 78;

const MAX_IOVECS: usize = 1024;
const MAX_POLL_SUBSCRIPTIONS: usize = 1024;

pub async fn dispatch(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<bool> {
    use ImportId::*;
    let status = match abi.id {
        WasiSnapshotPreview1ArgsGet
        | WasiSnapshotPreview1ArgsSizesGet
        | WasiSnapshotPreview1EnvironGet
        | WasiSnapshotPreview1EnvironSizesGet
        | WasiSnapshotPreview1ProcExit
        | WasiSnapshotPreview1SchedYield => return Ok(false),
        WasiSnapshotPreview1ClockTimeGet => clock_value(caller, params, false).await,
        WasiSnapshotPreview1ClockResGet => clock_value(caller, params, true).await,
        WasiSnapshotPreview1RandomGet => random_get(caller, params).await,
        WasiSnapshotPreview1FdAllocate => fd_allocate(caller, params).await,
        WasiSnapshotPreview1FdClose => {
            simple_call(caller, "process.fd_close", vec![u32v(params, 0)?]).await
        }
        WasiSnapshotPreview1FdDatasync => {
            simple_call(caller, "process.fd_datasync", vec![u32v(params, 0)?]).await
        }
        WasiSnapshotPreview1FdSync => {
            simple_call(caller, "process.fd_sync", vec![u32v(params, 0)?]).await
        }
        WasiSnapshotPreview1FdFdstatGet => fd_fdstat_get(caller, params).await,
        WasiSnapshotPreview1FdFdstatSetFlags => fd_fdstat_set_flags(caller, params).await,
        WasiSnapshotPreview1FdFilestatGet => fd_filestat_get(caller, params).await,
        WasiSnapshotPreview1FdFilestatSetSize => {
            simple_call(
                caller,
                "process.fd_truncate",
                vec![u32v(params, 0)?, json!(i64_arg(params, 1)?.to_string())],
            )
            .await
        }
        WasiSnapshotPreview1FdFilestatSetTimes => fd_filestat_set_times(caller, params).await,
        WasiSnapshotPreview1FdPread => fd_read(caller, params, true).await,
        WasiSnapshotPreview1FdRead => fd_read(caller, params, false).await,
        WasiSnapshotPreview1FdPwrite => fd_write(caller, params, true).await,
        WasiSnapshotPreview1FdWrite => fd_write(caller, params, false).await,
        WasiSnapshotPreview1FdPrestatGet => fd_prestat_get(caller, params).await,
        WasiSnapshotPreview1FdPrestatDirName => fd_prestat_dir_name(caller, params).await,
        WasiSnapshotPreview1FdReaddir => fd_readdir(caller, params).await,
        WasiSnapshotPreview1FdRenumber => fd_renumber(caller, params).await,
        WasiSnapshotPreview1FdSeek => fd_seek(caller, params, false).await,
        WasiSnapshotPreview1FdTell => fd_seek(caller, params, true).await,
        WasiSnapshotPreview1PathCreateDirectory => path_create_directory(caller, params).await,
        WasiSnapshotPreview1PathFilestatGet => path_filestat_get(caller, params).await,
        WasiSnapshotPreview1PathFilestatSetTimes => path_filestat_set_times(caller, params).await,
        WasiSnapshotPreview1PathLink => path_link(caller, params).await,
        WasiSnapshotPreview1PathOpen => path_open(caller, params).await,
        WasiSnapshotPreview1PathReadlink => path_readlink(caller, params).await,
        WasiSnapshotPreview1PathRemoveDirectory => {
            path_one(caller, params, "process.path_remove_dir_at").await
        }
        WasiSnapshotPreview1PathRename => path_rename(caller, params).await,
        WasiSnapshotPreview1PathSymlink => path_symlink(caller, params).await,
        WasiSnapshotPreview1PathUnlinkFile => {
            path_one(caller, params, "process.path_unlink_at").await
        }
        WasiSnapshotPreview1PollOneoff => poll_oneoff(caller, params).await,
        WasiSnapshotPreview1SockShutdown => sock_shutdown(caller, params).await,
        _ => return Ok(false),
    };
    set_i32_result(results, status)?;
    Ok(true)
}

pub(super) async fn call(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    method: &str,
    args: Vec<Value>,
    raw: HashMap<usize, Vec<u8>>,
) -> Result<HostCallReply, HostServiceError> {
    // Clone the capability before awaiting. No Caller, Store borrow, Memory,
    // slice, or pointer-derived reference crosses this suspension point.
    let host = caller.data().host.clone();
    host.submit_adapter_call(method.to_owned(), args, raw).await
}

pub(super) async fn simple_call(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    method: &str,
    args: Vec<Value>,
) -> i32 {
    match call(caller, method, args, HashMap::new()).await {
        Ok(_) => SUCCESS,
        Err(error) => errno(&error),
    }
}

fn u32v(params: &[Val], index: usize) -> wasmtime::Result<Value> {
    Ok(json!(i32_arg(params, index)?))
}

pub(super) fn i64_arg(params: &[Val], index: usize) -> wasmtime::Result<u64> {
    match params.get(index) {
        Some(Val::I64(value)) => Ok(*value as u64),
        _ => Err(wasmtime::format_err!(
            "invalid i64 ABI argument at index {index}"
        )),
    }
}

pub(super) fn errno(error: &HostServiceError) -> i32 {
    match error.code.as_str() {
        "E2BIG" => ERRNO_2BIG,
        "EACCES" => ERRNO_ACCES,
        "EADDRINUSE" => ERRNO_ADDRINUSE,
        "EADDRNOTAVAIL" => ERRNO_ADDRNOTAVAIL,
        "EAFNOSUPPORT" => ERRNO_AFNOSUPPORT,
        "EAGAIN" | "EWOULDBLOCK" => ERRNO_AGAIN,
        "EALREADY" => ERRNO_ALREADY,
        "EBADF" => ERRNO_BADF,
        "EBUSY" => ERRNO_BUSY,
        "ECHILD" => ERRNO_CHILD,
        "ECONNREFUSED" => ERRNO_CONNREFUSED,
        "ECONNRESET" => ERRNO_CONNRESET,
        "EDEADLK" => ERRNO_DEADLK,
        "EDESTADDRREQ" => ERRNO_DESTADDRREQ,
        "EEXIST" => ERRNO_EXIST,
        "EFAULT" => ERRNO_FAULT,
        "EFBIG" => ERRNO_FBIG,
        "EHOSTUNREACH" => ERRNO_HOSTUNREACH,
        "EILSEQ" => ERRNO_ILSEQ,
        "EINPROGRESS" => ERRNO_INPROGRESS,
        "EINTR" => ERRNO_INTR,
        "EINVAL" => ERRNO_INVAL,
        "EIO" => ERRNO_IO,
        "EISCONN" => ERRNO_ISCONN,
        "EISDIR" => ERRNO_ISDIR,
        "ELOOP" => ERRNO_LOOP,
        "EMFILE" => ERRNO_MFILE,
        "EMSGSIZE" => ERRNO_MSGSIZE,
        "ENAMETOOLONG" => ERRNO_NAMETOOLONG,
        "ENETUNREACH" => ERRNO_NETUNREACH,
        "ENFILE" => ERRNO_NFILE,
        "ENOBUFS" => ERRNO_NOBUFS,
        "ENODATA" => ERRNO_NODATA,
        "ENOENT" => ERRNO_NOENT,
        "ENOEXEC" => ERRNO_NOEXEC,
        "ENOSPC" => ERRNO_NOSPC,
        "ENOSYS" => ERRNO_NOSYS,
        "ENOTCONN" => ERRNO_NOTCONN,
        "ENOTDIR" => ERRNO_NOTDIR,
        "ENOTEMPTY" => ERRNO_NOTEMPTY,
        "ENOTSOCK" => ERRNO_NOTSOCK,
        "ENOTSUP" | "EOPNOTSUPP" => ERRNO_NOTSUP,
        "ENXIO" => ERRNO_NXIO,
        "EOVERFLOW" => ERRNO_OVERFLOW,
        "EPERM" => ERRNO_PERM,
        "EPIPE" => ERRNO_PIPE,
        "EPROTONOSUPPORT" => ERRNO_PROTONOSUPPORT,
        "ERANGE" => ERRNO_RANGE,
        "EROFS" => ERRNO_ROFS,
        "ESPIPE" => ERRNO_SPIPE,
        "ESRCH" => ERRNO_SRCH,
        "ETIMEDOUT" => ERRNO_TIMEDOUT,
        "EXDEV" => ERRNO_XDEV,
        _ => ERRNO_IO,
    }
}

pub(super) fn json_reply(reply: HostCallReply) -> Result<Value, i32> {
    match reply {
        HostCallReply::Json(value) => Ok(value),
        _ => Err(ERRNO_IO),
    }
}

pub(super) fn reply_bytes(reply: HostCallReply) -> Result<Vec<u8>, i32> {
    match reply {
        HostCallReply::Raw(bytes) => Ok(bytes),
        HostCallReply::Json(Value::String(value)) => Ok(value.into_bytes()),
        HostCallReply::Json(value) => {
            let encoded = value
                .get("base64")
                .and_then(Value::as_str)
                .ok_or(ERRNO_IO)?;
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| ERRNO_IO)
        }
        HostCallReply::Empty => Err(ERRNO_IO),
    }
}

pub(super) fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

async fn clock_value(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    resolution: bool,
) -> i32 {
    let output = if resolution {
        i32_arg(params, 1)
    } else {
        i32_arg(params, 2)
    };
    let Ok(output) = output else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 8).is_err() {
        return ERRNO_FAULT;
    }
    let Ok(clock) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    let (method, args) = if resolution {
        ("process.clock_resolution", vec![json!(clock)])
    } else {
        let Ok(precision) = i64_arg(params, 1) else {
            return ERRNO_INVAL;
        };
        (
            "process.clock_time",
            vec![json!(clock), json!(precision.to_string()), Value::Null],
        )
    };
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(value) = value_u64(&value) else {
                return ERRNO_OVERFLOW;
            };
            if memory::validate_range(caller, output, 8).is_err() {
                return ERRNO_FAULT;
            }
            memory::write_u64(caller, output, value).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

async fn random_get(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let Ok(pointer) = i32_arg(params, 0) else {
        return ERRNO_FAULT;
    };
    let Ok(length) = i32_arg(params, 1).map(|value| value as usize) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, pointer, length).is_err() {
        return ERRNO_FAULT;
    }
    let mut output = Vec::with_capacity(length);
    while output.len() < length {
        let requested = (length - output.len()).min(64 * 1024);
        match call(
            caller,
            "process.random_get",
            vec![json!(requested)],
            HashMap::new(),
        )
        .await
        {
            Ok(reply) => match reply_bytes(reply) {
                Ok(bytes) if bytes.len() == requested => output.extend(bytes),
                _ => return ERRNO_IO,
            },
            Err(error) => return errno(&error),
        }
    }
    if memory::validate_range(caller, pointer, length).is_err() {
        return ERRNO_FAULT;
    }
    memory::write_bytes(caller, pointer, &output).map_or(ERRNO_FAULT, |_| SUCCESS)
}

async fn fd_allocate(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(offset), Ok(length)) =
        (i32_arg(params, 0), i64_arg(params, 1), i64_arg(params, 2))
    else {
        return ERRNO_INVAL;
    };
    simple_call(
        caller,
        "fs.fallocateSync",
        vec![json!(fd), json!(offset), json!(length)],
    )
    .await
}

async fn fd_fdstat_get(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(output)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 24).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, "process.fd_stat", vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => {
            let Ok(stat) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let filetype = stat.get("filetype").and_then(value_u64).unwrap_or(0) as u8;
            let kernel_flags = stat.get("flags").and_then(value_u64).unwrap_or(0) as u32;
            let flags = (if kernel_flags & 0x400 != 0 { 1 } else { 0 })
                | (if kernel_flags & 0x800 != 0 { 4 } else { 0 });
            let rights_base = stat.get("rightsBase").and_then(value_u64).unwrap_or(0);
            let rights_inheriting = stat
                .get("rightsInheriting")
                .and_then(value_u64)
                .unwrap_or(0);
            let mut bytes = [0u8; 24];
            bytes[0] = filetype;
            bytes[2..4].copy_from_slice(&(flags as u16).to_le_bytes());
            bytes[8..16].copy_from_slice(&rights_base.to_le_bytes());
            bytes[16..24].copy_from_slice(&rights_inheriting.to_le_bytes());
            commit(caller, output, &bytes)
        }
        Err(error) => errno(&error),
    }
}

async fn fd_fdstat_set_flags(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(flags)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    let kernel =
        (if flags & 1 != 0 { 0x400 } else { 0 }) | (if flags & 4 != 0 { 0x800 } else { 0 });
    simple_call(
        caller,
        "process.fd_set_flags",
        vec![json!(fd), json!(kernel)],
    )
    .await
}

async fn fd_filestat_get(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(output)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_FAULT;
    };
    filestat_call(caller, "process.fd_filestat", vec![json!(fd)], output).await
}

async fn fd_filestat_set_times(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(atime), Ok(mtime), Ok(flags)) = (
        i32_arg(params, 0),
        i64_arg(params, 1),
        i64_arg(params, 2),
        i32_arg(params, 3),
    ) else {
        return ERRNO_INVAL;
    };
    simple_call(
        caller,
        "process.fd_utimes",
        vec![
            json!(fd),
            json!(atime.to_string()),
            json!(mtime.to_string()),
            json!(flags),
        ],
    )
    .await
}

#[derive(Clone, Copy)]
struct Iovec {
    pointer: u32,
    length: usize,
}

async fn read_iovecs(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    count: u32,
    writable: bool,
) -> Result<Vec<Iovec>, i32> {
    let count = usize::try_from(count).map_err(|_| ERRNO_2BIG)?;
    let limit = super::check_fixed_request_limit(
        caller,
        "wasm.abi.maxIovecs",
        count,
        MAX_IOVECS,
        ERRNO_INVAL,
    )
    .await;
    if limit != SUCCESS {
        return Err(limit);
    }
    let descriptor_bytes = count.checked_mul(8).ok_or(ERRNO_2BIG)?;
    memory::validate_range(caller, pointer, descriptor_bytes).map_err(|_| ERRNO_FAULT)?;
    let mut iovecs = Vec::with_capacity(count);
    let mut total = 0usize;
    for index in 0..count {
        let slot = pointer
            .checked_add(u32::try_from(index * 8).map_err(|_| ERRNO_FAULT)?)
            .ok_or(ERRNO_FAULT)?;
        let data = memory::read_u32(caller, slot).map_err(|_| ERRNO_FAULT)?;
        let length = usize::try_from(memory::read_u32(caller, slot + 4).map_err(|_| ERRNO_FAULT)?)
            .map_err(|_| ERRNO_2BIG)?;
        memory::validate_range(caller, data, length).map_err(|_| ERRNO_FAULT)?;
        total = total.checked_add(length).ok_or(ERRNO_2BIG)?;
        iovecs.push(Iovec {
            pointer: data,
            length,
        });
    }
    let _ = writable;
    Ok(iovecs)
}

fn iovec_total(iovecs: &[Iovec]) -> Result<usize, i32> {
    iovecs.iter().try_fold(0usize, |total, iov| {
        total.checked_add(iov.length).ok_or(ERRNO_2BIG)
    })
}

fn collect_iovecs(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    iovecs: &[Iovec],
) -> Result<Vec<u8>, i32> {
    let mut bytes = Vec::with_capacity(iovec_total(iovecs)?);
    for iov in iovecs {
        bytes.extend(memory::read_bytes(caller, iov.pointer, iov.length).map_err(|_| ERRNO_FAULT)?);
    }
    Ok(bytes)
}

fn scatter_iovecs(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    iovecs: &[Iovec],
    bytes: &[u8],
) -> Result<usize, i32> {
    let mut offset = 0usize;
    for iov in iovecs {
        if offset == bytes.len() {
            break;
        }
        let length = iov.length.min(bytes.len() - offset);
        memory::write_bytes(caller, iov.pointer, &bytes[offset..offset + length])
            .map_err(|_| ERRNO_FAULT)?;
        offset += length;
    }
    Ok(offset)
}

async fn fd_read(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    positioned: bool,
) -> i32 {
    let (Ok(fd), Ok(iovs_ptr), Ok(iovs_len)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    let result_index = if positioned { 4 } else { 3 };
    let Ok(result_ptr) = i32_arg(params, result_index) else {
        return ERRNO_FAULT;
    };
    let iovecs = match read_iovecs(caller, iovs_ptr, iovs_len, true).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if memory::validate_range(caller, result_ptr, 4).is_err() {
        return ERRNO_FAULT;
    }
    let Ok(total) = iovec_total(&iovecs) else {
        return ERRNO_2BIG;
    };
    let mut args = vec![json!(fd), json!(total)];
    let method = if positioned {
        let Ok(offset) = i64_arg(params, 3) else {
            return ERRNO_INVAL;
        };
        args.push(json!(offset.to_string()));
        "process.fd_pread"
    } else {
        args.push(Value::Null);
        "process.fd_read"
    };
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(bytes) = reply_bytes(reply) else {
                return ERRNO_IO;
            };
            if bytes.len() > total {
                return ERRNO_IO;
            }
            if read_iovecs(caller, iovs_ptr, iovs_len, true).await.is_err()
                || memory::validate_range(caller, result_ptr, 4).is_err()
            {
                return ERRNO_FAULT;
            }
            let Ok(written) = scatter_iovecs(caller, &iovecs, &bytes) else {
                return ERRNO_FAULT;
            };
            memory::write_u32(caller, result_ptr, written as u32).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

async fn fd_write(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    positioned: bool,
) -> i32 {
    let (Ok(fd), Ok(iovs_ptr), Ok(iovs_len)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    let result_index = if positioned { 4 } else { 3 };
    let Ok(result_ptr) = i32_arg(params, result_index) else {
        return ERRNO_FAULT;
    };
    let iovecs = match read_iovecs(caller, iovs_ptr, iovs_len, false).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    if memory::validate_range(caller, result_ptr, 4).is_err() {
        return ERRNO_FAULT;
    }
    let bytes = match collect_iovecs(caller, &iovecs) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut args = vec![json!(fd), Value::Null];
    let method = if positioned {
        let Ok(offset) = i64_arg(params, 3) else {
            return ERRNO_INVAL;
        };
        args.push(json!(offset.to_string()));
        "process.fd_pwrite"
    } else {
        "process.fd_write"
    };
    let mut raw = HashMap::new();
    raw.insert(1, bytes.clone());
    // Stdio aliases must use the shared ordered-output operation so host
    // capture, PTY cooking, and 1>&2/2>&1 routing match the V8 adapter. The
    // typed operation rejects ordinary descriptors with EINVAL without a
    // write side effect; those continue through the regular fd path.
    let reply = if positioned {
        call(caller, method, args, raw).await
    } else {
        match call(caller, "__kernel_stdio_write", args.clone(), raw.clone()).await {
            Err(error) if error.code == "EINVAL" => call(caller, method, args, raw).await,
            result => result,
        }
    };
    match reply {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(written) = value_u64(&value).and_then(|value| u32::try_from(value).ok())
            else {
                return ERRNO_IO;
            };
            if written as usize > bytes.len() {
                return ERRNO_IO;
            }
            if memory::validate_range(caller, result_ptr, 4).is_err() {
                return ERRNO_FAULT;
            }
            memory::write_u32(caller, result_ptr, written).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

async fn fd_prestat_get(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(output)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 8).is_err() {
        return ERRNO_FAULT;
    }
    match preopen(caller, fd).await {
        Ok(path) => {
            let mut bytes = [0u8; 8];
            bytes[4..8].copy_from_slice(&(path.len() as u32).to_le_bytes());
            commit(caller, output, &bytes)
        }
        Err(error) => error,
    }
}

async fn fd_prestat_dir_name(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(output), Ok(length)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, length as usize).is_err() {
        return ERRNO_FAULT;
    }
    match preopen(caller, fd).await {
        Ok(path) if path.len() <= length as usize => commit(caller, output, path.as_bytes()),
        Ok(_) => ERRNO_NAMETOOLONG,
        Err(error) => error,
    }
}

async fn preopen(caller: &mut Caller<'_, WasmtimeStoreState>, fd: u32) -> Result<String, i32> {
    match call(
        caller,
        "process.fd_preopen",
        vec![json!(fd)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => json_reply(reply)?
            .get("guestPath")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(ERRNO_BADF),
        Err(error) => Err(errno(&error)),
    }
}

async fn fd_readdir(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(output), Ok(length), Ok(cookie), Ok(used_ptr)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i64_arg(params, 3),
        i32_arg(params, 4),
    ) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, length as usize).is_err()
        || memory::validate_range(caller, used_ptr, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    let max_entries = ((length as usize / 24) + 1).clamp(1, 4096);
    match call(
        caller,
        "process.fd_readdir",
        vec![json!(fd), json!(cookie.to_string()), json!(max_entries)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(entries) = value.as_array() else {
                return ERRNO_IO;
            };
            let mut bytes = Vec::with_capacity(length as usize);
            for entry in entries {
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .as_bytes();
                let mut record = vec![0u8; 24 + name.len()];
                record[0..8].copy_from_slice(
                    &entry
                        .get("next")
                        .and_then(value_u64)
                        .unwrap_or(0)
                        .to_le_bytes(),
                );
                record[8..16].copy_from_slice(
                    &entry
                        .get("ino")
                        .and_then(value_u64)
                        .unwrap_or(0)
                        .to_le_bytes(),
                );
                record[16..20].copy_from_slice(&(name.len() as u32).to_le_bytes());
                record[20] = entry.get("filetype").and_then(value_u64).unwrap_or(0) as u8;
                record[24..].copy_from_slice(name);
                let remaining = length as usize - bytes.len();
                bytes.extend_from_slice(&record[..record.len().min(remaining)]);
                if bytes.len() == length as usize {
                    break;
                }
            }
            if memory::validate_range(caller, output, length as usize).is_err()
                || memory::validate_range(caller, used_ptr, 4).is_err()
            {
                return ERRNO_FAULT;
            }
            if memory::write_bytes(caller, output, &bytes).is_err() {
                return ERRNO_FAULT;
            }
            memory::write_u32(caller, used_ptr, bytes.len() as u32).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

async fn fd_renumber(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(from), Ok(to)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_BADF;
    };
    if from == to {
        return SUCCESS;
    }
    match call(
        caller,
        "process.fd_move",
        vec![json!(from), json!(to)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => match json_reply(reply).ok().as_ref().and_then(value_u64) {
            Some(value) if value == u64::from(to) => SUCCESS,
            Some(_) => ERRNO_IO,
            None => ERRNO_IO,
        },
        Err(error) => errno(&error),
    }
}

async fn fd_seek(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], tell: bool) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_BADF;
    };
    let (offset, whence, output) = if tell {
        let Ok(output) = i32_arg(params, 1) else {
            return ERRNO_FAULT;
        };
        (0i64, 1u32, output)
    } else {
        let (Ok(raw_offset), Ok(whence), Ok(output)) =
            (i64_arg(params, 1), i32_arg(params, 2), i32_arg(params, 3))
        else {
            return ERRNO_INVAL;
        };
        (raw_offset as i64, whence, output)
    };
    if memory::validate_range(caller, output, 8).is_err() {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.fd_seek",
        vec![json!(fd), json!(offset.to_string()), json!(whence)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(next) = value_u64(&value) else {
                return ERRNO_OVERFLOW;
            };
            if memory::validate_range(caller, output, 8).is_err() {
                return ERRNO_FAULT;
            }
            memory::write_u64(caller, output, next).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

fn guest_path(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    pointer_index: usize,
    length_index: usize,
) -> Result<String, i32> {
    let pointer = i32_arg(params, pointer_index).map_err(|_| ERRNO_FAULT)?;
    let length = i32_arg(params, length_index).map_err(|_| ERRNO_FAULT)? as usize;
    memory::read_string(caller, pointer, length).map_err(|_| ERRNO_FAULT)
}

async fn path_create_directory(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    path_one(caller, params, "process.path_mkdir_at").await
}

async fn path_one(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    method: &str,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_BADF;
    };
    let Ok(path) = guest_path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    simple_call(caller, method, vec![json!(fd), json!(path)]).await
}

async fn path_filestat_get(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(flags), Ok(output)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 4))
    else {
        return ERRNO_FAULT;
    };
    let Ok(path) = guest_path(caller, params, 2, 3) else {
        return ERRNO_FAULT;
    };
    filestat_call(
        caller,
        "process.path_stat_at",
        vec![json!(fd), json!(path), json!(flags & 1 != 0)],
        output,
    )
    .await
}

async fn filestat_call(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    method: &str,
    args: Vec<Value>,
    output: u32,
) -> i32 {
    if memory::validate_range(caller, output, 64).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(stat) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let mut bytes = [0u8; 64];
            bytes[8..16].copy_from_slice(
                &stat
                    .get("ino")
                    .and_then(value_u64)
                    .unwrap_or(0)
                    .to_le_bytes(),
            );
            bytes[16] = stat.get("filetype").and_then(value_u64).unwrap_or(0) as u8;
            bytes[24..32].copy_from_slice(
                &stat
                    .get("nlink")
                    .and_then(value_u64)
                    .unwrap_or(1)
                    .to_le_bytes(),
            );
            bytes[32..40].copy_from_slice(
                &stat
                    .get("size")
                    .and_then(value_u64)
                    .unwrap_or(0)
                    .to_le_bytes(),
            );
            for (offset, name) in [(40, "atimeMs"), (48, "mtimeMs"), (56, "ctimeMs")] {
                let ns = stat
                    .get(name)
                    .and_then(Value::as_f64)
                    .map(|value| (value * 1_000_000.0) as u64)
                    .or_else(|| {
                        stat.get(name)
                            .and_then(value_u64)
                            .map(|value| value.saturating_mul(1_000_000))
                    })
                    .unwrap_or(0);
                bytes[offset..offset + 8].copy_from_slice(&ns.to_le_bytes());
            }
            commit(caller, output, &bytes)
        }
        Err(error) => errno(&error),
    }
}

async fn path_filestat_set_times(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
) -> i32 {
    let (Ok(fd), Ok(lookup), Ok(atime), Ok(mtime), Ok(flags)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i64_arg(params, 4),
        i64_arg(params, 5),
        i32_arg(params, 6),
    ) else {
        return ERRNO_INVAL;
    };
    let Ok(path) = guest_path(caller, params, 2, 3) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_utimes_at",
        vec![
            json!(fd),
            json!(path),
            json!(lookup & 1 != 0),
            json!(atime.to_string()),
            json!(mtime.to_string()),
            json!(flags),
        ],
    )
    .await
}

async fn path_link(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(old_fd), Ok(flags), Ok(new_fd)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 4))
    else {
        return ERRNO_BADF;
    };
    let Ok(old_path) = guest_path(caller, params, 2, 3) else {
        return ERRNO_FAULT;
    };
    let Ok(new_path) = guest_path(caller, params, 5, 6) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_link_at",
        vec![
            json!(old_fd),
            json!(old_path),
            json!(new_fd),
            json!(new_path),
            json!(flags & 1 != 0),
        ],
    )
    .await
}

async fn path_open(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (
        Ok(fd),
        Ok(lookup),
        Ok(oflags),
        Ok(rights_base),
        Ok(rights_inheriting),
        Ok(fdflags),
        Ok(output),
    ) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 4),
        i64_arg(params, 5),
        i64_arg(params, 6),
        i32_arg(params, 7),
        i32_arg(params, 8),
    )
    else {
        return ERRNO_INVAL;
    };
    let Ok(path) = guest_path(caller, params, 2, 3) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    let create_mode = caller.data_mut().pending_open_mode.take().unwrap_or(0o666);
    let direct = std::mem::take(&mut caller.data_mut().pending_open_direct);
    const RIGHT_READ: u64 = 1 << 1;
    const RIGHT_WRITE: u64 = 1 << 6;
    let mut flags = if rights_base & RIGHT_WRITE != 0 {
        if rights_base & RIGHT_READ != 0 {
            2
        } else {
            1
        }
    } else {
        0
    };
    if oflags & 1 != 0 {
        flags |= 0x40;
    }
    if oflags & 2 != 0 {
        flags |= 0x10000;
    }
    if oflags & 4 != 0 {
        flags |= 0x80;
    }
    if oflags & 8 != 0 {
        flags |= 0x200;
    }
    if fdflags & 1 != 0 {
        flags |= 0x400;
    }
    if fdflags & 4 != 0 {
        flags |= 0x800;
    }
    if direct {
        flags |= 0x4000;
    }
    if lookup & 1 == 0 {
        flags |= 0x20000;
    }
    match call(
        caller,
        "process.path_open_at",
        vec![
            json!(fd),
            json!(path),
            json!(flags),
            json!(create_mode),
            json!(rights_base.to_string()),
            json!(rights_inheriting.to_string()),
        ],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(opened) = value_u64(&value).and_then(|value| u32::try_from(value).ok()) else {
                return ERRNO_IO;
            };
            if memory::validate_range(caller, output, 4).is_err() {
                if let Err(error) = call(
                    caller,
                    "process.fd_close",
                    vec![json!(opened)],
                    HashMap::new(),
                )
                .await
                {
                    eprintln!(
                        "ERR_AGENTOS_WASMTIME_FD_ROLLBACK: context=path_open output commit fd={opened} code={}",
                        error.code
                    );
                }
                return ERRNO_FAULT;
            }
            memory::write_u32(caller, output, opened).map_or(ERRNO_FAULT, |_| SUCCESS)
        }
        Err(error) => errno(&error),
    }
}

async fn path_readlink(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(output), Ok(length), Ok(used)) = (
        i32_arg(params, 0),
        i32_arg(params, 3),
        i32_arg(params, 4),
        i32_arg(params, 5),
    ) else {
        return ERRNO_FAULT;
    };
    let Ok(path) = guest_path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, length as usize).is_err()
        || memory::validate_range(caller, used, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.path_readlink_at",
        vec![json!(fd), json!(path)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(target) = value.as_str() else {
                return ERRNO_IO;
            };
            let bytes = target.as_bytes();
            let written = bytes.len().min(length as usize);
            if memory::write_bytes(caller, output, &bytes[..written]).is_err()
                || memory::write_u32(caller, used, written as u32).is_err()
            {
                ERRNO_FAULT
            } else {
                SUCCESS
            }
        }
        Err(error) => errno(&error),
    }
}

async fn path_rename(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(old_fd), Ok(new_fd)) = (i32_arg(params, 0), i32_arg(params, 3)) else {
        return ERRNO_BADF;
    };
    let Ok(old_path) = guest_path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    let Ok(new_path) = guest_path(caller, params, 4, 5) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_rename_at",
        vec![
            json!(old_fd),
            json!(old_path),
            json!(new_fd),
            json!(new_path),
        ],
    )
    .await
}

async fn path_symlink(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let Ok(old_path) = guest_path(caller, params, 0, 1) else {
        return ERRNO_FAULT;
    };
    let Ok(fd) = i32_arg(params, 2) else {
        return ERRNO_BADF;
    };
    let Ok(new_path) = guest_path(caller, params, 3, 4) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_symlink_at",
        vec![json!(old_path), json!(fd), json!(new_path)],
    )
    .await
}

async fn sock_shutdown(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(how)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    let mode = match how {
        1 => 0,
        2 => 1,
        3 => 2,
        _ => return ERRNO_INVAL,
    };
    simple_call(
        caller,
        "process.fd_socket_shutdown",
        vec![json!(fd), json!(mode)],
    )
    .await
}

async fn poll_oneoff(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(input), Ok(output), Ok(count), Ok(event_count)) = (
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
        "wasm.abi.maxPollSubscriptions",
        count,
        MAX_POLL_SUBSCRIPTIONS,
        ERRNO_INVAL,
    )
    .await;
    if limit != SUCCESS {
        return limit;
    }
    if memory::validate_range(caller, input, count.saturating_mul(48)).is_err()
        || memory::validate_range(caller, output, count.saturating_mul(32)).is_err()
        || memory::validate_range(caller, event_count, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    let mut interests = Vec::new();
    let mut subscriptions = Vec::with_capacity(count);
    let mut timeout_ms: Option<u64> = None;
    for index in 0..count {
        let base = input + (index * 48) as u32;
        let userdata = memory::read_u64(caller, base).unwrap_or(0);
        let tag = memory::read_bytes(caller, base + 8, 1)
            .ok()
            .and_then(|bytes| bytes.first().copied())
            .unwrap_or(255);
        if tag == 0 {
            let timeout_ns = memory::read_u64(caller, base + 24).unwrap_or(0);
            timeout_ms = Some(timeout_ms.map_or(timeout_ns / 1_000_000, |prior| {
                prior.min(timeout_ns / 1_000_000)
            }));
            subscriptions.push((userdata, tag, 0u32));
        } else if tag == 1 || tag == 2 {
            let fd = memory::read_u32(caller, base + 16).unwrap_or(u32::MAX);
            interests.push(json!({ "fd": fd, "events": if tag == 1 { 1 } else { 4 } }));
            subscriptions.push((userdata, tag, fd));
        } else {
            subscriptions.push((userdata, tag, 0));
        }
    }
    let reply = if interests.is_empty() {
        if let Some(delay) = timeout_ms {
            simple_call(caller, "process.sleep", vec![json!(delay)]).await;
        }
        json!({"fds": []})
    } else {
        match call(
            caller,
            "__kernel_poll",
            vec![
                Value::Array(interests),
                timeout_ms.map(Value::from).unwrap_or(Value::Null),
            ],
            HashMap::new(),
        )
        .await
        {
            Ok(reply) => match json_reply(reply) {
                Ok(value) => value,
                Err(error) => return error,
            },
            Err(error) => return errno(&error),
        }
    };
    let fds = reply
        .get("fds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut encoded = Vec::new();
    for (userdata, tag, fd) in subscriptions {
        let ready = if tag == 0 {
            true
        } else {
            fds.iter().any(|entry| {
                entry.get("fd").and_then(value_u64) == Some(u64::from(fd))
                    && entry.get("revents").and_then(value_u64).unwrap_or(0) != 0
            })
        };
        if !ready {
            continue;
        }
        let mut event = [0u8; 32];
        event[0..8].copy_from_slice(&userdata.to_le_bytes());
        event[10] = tag;
        if tag == 1 {
            event[16..24].copy_from_slice(&1u64.to_le_bytes());
        }
        if tag == 2 {
            event[16..24].copy_from_slice(&65536u64.to_le_bytes());
        }
        encoded.extend_from_slice(&event);
    }
    if memory::validate_range(caller, output, count.saturating_mul(32)).is_err()
        || memory::validate_range(caller, event_count, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    if memory::write_bytes(caller, output, &encoded).is_err()
        || memory::write_u32(caller, event_count, (encoded.len() / 32) as u32).is_err()
    {
        ERRNO_FAULT
    } else {
        SUCCESS
    }
}

pub(super) fn commit(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    output: u32,
    bytes: &[u8],
) -> i32 {
    if memory::validate_range(caller, output, bytes.len()).is_err() {
        return ERRNO_FAULT;
    }
    memory::write_bytes(caller, output, bytes).map_or(ERRNO_FAULT, |_| SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_errno_mapping_matches_preview1() {
        assert_eq!(errno(&HostServiceError::new("EBADF", "bad fd")), 8);
        assert_eq!(errno(&HostServiceError::new("EWOULDBLOCK", "wait")), 6);
        assert_eq!(errno(&HostServiceError::new("unknown", "fault")), 29);
    }
}
