//! AgentOS process, descriptor-control, and signal-registration ABI codecs.
//!
//! Process and descriptor state remains sidecar/kernel-owned. The adapter
//! snapshots live fds only while constructing one spawn request and never
//! retains a child table, signal mask, or fd projection in the Store.

use super::preview1::{
    call, commit, errno, i64_arg, json_reply, reply_bytes, simple_call, value_u64, ERRNO_2BIG,
    ERRNO_FAULT, ERRNO_INVAL, ERRNO_IO, SUCCESS,
};
use super::{i32_arg, set_i32_result};
use crate::abi::{AbiBinding, ImportId};
use crate::backend::HostCallReply;
use crate::host::ExecutableImageSource;
use crate::wasm::wasmtime::{
    lifecycle, memory, module,
    store::{PendingExecReplacement, WasmtimeStoreState},
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use wasmtime::{Caller, Extern, Val, ValType};

const ERRNO_BADF: i32 = 8;
const ERRNO_NOENT: i32 = 44;
const ERRNO_NOEXEC: i32 = 45;
const ERRNO_NOTSUP: i32 = 58;
const ERRNO_PERM: i32 = 63;
const ERRNO_SRCH: i32 = 71;
const MAX_FDS: usize = 1 << 20;
const MAX_RIGHTS: usize = 253;
const SUPPORTED_SPAWN_FLAGS: u32 = 0xff;

pub async fn dispatch(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<bool> {
    use ImportId::*;
    let status = match abi.id {
        HostProcessProcSpawn => spawn(caller, params, SpawnVersion::Legacy).await,
        HostProcessProcSpawnV2 => spawn(caller, params, SpawnVersion::V2).await,
        HostProcessProcSpawnV3 => spawn(caller, params, SpawnVersion::V3).await,
        HostProcessProcSpawnV4 => spawn(caller, params, SpawnVersion::V4).await,
        HostProcessProcExec => exec(caller, params, false).await,
        HostProcessProcFexec => exec(caller, params, true).await,
        HostProcessProcWaitpid => wait(caller, params, WaitVersion::Legacy).await,
        HostProcessProcWaitpidV2 => wait(caller, params, WaitVersion::V2).await,
        HostProcessProcWaitpidV3 => wait(caller, params, WaitVersion::V3).await,
        HostProcessProcKill => kill(caller, params).await,
        HostProcessProcGetpid => local_pid(caller, params, false),
        HostProcessProcGetppid => local_pid(caller, params, true),
        HostProcessProcGetrlimit => getrlimit(caller, params).await,
        HostProcessProcSetrlimit => setrlimit(caller, params).await,
        HostProcessProcUmask => umask(caller, params, true).await,
        HostProcessUmask => umask(caller, params, false).await,
        HostProcessProcItimerReal => itimer(caller, params).await,
        HostProcessProcGetpgid => getpgid(caller, params).await,
        HostProcessProcSetpgid => setpgid(caller, params).await,
        HostProcessFdPipe => pair(caller, params, PairKind::Pipe).await,
        HostProcessFdSocketpair => pair(caller, params, PairKind::Socket).await,
        HostProcessPtyOpen => pair(caller, params, PairKind::Pty).await,
        HostProcessFdDup => duplicate(caller, params, DuplicateKind::Any).await,
        HostProcessFdDupMin => duplicate(caller, params, DuplicateKind::Minimum).await,
        HostProcessFdDup2 => duplicate_to(caller, params).await,
        HostProcessFdGetfd => descriptor_flags(caller, params, false).await,
        HostProcessFdSetfd => descriptor_flags(caller, params, true).await,
        HostProcessFdFlock => flock(caller, params).await,
        HostProcessFdRecordLock => record_lock(caller, params).await,
        HostProcessProcClosefrom => closefrom(caller, params).await,
        HostProcessFdSendmsgRights => send_rights(caller, params).await,
        HostProcessFdRecvmsgRights => receive_rights(caller, params).await,
        HostProcessSleepMs => sleep(caller, params).await,
        HostProcessProcSigaction => sigaction(caller, params).await,
        HostProcessProcSignalMaskV2 => signal_mask(caller, params).await,
        HostProcessProcPpollV1 => ppoll(caller, params).await,
        _ => return Ok(false),
    };
    if caller.data().exec_replaced {
        return Err(wasmtime::format_err!("agentos:exec-replaced"));
    }
    set_i32_result(results, status)?;
    Ok(true)
}

#[derive(Clone, Copy)]
enum SpawnVersion {
    Legacy,
    V2,
    V3,
    V4,
}

struct SpawnInput {
    command: String,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: Option<String>,
    actions: Vec<Value>,
    attr_flags: u32,
    exact_path: bool,
    search_path: Option<String>,
    sched_policy: Option<i32>,
    sched_priority: Option<i32>,
    pgroup: Option<i32>,
    signal_defaults: Vec<u32>,
    signal_mask: Vec<u32>,
    output: u32,
}

async fn spawn(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    version: SpawnVersion,
) -> i32 {
    let output_index = match version {
        SpawnVersion::Legacy => 9,
        SpawnVersion::V2 => 11,
        SpawnVersion::V3 => 16,
        SpawnVersion::V4 => 20,
    };
    let Ok(output) = i32_arg(params, output_index) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    if matches!(version, SpawnVersion::V3 | SpawnVersion::V4) {
        let Ok(action_bytes) = arg(params, 7)
            .map(usize::try_from)
            .and_then(|value| value.map_err(|_| ERRNO_2BIG))
        else {
            return ERRNO_2BIG;
        };
        let limit = caller.data().max_spawn_file_action_bytes;
        if action_bytes > limit {
            let message = format!(
                "[agentos] posix_spawn file-action payload is {action_bytes} bytes, exceeding limits.process.maxSpawnFileActionBytes ({limit}); raise limits.process.maxSpawnFileActionBytes if needed\n"
            );
            if caller
                .data()
                .host
                .publish_stderr(message.into_bytes())
                .await
                .is_err()
            {
                return ERRNO_IO;
            }
            return ERRNO_2BIG;
        }
    }
    let mut input = match decode_spawn(caller, params, version, output) {
        Ok(input) => input,
        Err(error) => return error,
    };
    if input.command.is_empty() {
        return ERRNO_NOENT;
    }
    let snapshot = match fd_snapshot(caller).await {
        Ok(value) => value,
        Err(error) => return error,
    };
    let live_guest_fds = snapshot
        .iter()
        .filter_map(|entry| {
            entry
                .get("fd")
                .and_then(value_u64)
                .and_then(|value| u32::try_from(value).ok())
        })
        .collect::<Vec<_>>();
    for action in &mut input.actions {
        if action.get("command").and_then(value_u64) != Some(6) {
            continue;
        }
        let Some(minimum) = action
            .get("guestFd")
            .and_then(Value::as_i64)
            .and_then(|value| u32::try_from(value).ok())
        else {
            return ERRNO_BADF;
        };
        action["closeFromGuestFds"] = Value::Array(
            live_guest_fds
                .iter()
                .copied()
                .filter(|fd| *fd >= minimum)
                .map(Value::from)
                .collect(),
        );
    }
    let (mappings, host_net) = spawn_fd_state(&snapshot);
    let argv0 = input
        .argv
        .first()
        .cloned()
        .unwrap_or_else(|| input.command.clone());
    let request = json!({
        "command": input.command,
        "args": input.argv.into_iter().skip(1).collect::<Vec<_>>(),
        "options": {
            "argv0": argv0,
            "cwd": input.cwd,
            "env": input.env,
            "internalBootstrapEnv": {},
            "spawnAttrFlags": input.attr_flags,
            "spawnExactPath": input.exact_path,
            "spawnSearchPath": input.search_path,
            "spawnSchedPolicy": input.sched_policy,
            "spawnSchedPriority": input.sched_priority,
            "spawnPgroup": input.pgroup,
            "spawnSignalDefaults": input.signal_defaults,
            "spawnSignalMask": input.signal_mask,
            "spawnFileActions": input.actions,
            "spawnFdMappings": mappings,
            "spawnHostNetFds": host_net,
            "shell": false,
            "stdio": ["inherit", "inherit", "inherit"],
        }
    });
    match call(caller, "child_process.spawn", vec![request], HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(pid) = value
                .get("pid")
                .and_then(value_u64)
                .and_then(|value| u32::try_from(value).ok())
            else {
                return ERRNO_IO;
            };
            commit(caller, input.output, &pid.to_le_bytes())
        }
        Err(error) => errno(&error),
    }
}

fn decode_spawn(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    version: SpawnVersion,
    output: u32,
) -> Result<SpawnInput, i32> {
    let (command_pointer, command_length, argv_pointer, argv_length, env_pointer, env_length) =
        match version {
            SpawnVersion::Legacy => {
                let pointer = arg(params, 0)?;
                let length = arg(params, 1)?;
                let bytes = memory::read_bytes(caller, pointer, length as usize)
                    .map_err(|_| ERRNO_FAULT)?;
                let command_length = bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or(ERRNO_FAULT)?;
                if command_length == 0 {
                    return Err(ERRNO_FAULT);
                }
                (
                    pointer,
                    command_length as u32,
                    pointer,
                    length,
                    arg(params, 2)?,
                    arg(params, 3)?,
                )
            }
            _ => (
                arg(params, 0)?,
                arg(params, 1)?,
                arg(params, 2)?,
                arg(params, 3)?,
                arg(params, 4)?,
                arg(params, 5)?,
            ),
        };
    let command = memory::read_string(caller, command_pointer, command_length as usize)
        .map_err(|_| ERRNO_FAULT)?;
    let argv = nul_strings(
        memory::read_bytes(caller, argv_pointer, argv_length as usize).map_err(|_| ERRNO_FAULT)?,
    )?;
    let env = serialized_env(
        memory::read_bytes(caller, env_pointer, env_length as usize).map_err(|_| ERRNO_FAULT)?,
    )?;
    let mut actions = Vec::new();
    let mut attr_flags = 0;
    let mut exact_path = false;
    let mut search_path = None;
    let mut sched_policy = None;
    let mut sched_priority = None;
    let mut pgroup = None;
    let mut signal_defaults = Vec::new();
    let mut signal_mask = Vec::new();
    let cwd = match version {
        SpawnVersion::Legacy => cwd(caller, params, 7, 8)?,
        SpawnVersion::V2 => {
            actions.extend(stdio_actions([
                arg(params, 6)?,
                arg(params, 7)?,
                arg(params, 8)?,
            ]));
            cwd(caller, params, 9, 10)?
        }
        SpawnVersion::V3 | SpawnVersion::V4 => {
            actions = decode_actions(
                caller,
                arg(params, 6)?,
                arg(params, 7)?,
                caller.data().max_spawn_file_action_bytes,
                caller.data().max_spawn_file_actions,
            )?;
            let value = cwd(caller, params, 8, 9)?;
            let base = if matches!(version, SpawnVersion::V4) {
                12
            } else {
                10
            };
            attr_flags = arg(params, base)?;
            if attr_flags & !SUPPORTED_SPAWN_FLAGS != 0 {
                return Err(ERRNO_NOTSUP);
            }
            signal_defaults = if attr_flags & 4 != 0 {
                signal_set(arg(params, base + 1)?, arg(params, base + 2)?)
            } else {
                Vec::new()
            };
            signal_mask = signal_set(arg(params, base + 3)?, arg(params, base + 4)?)
                .into_iter()
                .filter(|signal| !matches!(signal, 9 | 19))
                .collect();
            pgroup = Some(arg(params, base + 5)? as i32);
            if matches!(version, SpawnVersion::V4) {
                if arg(params, 10)? != 0 {
                    search_path = Some(
                        memory::read_string(caller, arg(params, 10)?, arg(params, 11)? as usize)
                            .map_err(|_| ERRNO_FAULT)?,
                    );
                } else {
                    exact_path = true;
                }
                sched_policy = Some(arg(params, 18)? as i32);
                sched_priority = Some(arg(params, 19)? as i32);
                if attr_flags & 128 != 0 && attr_flags & 2 != 0 {
                    return Err(ERRNO_PERM);
                }
                if attr_flags & (16 | 32) != 0 && sched_priority != Some(0) {
                    return Err(ERRNO_INVAL);
                }
                if attr_flags & 32 != 0 && sched_policy != Some(0) {
                    return Err(ERRNO_PERM);
                }
            }
            value
        }
    };
    if matches!(version, SpawnVersion::Legacy) {
        actions.extend(stdio_actions([
            arg(params, 4)?,
            arg(params, 5)?,
            arg(params, 6)?,
        ]));
    }
    Ok(SpawnInput {
        command,
        argv,
        env,
        cwd,
        actions,
        attr_flags,
        exact_path,
        search_path,
        sched_policy,
        sched_priority,
        pgroup,
        signal_defaults,
        signal_mask,
        output,
    })
}

fn arg(params: &[Val], index: usize) -> Result<u32, i32> {
    i32_arg(params, index).map_err(|_| ERRNO_INVAL)
}

fn cwd(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    pointer_index: usize,
    length_index: usize,
) -> Result<Option<String>, i32> {
    let pointer = arg(params, pointer_index)?;
    let length = arg(params, length_index)? as usize;
    if length == 0 {
        Ok(None)
    } else {
        memory::read_string(caller, pointer, length)
            .map(Some)
            .map_err(|_| ERRNO_FAULT)
    }
}

fn nul_strings(bytes: Vec<u8>) -> Result<Vec<String>, i32> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut values = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    if values.last().is_some_and(|value| value.is_empty()) {
        values.pop();
    }
    values
        .into_iter()
        .map(|value| String::from_utf8(value.to_vec()).map_err(|_| ERRNO_INVAL))
        .collect()
}

fn serialized_env(bytes: Vec<u8>) -> Result<BTreeMap<String, String>, i32> {
    let mut env = BTreeMap::new();
    for value in nul_strings(bytes)? {
        let Some((key, value)) = value.split_once('=') else {
            continue;
        };
        if !key.is_empty() {
            env.insert(key.to_owned(), value.to_owned());
        }
    }
    Ok(env)
}

fn stdio_actions(fds: [u32; 3]) -> Vec<Value> {
    fds.into_iter()
        .enumerate()
        .filter_map(|(target, source)| {
            if source == target as u32 {
                None
            } else if source == u32::MAX {
                Some(action(1, target as i32, -1, 0, 0, "", Vec::new()))
            } else {
                Some(action(
                    2,
                    target as i32,
                    source as i32,
                    0,
                    0,
                    "",
                    Vec::new(),
                ))
            }
        })
        .collect()
}

fn decode_actions(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    length: u32,
    max_bytes: usize,
    max_actions: usize,
) -> Result<Vec<Value>, i32> {
    let length = length as usize;
    if length > max_bytes {
        return Err(ERRNO_2BIG);
    }
    let bytes = memory::read_bytes(caller, pointer, length).map_err(|_| ERRNO_FAULT)?;
    let mut offset = 0usize;
    let mut actions = Vec::new();
    while offset < bytes.len() {
        if bytes.len() - offset < 24 || actions.len() >= max_actions {
            return Err(if actions.len() >= max_actions {
                ERRNO_2BIG
            } else {
                ERRNO_INVAL
            });
        }
        let command = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let fd = i32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let source = i32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
        let flags = i32::from_le_bytes(bytes[offset + 12..offset + 16].try_into().unwrap());
        let mode = u32::from_le_bytes(bytes[offset + 16..offset + 20].try_into().unwrap());
        let path_length =
            u32::from_le_bytes(bytes[offset + 20..offset + 24].try_into().unwrap()) as usize;
        offset += 24;
        let end = offset.checked_add(path_length).ok_or(ERRNO_INVAL)?;
        let path = String::from_utf8(bytes.get(offset..end).ok_or(ERRNO_INVAL)?.to_vec())
            .map_err(|_| ERRNO_INVAL)?;
        offset = end;
        if !matches!(command, 1..=6) {
            return Err(ERRNO_INVAL);
        }
        if fd < 0 && matches!(command, 1 | 2 | 3 | 5 | 6) {
            return Err(ERRNO_BADF);
        }
        actions.push(action(command, fd, source, flags, mode, &path, Vec::new()));
    }
    Ok(actions)
}

fn action(
    command: u32,
    fd: i32,
    source: i32,
    flags: i32,
    mode: u32,
    path: &str,
    close_from: Vec<u32>,
) -> Value {
    json!({
        "command": command,
        "guestFd": fd,
        "fd": fd,
        "guestSourceFd": source,
        "sourceFd": source,
        "oflag": flags,
        "mode": mode,
        "path": path,
        "closeFromGuestFds": close_from,
    })
}

fn signal_set(low: u32, high: u32) -> Vec<u32> {
    (1..=64)
        .filter(|signal| {
            let bit = signal - 1;
            if bit < 32 {
                low & (1 << bit) != 0
            } else {
                high & (1 << (bit - 32)) != 0
            }
        })
        .collect()
}

async fn fd_snapshot(caller: &mut Caller<'_, WasmtimeStoreState>) -> Result<Vec<Value>, i32> {
    match call(caller, "process.fd_snapshot", vec![], HashMap::new()).await {
        Ok(reply) => json_reply(reply)?.as_array().cloned().ok_or(ERRNO_IO),
        Err(error) => Err(errno(&error)),
    }
}

fn spawn_fd_state(snapshot: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let mut mappings = Vec::new();
    let mut host_net = Vec::new();
    for entry in snapshot {
        let Some(fd) = entry
            .get("fd")
            .and_then(value_u64)
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        mappings.push(json!([fd, fd]));
        if entry.get("kind").and_then(Value::as_str) == Some("socket") {
            host_net.push(json!({
                "guestFd": fd,
                "descriptionId": entry.get("descriptionId").cloned().unwrap_or(Value::Null),
                "closeOnExec": entry.get("fdFlags").and_then(value_u64).unwrap_or(0) & 1 != 0,
                "metadata": {},
            }));
        }
    }
    (mappings, host_net)
}

async fn exec(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], by_fd: bool) -> i32 {
    let (command, argv_start, env_start, close_start) = if by_fd {
        (None, 1, 3, 5)
    } else {
        let (Ok(pointer), Ok(length)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
            return ERRNO_FAULT;
        };
        let Ok(command) = memory::read_string(caller, pointer, length as usize) else {
            return ERRNO_FAULT;
        };
        if command.is_empty() {
            return ERRNO_NOENT;
        }
        (Some(command), 2, 4, 6)
    };
    let (
        Ok(argv_pointer),
        Ok(argv_length),
        Ok(env_pointer),
        Ok(env_length),
        Ok(close_pointer),
        Ok(close_count),
    ) = (
        i32_arg(params, argv_start),
        i32_arg(params, argv_start + 1),
        i32_arg(params, env_start),
        i32_arg(params, env_start + 1),
        i32_arg(params, close_start),
        i32_arg(params, close_start + 1),
    )
    else {
        return ERRNO_FAULT;
    };
    let Ok(argv) = memory::read_bytes(caller, argv_pointer, argv_length as usize)
        .map_err(|_| ERRNO_FAULT)
        .and_then(nul_strings)
    else {
        return ERRNO_FAULT;
    };
    let Ok(env) = memory::read_bytes(caller, env_pointer, env_length as usize)
        .map_err(|_| ERRNO_FAULT)
        .and_then(serialized_env)
    else {
        return ERRNO_FAULT;
    };
    if close_count as usize > MAX_FDS {
        return ERRNO_2BIG;
    }
    let Ok(close_bytes) = memory::read_bytes(caller, close_pointer, close_count as usize * 4)
    else {
        return ERRNO_FAULT;
    };
    let close_fds = close_bytes
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        .collect::<Vec<_>>();
    let executable_fd = if by_fd {
        let Ok(fd) = i32_arg(params, 0) else {
            return ERRNO_BADF;
        };
        Some(fd)
    } else {
        None
    };
    let prepared_replacement = if let Some(fd) = executable_fd {
        let host = caller.data().host.clone();
        let engine = caller.data().engine.clone();
        let maximum = caller.data().max_module_file_bytes;
        let bytes = match lifecycle::load_executable_image(
            &host,
            ExecutableImageSource::Descriptor(fd),
            maximum,
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(error) => return errno(&error),
        };
        let compiled = match module::compile_module(&engine, &bytes) {
            Ok(compiled) => compiled.module,
            Err(error) if error.code == "ERR_AGENTOS_WASM_INVALID_MODULE" => {
                return ERRNO_NOEXEC;
            }
            Err(error) => return errno(&error),
        };
        Some(compiled)
    } else {
        None
    };
    let command = command.unwrap_or_else(|| format!("/proc/self/fd/{}", executable_fd.unwrap()));
    let request = json!({
        "command": command,
        "args": argv.iter().skip(1).cloned().collect::<Vec<_>>(),
        "options": {
            "argv0": argv.first().cloned().unwrap_or_else(|| command.clone()),
            "env": env,
            "shell": false,
            "cloexecFds": close_fds,
            "localReplacement": by_fd,
            "executableFd": executable_fd,
            "internalBootstrapEnv": {},
        }
    });
    let method = if by_fd {
        "process.exec_fd_image_commit"
    } else {
        "process.exec"
    };
    match call(caller, method, vec![request], HashMap::new()).await {
        Ok(_) if by_fd => {
            caller.data_mut().pending_exec_replacement = Some(PendingExecReplacement {
                module: prepared_replacement.expect("fexec replacement was precompiled"),
                argv,
                env,
            });
            caller.data_mut().exec_replaced = true;
            ERRNO_IO
        }
        Ok(_) => ERRNO_IO,
        Err(error) if error.code == "ERR_AGENTOS_EXEC_REPLACED" => {
            caller.data_mut().exec_replaced = true;
            ERRNO_IO
        }
        Err(error) => errno(&error),
    }
}

#[derive(Clone, Copy)]
enum WaitVersion {
    Legacy,
    V2,
    V3,
}

async fn wait(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    version: WaitVersion,
) -> i32 {
    let (Ok(raw_pid), Ok(options)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    let pointers = match version {
        WaitVersion::Legacy | WaitVersion::V3 => vec![arg(params, 2), arg(params, 3)],
        WaitVersion::V2 => vec![
            arg(params, 2),
            arg(params, 3),
            arg(params, 4),
            arg(params, 5),
        ],
    };
    let pointers = match pointers.into_iter().collect::<Result<Vec<_>, _>>() {
        Ok(value) => value,
        Err(error) => return error,
    };
    if pointers
        .iter()
        .any(|pointer| memory::validate_range(caller, *pointer, 4).is_err())
    {
        return ERRNO_FAULT;
    }
    let supported = if matches!(version, WaitVersion::V3) {
        1 | 2 | 8
    } else {
        1
    };
    if options & !supported != 0 {
        return ERRNO_INVAL;
    }
    match call(
        caller,
        "process.waitpid",
        vec![json!(raw_pid as i32), json!(options)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            if value.is_null() {
                for pointer in pointers {
                    if commit(caller, pointer, &0u32.to_le_bytes()) != SUCCESS {
                        return ERRNO_FAULT;
                    }
                }
                return SUCCESS;
            }
            let field = |name: &str| {
                value
                    .get(name)
                    .and_then(value_u64)
                    .and_then(|value| u32::try_from(value).ok())
            };
            let values = match version {
                WaitVersion::Legacy => vec![field("status"), field("pid")],
                WaitVersion::V3 => vec![field("rawStatus"), field("pid")],
                WaitVersion::V2 => vec![
                    field("exitCode"),
                    field("signal"),
                    field("pid"),
                    value
                        .get("coreDumped")
                        .and_then(Value::as_bool)
                        .map(u32::from),
                ],
            };
            if values.iter().any(Option::is_none) {
                return ERRNO_IO;
            }
            for (pointer, value) in pointers.into_iter().zip(values.into_iter().flatten()) {
                if commit(caller, pointer, &value.to_le_bytes()) != SUCCESS {
                    return ERRNO_FAULT;
                }
            }
            SUCCESS
        }
        Err(error) => errno(&error),
    }
}

fn signal_name(signal: u32) -> Option<&'static str> {
    const NAMES: [&str; 32] = [
        "0",
        "SIGHUP",
        "SIGINT",
        "SIGQUIT",
        "SIGILL",
        "SIGTRAP",
        "SIGABRT",
        "SIGBUS",
        "SIGFPE",
        "SIGKILL",
        "SIGUSR1",
        "SIGSEGV",
        "SIGUSR2",
        "SIGPIPE",
        "SIGALRM",
        "SIGTERM",
        "SIGSTKFLT",
        "SIGCHLD",
        "SIGCONT",
        "SIGSTOP",
        "SIGTSTP",
        "SIGTTIN",
        "SIGTTOU",
        "SIGURG",
        "SIGXCPU",
        "SIGXFSZ",
        "SIGVTALRM",
        "SIGPROF",
        "SIGWINCH",
        "SIGIO",
        "SIGPWR",
        "SIGSYS",
    ];
    NAMES.get(signal as usize).copied()
}

async fn kill(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(pid), Ok(signal)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    let Some(name) = signal_name(signal) else {
        return ERRNO_INVAL;
    };
    simple_call(caller, "process.kill", vec![json!(pid as i32), json!(name)]).await
}

fn local_pid(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], parent: bool) -> i32 {
    let Ok(output) = i32_arg(params, 0) else {
        return ERRNO_FAULT;
    };
    let value = if parent {
        caller.data().virtual_ppid
    } else {
        caller.data().virtual_pid
    };
    commit(caller, output, &value.to_le_bytes())
}

async fn getrlimit(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(resource), Ok(soft_output), Ok(hard_output)) =
        (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
    else {
        return ERRNO_FAULT;
    };
    if resource > 9 {
        return ERRNO_INVAL;
    }
    if memory::validate_range(caller, soft_output, 8).is_err()
        || memory::validate_range(caller, hard_output, 8).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.getrlimit",
        vec![json!(resource)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let (Some(soft), Some(hard)) = (
                value.get("soft").and_then(value_u64),
                value.get("hard").and_then(value_u64),
            ) else {
                return ERRNO_IO;
            };
            if commit(caller, soft_output, &soft.to_le_bytes()) != SUCCESS {
                ERRNO_FAULT
            } else {
                commit(caller, hard_output, &hard.to_le_bytes())
            }
        }
        Err(error) => errno(&error),
    }
}

async fn setrlimit(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(resource), Ok(soft), Ok(hard)) =
        (i32_arg(params, 0), i64_arg(params, 1), i64_arg(params, 2))
    else {
        return ERRNO_INVAL;
    };
    if resource > 9 {
        return ERRNO_INVAL;
    }
    simple_call(
        caller,
        "process.setrlimit",
        vec![
            json!(resource),
            json!(soft.to_string()),
            json!(hard.to_string()),
        ],
    )
    .await
}

async fn umask(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    always_set: bool,
) -> i32 {
    let (mask, output) = if always_set {
        let (Ok(mask), Ok(output)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
            return ERRNO_FAULT;
        };
        (Some(mask & 0o777), output)
    } else {
        let (Ok(mask), Ok(set), Ok(output)) =
            (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
        else {
            return ERRNO_FAULT;
        };
        ((set != 0).then_some(mask & 0o777), output)
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    match call(
        caller,
        "process.umask",
        mask.map(|value| vec![json!(value)]).unwrap_or_default(),
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => match json_reply(reply)
            .ok()
            .as_ref()
            .and_then(value_u64)
            .and_then(|value| u32::try_from(value).ok())
        {
            Some(value) => commit(caller, output, &value.to_le_bytes()),
            None => ERRNO_IO,
        },
        Err(error) => errno(&error),
    }
}

async fn itimer(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(operation), Ok(value), Ok(interval), Ok(remaining_output), Ok(interval_output)) = (
        i32_arg(params, 0),
        i64_arg(params, 1),
        i64_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
    ) else {
        return ERRNO_FAULT;
    };
    if operation > 1 {
        return ERRNO_INVAL;
    }
    if memory::validate_range(caller, remaining_output, 8).is_err()
        || memory::validate_range(caller, interval_output, 8).is_err()
    {
        return ERRNO_FAULT;
    }
    let args = if operation == 0 {
        vec![json!(0)]
    } else {
        vec![json!(1), json!(value), json!(interval)]
    };
    match call(caller, "process.itimer_real", args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let (Some(remaining), Some(interval)) = (
                value.get("remainingUs").and_then(value_u64),
                value.get("intervalUs").and_then(value_u64),
            ) else {
                return ERRNO_IO;
            };
            if commit(caller, remaining_output, &remaining.to_le_bytes()) != SUCCESS {
                ERRNO_FAULT
            } else {
                commit(caller, interval_output, &interval.to_le_bytes())
            }
        }
        Err(error) => errno(&error),
    }
}

async fn getpgid(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(pid), Ok(output)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_FAULT;
    };
    if (pid as i32) < 0 {
        return ERRNO_SRCH;
    }
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, "process.getpgid", vec![json!(pid)], HashMap::new()).await {
        Ok(reply) => match json_reply(reply)
            .ok()
            .as_ref()
            .and_then(value_u64)
            .and_then(|value| u32::try_from(value).ok())
        {
            Some(value) => commit(caller, output, &value.to_le_bytes()),
            None => ERRNO_IO,
        },
        Err(error) => errno(&error),
    }
}

async fn setpgid(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(pid), Ok(pgid)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    if (pid as i32) < 0 || (pgid as i32) < 0 {
        return ERRNO_INVAL;
    }
    simple_call(caller, "process.setpgid", vec![json!(pid), json!(pgid)]).await
}

#[derive(Clone, Copy)]
enum PairKind {
    Pipe,
    Socket,
    Pty,
}

async fn pair(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val], kind: PairKind) -> i32 {
    let (first_output_index, second_output_index, method, args) = match kind {
        PairKind::Pipe => (0, 1, "process.fd_pipe", vec![]),
        PairKind::Pty => (0, 1, "process.pty_open", vec![]),
        PairKind::Socket => {
            let (Ok(socket_kind), Ok(nonblocking), Ok(close_on_exec)) =
                (i32_arg(params, 0), i32_arg(params, 1), i32_arg(params, 2))
            else {
                return ERRNO_INVAL;
            };
            (
                3,
                4,
                "process.fd_socketpair",
                vec![
                    json!(socket_kind),
                    json!(nonblocking != 0),
                    json!(close_on_exec != 0),
                ],
            )
        }
    };
    let (Ok(first_output), Ok(second_output)) = (
        i32_arg(params, first_output_index),
        i32_arg(params, second_output_index),
    ) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, first_output, 4).is_err()
        || memory::validate_range(caller, second_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let names = match kind {
                PairKind::Pipe => ("readFd", "writeFd"),
                PairKind::Socket => ("firstFd", "secondFd"),
                PairKind::Pty => ("masterFd", "slaveFd"),
            };
            let (Some(first), Some(second)) = (
                value
                    .get(names.0)
                    .and_then(value_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                value
                    .get(names.1)
                    .and_then(value_u64)
                    .and_then(|value| u32::try_from(value).ok()),
            ) else {
                return ERRNO_IO;
            };
            let status = if commit(caller, first_output, &first.to_le_bytes()) == SUCCESS
                && commit(caller, second_output, &second.to_le_bytes()) == SUCCESS
            {
                SUCCESS
            } else {
                ERRNO_FAULT
            };
            if status != SUCCESS {
                log_fd_rollback(caller, first, "pair first output commit").await;
                log_fd_rollback(caller, second, "pair second output commit").await;
            }
            status
        }
        Err(error) => errno(&error),
    }
}

#[derive(Clone, Copy)]
enum DuplicateKind {
    Any,
    Minimum,
}

async fn duplicate(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    kind: DuplicateKind,
) -> i32 {
    let (Ok(fd), Ok(output)) = (
        i32_arg(params, 0),
        i32_arg(
            params,
            if matches!(kind, DuplicateKind::Any) {
                1
            } else {
                2
            },
        ),
    ) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    let (method, args) = match kind {
        DuplicateKind::Any => ("process.fd_dup", vec![json!(fd)]),
        DuplicateKind::Minimum => {
            let Ok(minimum) = i32_arg(params, 1) else {
                return ERRNO_INVAL;
            };
            ("process.fd_dup_min", vec![json!(fd), json!(minimum)])
        }
    };
    match call(caller, method, args, HashMap::new()).await {
        Ok(reply) => {
            let value = match json_reply(reply) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let Some(new_fd) = value_u64(&value)
                .or_else(|| value.get("fd").and_then(value_u64))
                .and_then(|value| u32::try_from(value).ok())
            else {
                return ERRNO_IO;
            };
            if commit(caller, output, &new_fd.to_le_bytes()) == SUCCESS {
                SUCCESS
            } else {
                log_fd_rollback(caller, new_fd, "duplicate output commit").await;
                ERRNO_FAULT
            }
        }
        Err(error) => errno(&error),
    }
}

async fn duplicate_to(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(target)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_BADF;
    };
    simple_call(caller, "process.fd_dup2", vec![json!(fd), json!(target)]).await
}

async fn descriptor_flags(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    params: &[Val],
    set: bool,
) -> i32 {
    let Ok(fd) = i32_arg(params, 0) else {
        return ERRNO_BADF;
    };
    if set {
        let Ok(flags) = i32_arg(params, 1) else {
            return ERRNO_INVAL;
        };
        return simple_call(caller, "process.fd_setfd", vec![json!(fd), json!(flags)]).await;
    }
    let Ok(output) = i32_arg(params, 1) else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    match call(caller, "process.fd_getfd", vec![json!(fd)], HashMap::new()).await {
        Ok(reply) => match json_reply(reply)
            .ok()
            .as_ref()
            .and_then(value_u64)
            .and_then(|value| u32::try_from(value).ok())
        {
            Some(value) => commit(caller, output, &value.to_le_bytes()),
            None => ERRNO_IO,
        },
        Err(error) => errno(&error),
    }
}

async fn flock(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(operation)) = (i32_arg(params, 0), i32_arg(params, 1)) else {
        return ERRNO_INVAL;
    };
    simple_call(
        caller,
        "process.fd_flock",
        vec![json!(fd), json!(operation)],
    )
    .await
}

async fn record_lock(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(fd), Ok(command), Ok(kind), Ok(start), Ok(length)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i64_arg(params, 3),
        i64_arg(params, 4),
    ) else {
        return ERRNO_INVAL;
    };
    if command == 12 {
        for index in [5usize, 6, 7, 8] {
            let Ok(pointer) = (if index < 7 {
                i32_arg(params, index).map(|value| (value, 4))
            } else {
                i32_arg(params, index).map(|value| (value, 8))
            }) else {
                return ERRNO_FAULT;
            };
            if memory::validate_range(caller, pointer.0, pointer.1).is_err() {
                return ERRNO_FAULT;
            }
        }
    }
    match call(
        caller,
        "process.fd_record_lock",
        vec![
            json!(fd),
            json!(command),
            json!(kind),
            json!(start.to_string()),
            json!(length.to_string()),
        ],
        HashMap::new(),
    )
    .await
    {
        Ok(_reply) if command != 12 => SUCCESS,
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let values = [
                value.get("type").and_then(value_u64),
                value.get("pid").and_then(value_u64),
                value.get("start").and_then(value_u64),
                value.get("length").and_then(value_u64),
            ];
            if values.iter().any(Option::is_none) {
                return ERRNO_IO;
            }
            for (index, value) in values.into_iter().flatten().enumerate() {
                let pointer = i32_arg(params, index + 5).unwrap();
                let bytes = if index < 2 {
                    (value as u32).to_le_bytes().to_vec()
                } else {
                    value.to_le_bytes().to_vec()
                };
                if commit(caller, pointer, &bytes) != SUCCESS {
                    return ERRNO_FAULT;
                }
            }
            SUCCESS
        }
        Err(error) => errno(&error),
    }
}

async fn closefrom(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let Ok(minimum) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    simple_call(
        caller,
        "process.fd_closefrom",
        vec![json!(minimum), Value::Null],
    )
    .await
}

async fn send_rights(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (
        Ok(fd),
        Ok(data_pointer),
        Ok(data_length),
        Ok(rights_pointer),
        Ok(rights_count),
        Ok(flags),
        Ok(output),
    ) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
        i32_arg(params, 5),
        i32_arg(params, 6),
    )
    else {
        return ERRNO_FAULT;
    };
    if rights_count as usize > MAX_RIGHTS {
        return ERRNO_INVAL;
    }
    let Ok(bytes) = memory::read_bytes(caller, data_pointer, data_length as usize) else {
        return ERRNO_FAULT;
    };
    let Ok(rights_bytes) = memory::read_bytes(caller, rights_pointer, rights_count as usize * 4)
    else {
        return ERRNO_FAULT;
    };
    if memory::validate_range(caller, output, 4).is_err() {
        return ERRNO_FAULT;
    }
    let rights = rights_bytes
        .chunks_exact(4)
        .map(|bytes| json!(u32::from_le_bytes(bytes.try_into().unwrap())))
        .collect::<Vec<_>>();
    let mut raw = HashMap::new();
    raw.insert(1, bytes);
    match call(
        caller,
        "process.fd_sendmsg_rights",
        vec![json!(fd), Value::Null, Value::Array(rights), json!(flags)],
        raw,
    )
    .await
    {
        Ok(reply) => {
            let value = match json_reply(reply) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let Some(sent) = value_u64(&value).and_then(|value| u32::try_from(value).ok()) else {
                return ERRNO_IO;
            };
            commit(caller, output, &sent.to_le_bytes())
        }
        Err(error) => errno(&error),
    }
}

async fn receive_rights(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let values = (0..9)
        .map(|index| i32_arg(params, index).map_err(|_| ERRNO_FAULT))
        .collect::<Result<Vec<_>, _>>();
    let Ok(values) = values else {
        return ERRNO_FAULT;
    };
    let [fd, data_pointer, data_capacity, rights_pointer, rights_capacity, flags, received_output, rights_count_output, message_flags_output] =
        values.as_slice()
    else {
        return ERRNO_FAULT;
    };
    if *rights_capacity as usize > MAX_RIGHTS
        || memory::validate_range(caller, *data_pointer, *data_capacity as usize).is_err()
        || memory::validate_range(caller, *rights_pointer, *rights_capacity as usize * 4).is_err()
        || [
            *received_output,
            *rights_count_output,
            *message_flags_output,
        ]
        .into_iter()
        .any(|pointer| memory::validate_range(caller, pointer, 4).is_err())
    {
        return ERRNO_FAULT;
    }
    let close_on_exec = *flags & 0x4000_0000 != 0;
    match call(
        caller,
        "process.fd_recvmsg_rights",
        vec![
            json!(fd),
            json!(data_capacity),
            json!(rights_capacity),
            json!(close_on_exec),
            json!(*flags & 0x2 != 0),
            json!(*flags & 0x40 != 0),
            json!(*flags & 0x100 != 0),
        ],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let data = value
                .get("data")
                .cloned()
                .map(HostCallReply::Json)
                .and_then(|value| reply_bytes(value).ok())
                .unwrap_or_default();
            let Some(rights) = value.get("rights").and_then(Value::as_array) else {
                return ERRNO_IO;
            };
            let mut installed = Vec::new();
            for right in rights {
                let fd = right
                    .get("fd")
                    .and_then(value_u64)
                    .or_else(|| value_u64(right))
                    .and_then(|value| u32::try_from(value).ok());
                let Some(fd) = fd else {
                    rollback_fds(caller, &installed).await;
                    return ERRNO_IO;
                };
                installed.push(fd);
            }
            if data.len() > *data_capacity as usize || installed.len() > *rights_capacity as usize {
                rollback_fds(caller, &installed).await;
                return ERRNO_IO;
            }
            if commit(caller, *data_pointer, &data) != SUCCESS {
                rollback_fds(caller, &installed).await;
                return ERRNO_FAULT;
            }
            for (index, fd) in installed.iter().enumerate() {
                if commit(
                    caller,
                    *rights_pointer + (index * 4) as u32,
                    &fd.to_le_bytes(),
                ) != SUCCESS
                {
                    rollback_fds(caller, &installed).await;
                    return ERRNO_FAULT;
                }
            }
            let full_length = value
                .get("fullLength")
                .and_then(value_u64)
                .unwrap_or(data.len() as u64);
            let message_flags = u32::from(
                value
                    .get("payloadTruncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ) | (u32::from(
                value
                    .get("controlTruncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ) << 1)
                | (u32::try_from(full_length).unwrap_or(u32::MAX) << 2);
            let outputs = [
                (*received_output, data.len() as u32),
                (*rights_count_output, installed.len() as u32),
                (*message_flags_output, message_flags),
            ];
            for (pointer, value) in outputs {
                if commit(caller, pointer, &value.to_le_bytes()) != SUCCESS {
                    rollback_fds(caller, &installed).await;
                    return ERRNO_FAULT;
                }
            }
            SUCCESS
        }
        Err(error) => errno(&error),
    }
}

async fn rollback_fds(caller: &mut Caller<'_, WasmtimeStoreState>, fds: &[u32]) {
    for fd in fds {
        log_fd_rollback(caller, *fd, "received-rights output commit").await;
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

async fn sleep(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let Ok(milliseconds) = i32_arg(params, 0) else {
        return ERRNO_INVAL;
    };
    simple_call(caller, "process.sleep", vec![json!(milliseconds)]).await
}

fn has_signal_trampoline(caller: &mut Caller<'_, WasmtimeStoreState>) -> bool {
    let Some(Extern::Func(function)) = caller.get_export("__wasi_signal_trampoline") else {
        return false;
    };
    let ty = function.ty(&mut *caller);
    let mut params = ty.params();
    matches!(params.next(), Some(ValType::I32))
        && params.next().is_none()
        && ty.results().next().is_none()
}

async fn sigaction(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(signal), Ok(action), Ok(low), Ok(high), Ok(flags)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
    ) else {
        return ERRNO_INVAL;
    };
    if !(1..=64).contains(&signal) || action > 2 {
        return ERRNO_INVAL;
    }
    if action == 2 && !has_signal_trampoline(caller) {
        return ERRNO_NOTSUP;
    }
    let action_name = match action {
        0 => "default",
        1 => "ignore",
        _ => "user",
    };
    let mask = signal_set(low, high);
    simple_call(
        caller,
        "process.signal_state",
        vec![
            json!(signal),
            json!(action_name),
            json!(serde_json::to_string(&mask).unwrap()),
            json!(flags),
        ],
    )
    .await
}

async fn signal_mask(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (Ok(how), Ok(low), Ok(high), Ok(old_low), Ok(old_high)) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i32_arg(params, 2),
        i32_arg(params, 3),
        i32_arg(params, 4),
    ) else {
        return ERRNO_FAULT;
    };
    if how > 3 {
        return ERRNO_INVAL;
    }
    if memory::validate_range(caller, old_low, 4).is_err()
        || memory::validate_range(caller, old_high, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    let requested = signal_set(low, high)
        .into_iter()
        .filter(|signal| !matches!(signal, 9 | 19))
        .collect::<Vec<_>>();
    match call(
        caller,
        "process.signal_mask",
        vec![json!(how), json!(requested)],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(signals) = value.get("signals").and_then(Value::as_array) else {
                return ERRNO_IO;
            };
            let mut low = 0u32;
            let mut high = 0u32;
            for signal in signals.iter().filter_map(value_u64) {
                if (1..=32).contains(&signal) {
                    low |= 1 << (signal - 1);
                } else if (33..=64).contains(&signal) {
                    high |= 1 << (signal - 33);
                }
            }
            if commit(caller, old_low, &low.to_le_bytes()) != SUCCESS {
                ERRNO_FAULT
            } else {
                commit(caller, old_high, &high.to_le_bytes())
            }
        }
        Err(error) => errno(&error),
    }
}

async fn ppoll(caller: &mut Caller<'_, WasmtimeStoreState>, params: &[Val]) -> i32 {
    let (
        Ok(pointer),
        Ok(count),
        Ok(seconds),
        Ok(nanoseconds),
        Ok(low),
        Ok(high),
        Ok(has_mask),
        Ok(ready_output),
    ) = (
        i32_arg(params, 0),
        i32_arg(params, 1),
        i64_arg(params, 2),
        i64_arg(params, 3),
        i32_arg(params, 4),
        i32_arg(params, 5),
        i32_arg(params, 6),
        i32_arg(params, 7),
    )
    else {
        return ERRNO_FAULT;
    };
    let seconds = seconds as i64;
    let nanoseconds = nanoseconds as i64;
    let timeout = if seconds < 0 && nanoseconds < 0 {
        Value::Null
    } else {
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return ERRNO_INVAL;
        }
        let milliseconds = (seconds as u128)
            .saturating_mul(1000)
            .saturating_add((nanoseconds as u128).div_ceil(1_000_000))
            .min(i32::MAX as u128) as u64;
        json!(milliseconds)
    };
    let count = count as usize;
    if count > 1024 {
        return ERRNO_INVAL;
    }
    if memory::validate_range(caller, pointer, count.saturating_mul(8)).is_err()
        || memory::validate_range(caller, ready_output, 4).is_err()
    {
        return ERRNO_FAULT;
    }
    let mut entries = Vec::with_capacity(count);
    let mut fds = Vec::with_capacity(count);
    for index in 0..count {
        let base = pointer + (index * 8) as u32;
        let Ok(fd) = memory::read_u32(caller, base) else {
            return ERRNO_FAULT;
        };
        let Ok(events) = memory::read_bytes(caller, base + 4, 2) else {
            return ERRNO_FAULT;
        };
        fds.push(fd);
        entries.push(json!({"fd": fd, "events": u16::from_le_bytes([events[0], events[1]])}));
    }
    let mask = if has_mask != 0 {
        json!(signal_set(low, high)
            .into_iter()
            .filter(|signal| !matches!(signal, 9 | 19))
            .collect::<Vec<_>>())
    } else {
        Value::Null
    };
    match call(
        caller,
        "process.posix_poll",
        vec![Value::Array(entries), timeout, mask],
        HashMap::new(),
    )
    .await
    {
        Ok(reply) => {
            let Ok(value) = json_reply(reply) else {
                return ERRNO_IO;
            };
            let Some(ready_fds) = value.get("fds").and_then(Value::as_array) else {
                return ERRNO_IO;
            };
            for (index, fd) in fds.iter().copied().enumerate().take(count) {
                let revents = ready_fds
                    .iter()
                    .find(|entry| entry.get("fd").and_then(value_u64) == Some(fd as u64))
                    .and_then(|entry| entry.get("revents"))
                    .and_then(value_u64)
                    .unwrap_or(0) as u16;
                if commit(
                    caller,
                    pointer + (index * 8 + 6) as u32,
                    &revents.to_le_bytes(),
                ) != SUCCESS
                {
                    return ERRNO_FAULT;
                }
            }
            let ready = value
                .get("readyCount")
                .and_then(value_u64)
                .unwrap_or_else(|| {
                    ready_fds
                        .iter()
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
