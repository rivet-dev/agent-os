//! AgentOS filesystem extension ABI codecs.

use super::preview1::{
    call, commit, errno, i64_arg, json_reply, reply_bytes, simple_call, value_u64, ERRNO_2BIG,
    ERRNO_FAULT, ERRNO_INVAL, ERRNO_IO, ERRNO_NODATA, ERRNO_RANGE, SUCCESS,
};
use super::{i32_arg, set_i32_result};
use crate::abi::{AbiBinding, ImportId};
use crate::wasm::wasmtime::{memory, store::WasmtimeStoreState};
use serde_json::{json, Value};
use std::collections::HashMap;
use wasmtime::{Caller, Val};

const XATTR_NAME_MAX: usize = 255;
const XATTR_SIZE_MAX: usize = 64 * 1024;

pub async fn dispatch(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<bool> {
    use ImportId::*;
    let name_length_index = match abi.id {
        HostFsPathGetxattr | HostFsPathSetxattr | HostFsPathRemovexattr => Some(4),
        HostFsFdGetxattr | HostFsFdSetxattr | HostFsFdRemovexattr => Some(2),
        _ => None,
    };
    if let Some(index) = name_length_index {
        if let Ok(length) = i32_arg(params, index) {
            let status = super::check_fixed_request_limit(
                caller,
                "wasm.abi.maxXattrNameBytes",
                length as usize,
                XATTR_NAME_MAX,
                ERRNO_RANGE,
            )
            .await;
            if status != SUCCESS {
                set_i32_result(results, status)?;
                return Ok(true);
            }
        }
    }
    let value_length_index = match abi.id {
        HostFsPathSetxattr => Some(6),
        HostFsFdSetxattr => Some(4),
        _ => None,
    };
    if let Some(index) = value_length_index {
        if let Ok(length) = i32_arg(params, index) {
            let status = super::check_fixed_request_limit(
                caller,
                "wasm.abi.maxXattrValueBytes",
                length as usize,
                XATTR_SIZE_MAX,
                ERRNO_2BIG,
            )
            .await;
            if status != SUCCESS {
                set_i32_result(results, status)?;
                return Ok(true);
            }
        }
    }
    let status = match abi.id {
        HostFsSetOpenMode => {
            caller.data_mut().pending_open_mode = Some(i32_arg(params, 0)? & 0o7777);
            SUCCESS
        }
        HostFsSetOpenDirect => {
            caller.data_mut().pending_open_direct = i32_arg(params, 0)? != 0;
            SUCCESS
        }
        HostFsChmod => path_chmod(caller, params).await,
        HostFsFchmod => fd_mode_set(caller, params).await,
        HostFsChown | HostFsPathChown => path_chown(caller, params).await,
        HostFsFchown | HostFsFdChown => fd_owner_set(caller, params).await,
        HostFsFtruncate => {
            let (Ok(fd), Ok(length)) = (i32_arg(params, 0), i64_arg(params, 1)) else {
                set_i32_result(results, ERRNO_INVAL)?;
                return Ok(true);
            };
            simple_call(
                caller,
                "process.fd_truncate",
                vec![json!(fd), json!(length.to_string())],
            )
            .await
        }
        HostFsOpenTmpfile => open_tmpfile(caller, params).await,
        HostFsFdLink => fd_link(caller, params).await,
        HostFsRemount => remount(caller, params).await,
        HostFsPathMknod => path_mknod(caller, params).await,
        HostFsPathRenameat2 => path_renameat2(caller, params).await,
        HostFsPathStatfs => path_statfs(caller, params).await,
        HostFsFdFiemap => fd_fiemap(caller, params).await,
        HostFsFdPunchHole => range(caller, params, "fs.punchHoleSync", false).await,
        HostFsFdZeroRange => range(caller, params, "fs.zeroRangeSync", true).await,
        HostFsFdInsertRange => range(caller, params, "fs.insertRangeSync", false).await,
        HostFsFdCollapseRange => range(caller, params, "fs.collapseRangeSync", false).await,
        HostFsPathOwner => owner_get(caller, params, true).await,
        HostFsFdOwner => owner_get(caller, params, false).await,
        HostFsPathAccess => path_access(caller, params).await,
        HostFsPathGetxattr => xattr_get(caller, params, true).await,
        HostFsFdGetxattr => xattr_get(caller, params, false).await,
        HostFsPathListxattr => xattr_list(caller, params, true).await,
        HostFsFdListxattr => xattr_list(caller, params, false).await,
        HostFsPathSetxattr => xattr_set(caller, params, true).await,
        HostFsFdSetxattr => xattr_set(caller, params, false).await,
        HostFsPathRemovexattr => xattr_remove(caller, params, true).await,
        HostFsFdRemovexattr => xattr_remove(caller, params, false).await,
        HostFsFdMode => return scalar_stat(caller, params, results, false, "mode", false, 0).await,
        HostFsFdSize => {
            return scalar_stat(caller, params, results, false, "size", true, u64::MAX).await
        }
        HostFsFdBlocks => {
            return scalar_stat(caller, params, results, false, "blocks", true, u64::MAX).await
        }
        HostFsPathMode => {
            return scalar_stat(caller, params, results, true, "mode", false, 0).await
        }
        HostFsPathSize => {
            return scalar_stat(caller, params, results, true, "size", true, u64::MAX).await
        }
        HostFsPathBlocks => {
            return scalar_stat(caller, params, results, true, "blocks", true, u64::MAX).await
        }
        HostFsPathRdev => return scalar_stat(caller, params, results, true, "rdev", true, 0).await,
        _ => return Ok(false),
    };
    set_i32_result(results, status)?;
    Ok(true)
}

fn path(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    pointer: usize,
    length: usize,
) -> Result<String, i32> {
    let pointer = i32_arg(params, pointer).map_err(|_| ERRNO_FAULT)?;
    let length = i32_arg(params, length).map_err(|_| ERRNO_FAULT)? as usize;
    memory::read_string(caller, pointer, length).map_err(|_| ERRNO_FAULT)
}

async fn path_chmod(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(mode)) = (i32_arg(params, 0), i32_arg(params, 3)) else {
        return ERRNO_INVAL;
    };
    let Ok(path) = path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_chmod_at",
        vec![json!(fd), json!(path), json!(mode)],
    )
    .await
}

async fn fd_mode_set(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(mode)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    simple_call(caller, "process.fd_chmod", vec![json!(fd), json!(mode)]).await
}

async fn path_chown(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(uid), Ok(gid), Ok(follow)) = (
        i32_arg(params, 0),
        i32_arg(params, 3),
        i32_arg(params, 4),
        i32_arg(params, 5),
    ) else {
        return ERRNO_INVAL;
    };
    let Ok(path) = path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_chown_at",
        vec![
            json!(fd),
            json!(path),
            json!(uid),
            json!(gid),
            json!(follow != 0),
        ],
    )
    .await
}

async fn fd_owner_set(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(uid), Ok(gid)) = (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_INVAL;
    };
    simple_call(
        caller,
        "process.fd_chown",
        vec![json!(fd), json!(uid), json!(gid)],
    )
    .await
}

async fn open_tmpfile(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(flags), Ok(mode), Ok(output)) = (
        i32_arg(params, 0),
        i32_arg(params, 3),
        i32_arg(params, 4),
        i32_arg(params, 5),
    ) else {
        return ERRNO_FAULT;
    };
    let Ok(path) = path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.open_tmpfile_at",
        vec![
            json!(fd),
            json!(path),
            json!(flags),
            json!(mode),
            json!(flags & (1 << 15) == 0),
        ],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => resource_u32(caller, output, reply, "process.fd_close").await,
        Err(error) => errno(&error),
    }
}

async fn fd_link(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(dir_fd)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    let Ok(path) = path(caller, params, 2, 3) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.fd_link_at",
        vec![json!(fd), json!(dir_fd), json!(path)],
    )
    .await
}

async fn remount(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(target), Ok(options)) = (path(caller, params, 0, 1), path(caller, params, 2, 3)) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "fs.remountSync",
        vec![json!(target), json!(options)],
    )
    .await
}

async fn path_mknod(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(mode), Ok(device)) =
        (i32_arg(params, 0), i32_arg(params, 3), i64_arg(params, 4))
    else {
        return ERRNO_INVAL;
    };
    let Ok(path) = path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_mknod_at",
        vec![json!(fd), json!(path), json!(mode), json!(device)],
    )
    .await
}

async fn path_renameat2(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(old_fd), Ok(new_fd), Ok(flags)) =
        (i32_arg(params, 0), i32_arg(params, 3), i32_arg(params, 6))
    else {
        return ERRNO_INVAL;
    };
    let (Ok(old_path), Ok(new_path)) = (path(caller, params, 1, 2), path(caller, params, 4, 5))
    else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_rename_at2",
        vec![
            json!(old_fd),
            json!(old_path),
            json!(new_fd),
            json!(new_path),
            json!(flags),
        ],
    )
    .await
}

async fn path_statfs(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    let Ok(path) = path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    let outputs = match (3..8)
        .map(|index| i32_arg(params, index))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(value) => value,
        Err(_) => return ERRNO_FAULT,
    };
    if outputs
        .iter()
        .any(|output| memory::validate_range(caller, *output, 8).is_err())
    {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.path_statfs_at",
        vec![json!(fd), json!(path)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let fields = [
                "totalBytes",
                "usedBytes",
                "availableBytes",
                "totalInodes",
                "freeInodes",
            ];
            let Some(values) = fields
                .iter()
                .map(|field| value.get(*field).and_then(value_u64))
                .collect::<Option<Vec<_>>>()
            else {
                return ERRNO_IO;
            };
            if outputs
                .iter()
                .any(|output| memory::validate_range(caller, *output, 8).is_err())
            {
                return ERRNO_FAULT;
            }
            for (output, value) in outputs.into_iter().zip(values) {
                if memory::write_u64(caller, output, value).is_err() {
                    return ERRNO_FAULT;
                }
            }
            SUCCESS
        }
        Err(error) => errno(&error),
    }
}

async fn fd_fiemap(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(index), Ok(start), Ok(end), Ok(flags)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
    ) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, start, 8).is_err()
        || memory::validate_range(caller, end, 8).is_err()
        || memory::validate_range(caller, flags, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "fs.fiemapAtSync",
        vec![json!(fd), json!(index)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            if value.is_null() {
                return ERRNO_NODATA;
            }
            let (Some(first), Some(last)) = (
                value.get("start").and_then(value_u64),
                value.get("end").and_then(value_u64),
            ) else {
                return ERRNO_IO;
            };
            if memory::write_u64(caller, start, first).is_err()
                || memory::write_u64(caller, end, last).is_err()
                || memory::write_u32(
                    caller,
                    flags,
                    if value
                        .get("unwritten")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        0x800
                    } else {
                        0
                    },
                )
                .is_err()
            {
                ERRNO_FAULT
            } else {
                SUCCESS
            }
        }
        Err(error) => errno(&error),
    }
}

async fn range(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    method: &str,
    keep_size: bool,
) -> i32 {
    let (Ok(fd), Ok(offset), Ok(length)) =
        (i32_arg(params, 0), i64_arg(params, 1), i64_arg(params, 2))
    else {
        return ERRNO_INVAL;
    };
    let mut args = vec![json!(fd), json!(offset), json!(length)];
    if keep_size {
        args.push(json!(i32_arg(params, 3).unwrap_or(0)));
    }
    simple_call(caller, method, args).await
}

async fn owner_get(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    is_path: bool,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    let (args, uid_output, gid_output, method) = if is_path {
        let Ok(path) = path(caller, params, 1, 2) else {
            return ERRNO_FAULT;
        };
        let (Ok(follow), Ok(uid), Ok(gid)) =
            (i32_arg(params, 3), i32_arg(params, 4), i32_arg(params, 5))
        else {
            return ERRNO_FAULT;
        };
        (
            vec![json!(fd), json!(path), json!(follow != 0)],
            uid,
            gid,
            "process.path_stat_at",
        )
    } else {
        let (Ok(uid), Ok(gid)) = (i32_arg(params, 1), i32_arg(params, 2)) else {
            return ERRNO_FAULT;
        };
        (vec![json!(fd)], uid, gid, "process.fd_filestat")
    };
    if memory::validate_range(caller, uid_output, 4).is_err()
        || memory::validate_range(caller, gid_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let (Some(uid), Some(gid)) = (
                value
                    .get("uid")
                    .and_then(value_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                value
                    .get("gid")
                    .and_then(value_u64)
                    .and_then(|value| u32::try_from(value).ok()),
            ) else {
                return ERRNO_IO;
            };
            if memory::write_u32(caller, uid_output, uid).is_err()
                || memory::write_u32(caller, gid_output, gid).is_err()
            {
                ERRNO_FAULT
            } else {
                SUCCESS
            }
        }
        Err(error) => errno(&error),
    }
}

async fn path_access(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(mode), Ok(effective)) =
        (i32_arg(params, 0), i32_arg(params, 3), i32_arg(params, 4))
    else {
        return ERRNO_INVAL;
    };
    let Ok(path) = path(caller, params, 1, 2) else {
        return ERRNO_FAULT;
    };
    simple_call(
        caller,
        "process.path_access_at",
        vec![json!(fd), json!(path), json!(mode), json!(effective != 0)],
    )
    .await
}

async fn xattr_get(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    is_path: bool,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    let (args, value_ptr, capacity, size_ptr, method) = if is_path {
        let Ok(path) = path(caller, params, 1, 2) else {
            return ERRNO_FAULT;
        };
        let Ok(name) = bounded_name(caller, params, 3, 4) else {
            return ERRNO_RANGE;
        };
        let (Ok(value), Ok(capacity), Ok(follow), Ok(size)) = (
            i32_arg(params, 5),
            i32_arg(params, 6),
            i32_arg(params, 7),
            i32_arg(params, 8),
        ) else {
            return ERRNO_FAULT;
        };
        (
            vec![json!(fd), json!(path), json!(name), json!(follow != 0)],
            value,
            capacity,
            size,
            "process.path_getxattr_at",
        )
    } else {
        let Ok(name) = bounded_name(caller, params, 1, 2) else {
            return ERRNO_RANGE;
        };
        let (Ok(value), Ok(capacity), Ok(size)) =
            (i32_arg(params, 3), i32_arg(params, 4), i32_arg(params, 5))
        else {
            return ERRNO_FAULT;
        };
        (
            vec![json!(fd), json!(name)],
            value,
            capacity,
            size,
            "fs.fgetxattrSync",
        )
    };
    if memory::validate_range(caller, value_ptr, capacity as usize).is_err()
        || memory::validate_range(caller, size_ptr, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => publish_bytes(
            caller,
            value_ptr,
            capacity,
            size_ptr,
            match reply_bytes(reply) {
                Ok(value) => value,
                Err(error) => return error,
            },
        ),
        Err(error) => errno(&error),
    }
}

async fn xattr_list(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    is_path: bool,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    let (args, output, capacity, size_output, method) = if is_path {
        let Ok(path) = path(caller, params, 1, 2) else {
            return ERRNO_FAULT;
        };
        let (Ok(output), Ok(capacity), Ok(follow), Ok(size)) = (
            i32_arg(params, 3),
            i32_arg(params, 4),
            i32_arg(params, 5),
            i32_arg(params, 6),
        ) else {
            return ERRNO_FAULT;
        };
        (
            vec![json!(fd), json!(path), json!(follow != 0)],
            output,
            capacity,
            size,
            "process.path_listxattr_at",
        )
    } else {
        let (Ok(output), Ok(capacity), Ok(size)) =
            (i32_arg(params, 1), i32_arg(params, 2), i32_arg(params, 3))
        else {
            return ERRNO_FAULT;
        };
        (vec![json!(fd)], output, capacity, size, "fs.flistxattrSync")
    };
    if memory::validate_range(caller, output, capacity as usize).is_err()
        || memory::validate_range(caller, size_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(names) = value.as_array() else {
                return ERRNO_IO;
            };
            let mut bytes = Vec::new();
            for name in names.iter().filter_map(Value::as_str) {
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(0);
            }
            publish_bytes(caller, output, capacity, size_output, bytes)
        }
        Err(error) => errno(&error),
    }
}

async fn xattr_set(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    is_path: bool,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    let (args, raw_index, raw, method) = if is_path {
        let Ok(path) = path(caller, params, 1, 2) else {
            return ERRNO_FAULT;
        };
        let Ok(name) = bounded_name(caller, params, 3, 4) else {
            return ERRNO_RANGE;
        };
        let (Ok(value_ptr), Ok(length), Ok(flags), Ok(follow)) = (
            i32_arg(params, 5),
            i32_arg(params, 6),
            i32_arg(params, 7),
            i32_arg(params, 8),
        ) else {
            return ERRNO_FAULT;
        };
        if length as usize > XATTR_SIZE_MAX {
            return ERRNO_2BIG;
        }
        let Ok(bytes) = memory::read_bytes(caller, value_ptr, length as usize) else {
            return ERRNO_FAULT;
        };
        (
            vec![
                json!(fd),
                json!(path),
                json!(name),
                Value::Null,
                json!(flags),
                json!(follow != 0),
            ],
            3,
            bytes,
            "process.path_setxattr_at",
        )
    } else {
        let Ok(name) = bounded_name(caller, params, 1, 2) else {
            return ERRNO_RANGE;
        };
        let (Ok(value_ptr), Ok(length), Ok(flags)) =
            (i32_arg(params, 3), i32_arg(params, 4), i32_arg(params, 5))
        else {
            return ERRNO_FAULT;
        };
        if length as usize > XATTR_SIZE_MAX {
            return ERRNO_2BIG;
        }
        let Ok(bytes) = memory::read_bytes(caller, value_ptr, length as usize) else {
            return ERRNO_FAULT;
        };
        (
            vec![json!(fd), json!(name), Value::Null, json!(flags)],
            2,
            bytes,
            "fs.fsetxattrSync",
        )
    };
    let mut raw_args = HashMap::new();
    raw_args.insert(raw_index, raw);
    match call(caller, method, args, raw_args).await {
        Ok(_) => SUCCESS,
        Err(error) => errno(&error),
    }
}

async fn xattr_remove(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    is_path: bool,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    let (args, method) = if is_path {
        let Ok(path) = path(caller, params, 1, 2) else {
            return ERRNO_FAULT;
        };
        let Ok(name) = bounded_name(caller, params, 3, 4) else {
            return ERRNO_RANGE;
        };
        let Ok(follow) = i32_arg(params, 5) else {
            return ERRNO_INVAL;
        };
        (
            vec![json!(fd), json!(path), json!(name), json!(follow != 0)],
            "process.path_removexattr_at",
        )
    } else {
        let Ok(name) = bounded_name(caller, params, 1, 2) else {
            return ERRNO_RANGE;
        };
        (vec![json!(fd), json!(name)], "fs.fremovexattrSync")
    };
    simple_call(caller, method, args).await
}

fn bounded_name(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    pointer: usize,
    length: usize,
) -> Result<String, i32> {
    let length_value = i32_arg(params, length).map_err(|_| ERRNO_FAULT)? as usize;
    if length_value > XATTR_NAME_MAX {
        return Err(ERRNO_RANGE);
    }
    path(caller, params, pointer, length)
}

fn publish_bytes(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    output: u32,
    capacity: u32,
    size_output: u32,
    bytes: Vec<u8>,
) -> i32 {
    if memory::validate_range(caller, size_output, 4).is_err()
        || memory::validate_range(caller, output, capacity as usize).is_err()
    {
        return ERRNO_FAULT;
    }
    if memory::write_u32(caller, size_output, bytes.len() as u32).is_err() {
        return ERRNO_FAULT;
    }
    if capacity == 0 {
        return SUCCESS;
    }
    if (capacity as usize) < bytes.len() {
        return ERRNO_RANGE;
    }
    commit(caller, output, &bytes)
}

async fn scalar_stat(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    results: &mut [Val],
    is_path: bool,
    field: &str,
    i64_result: bool,
    failure: u64,
) -> wasmtime::Result<bool> {
    let Ok(fd) = i32_arg(params, 0) else {
        set_scalar(results, i64_result, failure)?;
        return Ok(true);
    };
    let (method, args) = if is_path {
        let Ok(path) = path(caller, params, 1, 2) else {
            set_scalar(results, i64_result, failure)?;
            return Ok(true);
        };
        let follow = i32_arg(params, 3).unwrap_or(0) != 0;
        (
            "process.path_stat_at",
            vec![json!(fd), json!(path), json!(follow)],
        )
    } else {
        ("process.fd_filestat", vec![json!(fd)])
    };
    let value = match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => json_reply(reply)
            .ok()
            .and_then(|value| value.get(field).and_then(value_u64))
            .unwrap_or(failure),
        Err(_) => failure,
    };
    set_scalar(results, i64_result, value)?;
    Ok(true)
}

fn set_scalar(results: &mut [Val], i64_result: bool, value: u64) -> wasmtime::Result<()> {
    match (i64_result, results) {
        (true, [slot]) => {
            *slot = Val::I64(value as i64);
            Ok(())
        }
        (false, [slot]) => {
            *slot = Val::I32(value as i32);
            Ok(())
        }
        _ => Err(wasmtime::format_err!(
            "invalid filesystem scalar result shape"
        )),
    }
}

async fn resource_u32(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    output: u32,
    reply: crate::backend::HostCallReply,
    rollback_method: &str,
) -> i32 {
    let Ok(value) = json_reply(reply) else {
        return ERRNO_IO;
    };
    let Some(fd) = value_u64(&value).and_then(|value| u32::try_from(value).ok()) else {
        return ERRNO_IO;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        if let Err(error) = call(caller, rollback_method, vec![json!(fd)], HashMap::new()).await {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_RESOURCE_ROLLBACK: method={rollback_method} resource={fd} code={}",
                error.code
            );
        }
        return ERRNO_FAULT;
    }
    memory::write_u32(caller, output, fd).map_or(ERRNO_FAULT, |_| SUCCESS)
}
