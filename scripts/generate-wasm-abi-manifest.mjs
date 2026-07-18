#!/usr/bin/env node

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const outputPath = resolve(root, 'crates/execution/assets/agentos-wasm-abi.json');
const registryOutputPath = resolve(root, 'crates/execution/src/abi/generated.rs');
const preview1WitxPath = resolve(
  root,
  'crates/execution/abi/wasi_snapshot_preview1/wasi_snapshot_preview1.witx',
);
const preview1TypesPath = resolve(
  root,
  'crates/execution/abi/wasi_snapshot_preview1/typenames.witx',
);

const definitions = [];

function define(module, name, signature, status = 'canonical') {
  const [paramsText, resultsText = ''] = signature.split('->').map((part) => part.trim());
  definitions.push({
    module,
    name,
    params: paramsText === '' ? [] : paramsText.split(/\s+/),
    results: resultsText === '' ? [] : resultsText.split(/\s+/),
    status,
  });
}

function defineMany(module, entries) {
  for (const [name, signature, status] of entries) {
    define(module, name, signature, status);
  }
}

const preview1Selection = [
  ['args_get'],
  ['args_sizes_get'],
  ['clock_res_get'],
  ['clock_time_get'],
  ['environ_get'],
  ['environ_sizes_get'],
  ['fd_allocate', 'compatibility'],
  ['fd_close'],
  ['fd_datasync'],
  ['fd_fdstat_get'],
  ['fd_fdstat_set_flags'],
  ['fd_filestat_get'],
  ['fd_filestat_set_size'],
  ['fd_filestat_set_times', 'compatibility'],
  ['fd_pread'],
  ['fd_prestat_dir_name'],
  ['fd_prestat_get'],
  ['fd_pwrite'],
  ['fd_read'],
  ['fd_readdir'],
  ['fd_renumber', 'compatibility'],
  ['fd_seek'],
  ['fd_sync'],
  ['fd_tell'],
  ['fd_write'],
  ['path_create_directory'],
  ['path_filestat_get'],
  ['path_filestat_set_times', 'compatibility'],
  ['path_link'],
  ['path_open'],
  ['path_readlink'],
  ['path_remove_directory'],
  ['path_rename'],
  ['path_symlink'],
  ['path_unlink_file'],
  ['poll_oneoff'],
  ['proc_exit'],
  ['random_get'],
  ['sched_yield'],
  ['sock_shutdown', 'compatibility'],
];

const loweredPreview1 = JSON.parse(
  execFileSync(
    'cargo',
    [
      'run',
      '--quiet',
      '-p',
      'agentos-wasm-abi-generator',
      '--',
      preview1WitxPath,
    ],
    { cwd: root, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 },
  ),
);
if (loweredPreview1.module !== 'wasi_snapshot_preview1') {
  throw new Error(`unexpected pinned WITX module ${loweredPreview1.module}`);
}
const loweredPreview1Imports = new Map(
  loweredPreview1.imports.map((entry) => [entry.name, entry]),
);
for (const [name, status = 'canonical'] of preview1Selection) {
  const entry = loweredPreview1Imports.get(name);
  if (entry == null) {
    throw new Error(`pinned Preview1 WITX is missing selected import ${name}`);
  }
  definitions.push({
    module: 'wasi_snapshot_preview1',
    name,
    params: entry.params,
    results: entry.results,
    status,
  });
}

defineMany('host_fs', [
  ['open_tmpfile', 'i32 i32 i32 i32 i32 i32 -> i32'],
  ['fd_link', 'i32 i32 i32 i32 -> i32'],
  ['remount', 'i32 i32 i32 i32 -> i32'],
  ['path_mknod', 'i32 i32 i32 i32 i64 -> i32'],
  ['path_renameat2', 'i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['path_statfs', 'i32 i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['fd_fiemap', 'i32 i32 i32 i32 i32 -> i32'],
  ['fd_punch_hole', 'i32 i64 i64 -> i32'],
  ['fd_zero_range', 'i32 i64 i64 i32 -> i32'],
  ['fd_insert_range', 'i32 i64 i64 -> i32'],
  ['fd_collapse_range', 'i32 i64 i64 -> i32'],
  ['set_open_mode', 'i32 -> i32', 'compatibility'],
  ['set_open_direct', 'i32 -> i32', 'compatibility'],
  ['path_owner', 'i32 i32 i32 i32 i32 i32 -> i32'],
  ['path_mode', 'i32 i32 i32 i32 -> i32'],
  ['path_size', 'i32 i32 i32 i32 -> i64'],
  ['path_blocks', 'i32 i32 i32 i32 -> i64'],
  ['path_rdev', 'i32 i32 i32 i32 -> i64'],
  ['fd_owner', 'i32 i32 i32 -> i32'],
  ['fd_mode', 'i32 -> i32'],
  ['fd_size', 'i32 -> i64'],
  ['fd_blocks', 'i32 -> i64'],
  ['path_access', 'i32 i32 i32 i32 i32 -> i32'],
  ['path_chown', 'i32 i32 i32 i32 i32 i32 -> i32', 'compatibility'],
  ['fd_chown', 'i32 i32 i32 -> i32', 'compatibility'],
  ['chown', 'i32 i32 i32 i32 i32 i32 -> i32'],
  ['fchown', 'i32 i32 i32 -> i32'],
  ['chmod', 'i32 i32 i32 i32 -> i32'],
  ['fchmod', 'i32 i32 -> i32'],
  ['path_getxattr', 'i32 i32 i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['path_listxattr', 'i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['path_setxattr', 'i32 i32 i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['path_removexattr', 'i32 i32 i32 i32 i32 i32 -> i32'],
  ['fd_getxattr', 'i32 i32 i32 i32 i32 i32 -> i32'],
  ['fd_listxattr', 'i32 i32 i32 i32 -> i32'],
  ['fd_setxattr', 'i32 i32 i32 i32 i32 i32 -> i32'],
  ['fd_removexattr', 'i32 i32 i32 -> i32'],
  ['ftruncate', 'i32 i64 -> i32', 'compatibility'],
]);

defineMany('host_net', [
  ['net_socket', 'i32 i32 i32 i32 -> i32'],
  ['net_set_nonblock', 'i32 i32 -> i32'],
  ['net_connect', 'i32 i32 i32 -> i32'],
  ['net_getaddrinfo', 'i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['net_dns_query_rr_v1', 'i32 i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['net_bind', 'i32 i32 i32 -> i32'],
  ['net_listen', 'i32 i32 -> i32'],
  ['net_accept', 'i32 i32 i32 i32 -> i32'],
  ['net_validate_socket', 'i32 -> i32', 'compatibility'],
  ['net_validate_accept', 'i32 -> i32', 'compatibility'],
  ['net_getsockname', 'i32 i32 i32 -> i32'],
  ['net_getpeername', 'i32 i32 i32 -> i32'],
  ['net_send', 'i32 i32 i32 i32 i32 -> i32'],
  ['net_recv', 'i32 i32 i32 i32 i32 -> i32'],
  ['net_sendto', 'i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['net_recvfrom', 'i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['net_setsockopt', 'i32 i32 i32 i32 i32 -> i32'],
  ['net_getsockopt', 'i32 i32 i32 i32 i32 -> i32'],
  ['net_poll', 'i32 i32 i32 i32 -> i32'],
  ['net_close', 'i32 -> i32', 'compatibility'],
  ['net_tls_connect', 'i32 i32 i32 -> i32'],
]);

defineMany('host_process', [
  ['proc_spawn', 'i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 -> i32', 'compatibility'],
  ['proc_spawn_v2', 'i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 -> i32', 'compatibility'],
  ['proc_spawn_v3', 'i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 -> i32', 'compatibility'],
  ['proc_spawn_v4', 'i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['proc_exec', 'i32 i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['proc_fexec', 'i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['proc_waitpid', 'i32 i32 i32 i32 -> i32', 'compatibility'],
  ['proc_waitpid_v2', 'i32 i32 i32 i32 i32 i32 -> i32', 'compatibility'],
  ['proc_waitpid_v3', 'i32 i32 i32 i32 -> i32'],
  ['proc_kill', 'i32 i32 -> i32'],
  ['proc_getpid', 'i32 -> i32'],
  ['proc_getppid', 'i32 -> i32'],
  ['proc_getrlimit', 'i32 i32 i32 -> i32'],
  ['proc_setrlimit', 'i32 i64 i64 -> i32'],
  ['proc_umask', 'i32 i32 -> i32'],
  ['umask', 'i32 i32 i32 -> i32', 'compatibility'],
  ['proc_itimer_real', 'i32 i64 i64 i32 i32 -> i32'],
  ['proc_getpgid', 'i32 i32 -> i32'],
  ['proc_setpgid', 'i32 i32 -> i32'],
  ['fd_pipe', 'i32 i32 -> i32'],
  ['fd_dup', 'i32 i32 -> i32'],
  ['fd_dup2', 'i32 i32 -> i32'],
  ['fd_dup_min', 'i32 i32 i32 -> i32'],
  ['fd_getfd', 'i32 i32 -> i32'],
  ['fd_setfd', 'i32 i32 -> i32'],
  ['fd_flock', 'i32 i32 -> i32'],
  ['fd_record_lock', 'i32 i32 i32 i64 i64 i32 i32 i32 i32 -> i32'],
  ['proc_closefrom', 'i32 -> i32'],
  ['fd_socketpair', 'i32 i32 i32 i32 i32 -> i32'],
  ['fd_sendmsg_rights', 'i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['fd_recvmsg_rights', 'i32 i32 i32 i32 i32 i32 i32 i32 i32 -> i32'],
  ['sleep_ms', 'i32 -> i32', 'compatibility'],
  ['pty_open', 'i32 i32 -> i32'],
  ['proc_sigaction', 'i32 i32 i32 i32 i32 -> i32'],
  ['proc_signal_mask_v2', 'i32 i32 i32 i32 i32 -> i32'],
  ['proc_ppoll_v1', 'i32 i32 i64 i64 i32 i32 i32 i32 -> i32'],
]);

defineMany('host_tty', [
  ['read', 'i32 i32 i32 -> i32', 'compatibility'],
  ['isatty', 'i32 -> i32', 'compatibility'],
  ['get_size', 'i32 i32 i32 -> i32', 'compatibility'],
  ['set_size', 'i32 i32 i32 -> i32'],
  ['get_attr', 'i32 i32 i32 -> i32'],
  ['set_attr', 'i32 i32 i32 -> i32'],
  ['get_pgrp', 'i32 i32 -> i32'],
  ['set_pgrp', 'i32 i32 -> i32'],
  ['get_sid', 'i32 i32 -> i32'],
  ['set_raw_mode', 'i32 -> i32', 'compatibility'],
]);

defineMany('host_user', [
  ['getuid', 'i32 -> i32'],
  ['getgid', 'i32 -> i32'],
  ['geteuid', 'i32 -> i32'],
  ['getegid', 'i32 -> i32'],
  ['getresuid', 'i32 i32 i32 -> i32'],
  ['getresgid', 'i32 i32 i32 -> i32'],
  ['setuid', 'i32 -> i32'],
  ['seteuid', 'i32 -> i32'],
  ['setreuid', 'i32 i32 -> i32'],
  ['setresuid', 'i32 i32 i32 -> i32'],
  ['setgid', 'i32 -> i32'],
  ['setegid', 'i32 -> i32'],
  ['setregid', 'i32 i32 -> i32'],
  ['setresgid', 'i32 i32 i32 -> i32'],
  ['getgroups', 'i32 i32 i32 -> i32'],
  ['setgroups', 'i32 i32 -> i32'],
  ['getpwuid', 'i32 i32 i32 i32 -> i32'],
  ['getpwnam', 'i32 i32 i32 i32 i32 -> i32'],
  ['getpwent', 'i32 i32 i32 i32 -> i32'],
  ['getgrgid', 'i32 i32 i32 i32 -> i32'],
  ['getgrnam', 'i32 i32 i32 i32 i32 -> i32'],
  ['getgrent', 'i32 i32 i32 i32 -> i32'],
  ['isatty', 'i32 i32 -> i32', 'compatibility'],
]);

defineMany('host_system', [
  ['get_identity', 'i32 i32 i32 -> i32'],
]);

definitions.sort((a, b) => `${a.module}\0${a.name}`.localeCompare(`${b.module}\0${b.name}`));

const allPermissionTiers = ['isolated', 'read-only', 'read-write', 'full'];
const moduleAliases = {
  wasi_unstable: 'wasi_snapshot_preview1',
};
const modulePolicy = {
  wasi_snapshot_preview1: allPermissionTiers,
  wasi_unstable: allPermissionTiers,
  host_fs: allPermissionTiers,
  host_user: allPermissionTiers,
  host_tty: allPermissionTiers,
  host_system: allPermissionTiers,
  host_net: ['full'],
  host_process: ['full'],
};
const importPolicyOverrides = {
  'host_process.fd_dup_min': ['read-only', 'read-write', 'full'],
  'host_process.fd_flock': ['read-only', 'read-write', 'full'],
  'host_process.fd_getfd': ['read-only', 'read-write', 'full'],
  'host_process.fd_record_lock': ['read-only', 'read-write', 'full'],
  'host_process.fd_setfd': ['read-only', 'read-write', 'full'],
  'host_process.proc_getrlimit': ['read-only', 'read-write', 'full'],
  'host_process.proc_setrlimit': ['read-only', 'read-write', 'full'],
  'host_process.proc_umask': ['read-only', 'read-write', 'full'],
  'host_process.umask': ['read-only', 'read-write', 'full'],
};

function importKey(module, name) {
  return `${module}.${name}`;
}

function importKeys(module, names) {
  return names.map((name) => importKey(module, name));
}

function pascalCase(value) {
  return value
    .split(/[^A-Za-z0-9]+/u)
    .filter(Boolean)
    .map((part) => `${part[0].toUpperCase()}${part.slice(1)}`)
    .join('');
}

function coreValueRun(values, empty) {
  if (values.length === 0) return empty;
  if (values.every((value) => value === values[0])) {
    const value = values[0].toUpperCase();
    return values.length === 1 ? value : `${value}x${values.length}`;
  }
  return values.map((value) => value.toUpperCase()).join('');
}

function coreSignatureId(params, results) {
  return `${coreValueRun(params, 'NoParams')}To${coreValueRun(results, 'NoResults')}`;
}

function groupMap(groups, defaultId) {
  const values = new Map();
  for (const [id, keys] of groups) {
    for (const key of keys) {
      if (values.has(key)) {
        throw new Error(`semantic binding ${key} appears in multiple groups`);
      }
      values.set(key, id);
    }
  }
  return (definition) => values.get(importKey(definition.module, definition.name)) ?? defaultId(definition);
}

const handlerId = groupMap([
  ['ProcessArguments', importKeys('wasi_snapshot_preview1', ['args_get', 'args_sizes_get'])],
  ['ProcessEnvironment', importKeys('wasi_snapshot_preview1', ['environ_get', 'environ_sizes_get'])],
  ['ClockSnapshot', importKeys('wasi_snapshot_preview1', ['clock_res_get', 'clock_time_get'])],
  ['DescriptorClose', [
    importKey('wasi_snapshot_preview1', 'fd_close'),
    importKey('host_net', 'net_close'),
  ]],
  ['DescriptorSync', importKeys('wasi_snapshot_preview1', ['fd_datasync', 'fd_sync'])],
  ['DescriptorStatusFlags', [
    importKey('wasi_snapshot_preview1', 'fd_fdstat_get'),
    importKey('wasi_snapshot_preview1', 'fd_fdstat_set_flags'),
    importKey('host_net', 'net_set_nonblock'),
  ]],
  ['DescriptorMetadata', [
    importKey('wasi_snapshot_preview1', 'fd_filestat_get'),
    ...importKeys('host_fs', ['fd_owner', 'fd_mode', 'fd_size', 'fd_blocks']),
  ]],
  ['DescriptorSetLength', [
    importKey('wasi_snapshot_preview1', 'fd_filestat_set_size'),
    importKey('host_fs', 'ftruncate'),
  ]],
  ['MetadataSetTimes', importKeys('wasi_snapshot_preview1', [
    'fd_filestat_set_times',
    'path_filestat_set_times',
  ])],
  ['DescriptorRead', importKeys('wasi_snapshot_preview1', ['fd_pread', 'fd_read'])],
  ['DescriptorWrite', importKeys('wasi_snapshot_preview1', ['fd_pwrite', 'fd_write'])],
  ['DescriptorSeek', importKeys('wasi_snapshot_preview1', ['fd_seek', 'fd_tell'])],
  ['Preopen', importKeys('wasi_snapshot_preview1', ['fd_prestat_dir_name', 'fd_prestat_get'])],
  ['ExtentRange', [
    importKey('wasi_snapshot_preview1', 'fd_allocate'),
    ...importKeys('host_fs', [
      'fd_punch_hole',
      'fd_zero_range',
      'fd_insert_range',
      'fd_collapse_range',
    ]),
  ]],
  ['PathMetadata', [
    importKey('wasi_snapshot_preview1', 'path_filestat_get'),
    ...importKeys('host_fs', ['path_owner', 'path_mode', 'path_size', 'path_blocks', 'path_rdev']),
  ]],
  ['PathRemove', importKeys('wasi_snapshot_preview1', ['path_remove_directory', 'path_unlink_file'])],
  ['PathRename', [
    importKey('wasi_snapshot_preview1', 'path_rename'),
    importKey('host_fs', 'path_renameat2'),
  ]],
  ['ProcessPoll', [
    importKey('wasi_snapshot_preview1', 'poll_oneoff'),
    importKey('host_net', 'net_poll'),
    importKey('host_process', 'proc_ppoll_v1'),
  ]],
  ['ProcessSpawn', importKeys('host_process', ['proc_spawn', 'proc_spawn_v2', 'proc_spawn_v3', 'proc_spawn_v4'])],
  ['ProcessExec', importKeys('host_process', ['proc_exec', 'proc_fexec'])],
  ['ProcessWait', importKeys('host_process', ['proc_waitpid', 'proc_waitpid_v2', 'proc_waitpid_v3'])],
  ['ProcessUmask', importKeys('host_process', ['proc_umask', 'umask'])],
  ['ProcessGroup', importKeys('host_process', ['proc_getpgid', 'proc_setpgid'])],
  ['DescriptorDuplicate', importKeys('host_process', ['fd_dup', 'fd_dup2', 'fd_dup_min'])],
  ['DescriptorFlags', importKeys('host_process', ['fd_getfd', 'fd_setfd'])],
  ['DescriptorLock', importKeys('host_process', ['fd_flock', 'fd_record_lock'])],
  ['DescriptorRights', importKeys('host_process', ['fd_sendmsg_rights', 'fd_recvmsg_rights'])],
  ['NetworkValidate', importKeys('host_net', ['net_validate_socket', 'net_validate_accept'])],
  ['NetworkAddress', importKeys('host_net', ['net_getsockname', 'net_getpeername'])],
  ['NetworkSend', importKeys('host_net', ['net_send', 'net_sendto'])],
  ['NetworkReceive', importKeys('host_net', ['net_recv', 'net_recvfrom'])],
  ['NetworkOption', importKeys('host_net', ['net_setsockopt', 'net_getsockopt'])],
  ['MetadataOwnership', importKeys('host_fs', ['path_chown', 'fd_chown', 'chown', 'fchown'])],
  ['MetadataMode', importKeys('host_fs', ['chmod', 'fchmod'])],
  ['PathXattr', importKeys('host_fs', [
    'path_getxattr',
    'path_listxattr',
    'path_setxattr',
    'path_removexattr',
  ])],
  ['DescriptorXattr', importKeys('host_fs', [
    'fd_getxattr',
    'fd_listxattr',
    'fd_setxattr',
    'fd_removexattr',
  ])],
  ['IdentitySnapshot', importKeys('host_user', [
    'getuid',
    'getgid',
    'geteuid',
    'getegid',
    'getresuid',
    'getresgid',
  ])],
  ['IdentityCredentials', importKeys('host_user', [
    'setuid',
    'seteuid',
    'setreuid',
    'setresuid',
    'setgid',
    'setegid',
    'setregid',
    'setresgid',
  ])],
  ['AccountPassword', importKeys('host_user', ['getpwuid', 'getpwnam', 'getpwent'])],
  ['AccountGroup', importKeys('host_user', ['getgrgid', 'getgrnam', 'getgrent'])],
  ['TerminalIsatty', [importKey('host_user', 'isatty'), importKey('host_tty', 'isatty')]],
  ['TerminalSize', importKeys('host_tty', ['get_size', 'set_size'])],
  ['TerminalAttributes', importKeys('host_tty', ['get_attr', 'set_attr'])],
  ['TerminalProcessGroup', importKeys('host_tty', ['get_pgrp', 'set_pgrp'])],
], (definition) => pascalCase(`${definition.module}_${definition.name}`));

const decodeId = groupMap([
  ['Fd', [
    importKey('wasi_snapshot_preview1', 'fd_close'),
    importKey('wasi_snapshot_preview1', 'fd_datasync'),
    importKey('wasi_snapshot_preview1', 'fd_sync'),
    importKey('host_net', 'net_close'),
    importKey('host_net', 'net_validate_socket'),
    importKey('host_net', 'net_validate_accept'),
    importKey('host_tty', 'isatty'),
  ]],
  ['FdSetLength', [
    importKey('wasi_snapshot_preview1', 'fd_filestat_set_size'),
    importKey('host_fs', 'ftruncate'),
  ]],
  ['PathOwnership', importKeys('host_fs', ['path_chown', 'chown'])],
  ['DescriptorOwnership', importKeys('host_fs', ['fd_chown', 'fchown'])],
  ['NetworkAddressOutput', importKeys('host_net', ['net_getsockname', 'net_getpeername'])],
  ['IdentityScalarOutput', importKeys('host_user', ['getuid', 'getgid', 'geteuid', 'getegid'])],
  ['IdentityTripleOutput', importKeys('host_user', ['getresuid', 'getresgid'])],
  ['IdentitySetOne', importKeys('host_user', ['setuid', 'seteuid', 'setgid', 'setegid'])],
  ['IdentitySetTwo', importKeys('host_user', ['setreuid', 'setregid'])],
  ['IdentitySetThree', importKeys('host_user', ['setresuid', 'setresgid'])],
  ['AccountById', importKeys('host_user', ['getpwuid', 'getgrgid'])],
  ['AccountByName', importKeys('host_user', ['getpwnam', 'getgrnam'])],
  ['AccountByIndex', importKeys('host_user', ['getpwent', 'getgrent'])],
  ['TerminalU32Output', importKeys('host_tty', ['get_pgrp', 'get_sid'])],
], (definition) => pascalCase(`${definition.module}_${definition.name}`));

const prevalidateOutputKeys = new Set([
  ...importKeys('wasi_snapshot_preview1', [
    'args_get',
    'args_sizes_get',
    'clock_res_get',
    'clock_time_get',
    'environ_get',
    'environ_sizes_get',
    'fd_fdstat_get',
    'fd_filestat_get',
    'fd_pread',
    'fd_prestat_dir_name',
    'fd_prestat_get',
    'fd_pwrite',
    'fd_read',
    'fd_readdir',
    'fd_seek',
    'fd_tell',
    'fd_write',
    'path_filestat_get',
    'path_open',
    'path_readlink',
    'poll_oneoff',
    'random_get',
  ]),
  ...importKeys('host_fs', [
    'open_tmpfile',
    'path_statfs',
    'fd_fiemap',
    'path_owner',
    'fd_owner',
    'path_getxattr',
    'path_listxattr',
    'fd_getxattr',
    'fd_listxattr',
  ]),
  ...importKeys('host_net', [
    'net_socket',
    'net_getaddrinfo',
    'net_dns_query_rr_v1',
    'net_accept',
    'net_getsockname',
    'net_getpeername',
    'net_send',
    'net_recv',
    'net_sendto',
    'net_recvfrom',
    'net_getsockopt',
    'net_poll',
  ]),
  ...importKeys('host_process', [
    'proc_spawn',
    'proc_spawn_v2',
    'proc_spawn_v3',
    'proc_spawn_v4',
    'proc_waitpid',
    'proc_waitpid_v2',
    'proc_waitpid_v3',
    'proc_getpid',
    'proc_getppid',
    'proc_getrlimit',
    'proc_umask',
    'umask',
    'proc_itimer_real',
    'proc_getpgid',
    'fd_pipe',
    'fd_dup',
    'fd_dup_min',
    'fd_getfd',
    'fd_record_lock',
    'fd_socketpair',
    'fd_sendmsg_rights',
    'fd_recvmsg_rights',
    'pty_open',
    'proc_signal_mask_v2',
    'proc_ppoll_v1',
  ]),
  ...importKeys('host_tty', ['read', 'get_size', 'get_attr', 'get_pgrp', 'get_sid']),
  ...importKeys('host_user', [
    'getuid',
    'getgid',
    'geteuid',
    'getegid',
    'getresuid',
    'getresgid',
    'getgroups',
    'getpwuid',
    'getpwnam',
    'getpwent',
    'getgrgid',
    'getgrnam',
    'getgrent',
    'isatty',
  ]),
  importKey('host_system', 'get_identity'),
]);

const transactionalKeys = new Set([
  ...importKeys('wasi_snapshot_preview1', [
    'fd_renumber',
    'fd_read',
    'fd_write',
    'fd_pwrite',
    'fd_seek',
    'path_open',
    'proc_exit',
    'random_get',
  ]),
  importKey('host_fs', 'open_tmpfile'),
  ...importKeys('host_net', ['net_socket', 'net_accept', 'net_send', 'net_recv', 'net_sendto', 'net_recvfrom']),
  ...importKeys('host_process', [
    'proc_closefrom',
    'proc_exec',
    'proc_fexec',
    'proc_spawn',
    'proc_spawn_v2',
    'proc_spawn_v3',
    'proc_spawn_v4',
    'proc_waitpid',
    'proc_waitpid_v2',
    'proc_waitpid_v3',
    'proc_umask',
    'umask',
    'proc_itimer_real',
    'fd_pipe',
    'fd_dup',
    'fd_dup_min',
    'fd_record_lock',
    'fd_socketpair',
    'fd_sendmsg_rights',
    'fd_recvmsg_rights',
    'pty_open',
    'proc_signal_mask_v2',
    'proc_ppoll_v1',
  ]),
  importKey('host_tty', 'read'),
]);

const waitKeys = new Set([
  ...importKeys('wasi_snapshot_preview1', ['fd_read', 'fd_write', 'path_open', 'poll_oneoff']),
  ...importKeys('host_net', [
    'net_connect',
    'net_getaddrinfo',
    'net_dns_query_rr_v1',
    'net_bind',
    'net_accept',
    'net_send',
    'net_recv',
    'net_sendto',
    'net_recvfrom',
    'net_poll',
    'net_close',
    'net_tls_connect',
  ]),
  ...importKeys('host_process', [
    'proc_waitpid',
    'proc_waitpid_v2',
    'proc_waitpid_v3',
    'fd_flock',
    'fd_record_lock',
    'fd_sendmsg_rights',
    'fd_recvmsg_rights',
    'sleep_ms',
    'proc_ppoll_v1',
  ]),
  importKey('host_tty', 'read'),
]);

const restartableKeys = new Set([
  ...importKeys('wasi_snapshot_preview1', ['fd_read', 'fd_write', 'path_open']),
  ...importKeys('host_net', ['net_accept', 'net_send', 'net_recv', 'net_sendto', 'net_recvfrom']),
  ...importKeys('host_process', [
    'proc_waitpid',
    'proc_waitpid_v2',
    'proc_waitpid_v3',
    'fd_flock',
    'fd_record_lock',
    'fd_sendmsg_rights',
    'fd_recvmsg_rights',
  ]),
  importKey('host_tty', 'read'),
]);

const bootstrapKeys = new Set([
  ...importKeys('wasi_snapshot_preview1', [
    'args_get',
    'args_sizes_get',
    'environ_get',
    'environ_sizes_get',
    'fd_prestat_dir_name',
    'fd_prestat_get',
  ]),
]);
const localKeys = new Set([
  importKey('wasi_snapshot_preview1', 'sched_yield'),
  ...importKeys('host_fs', ['set_open_mode', 'set_open_direct']),
]);
const terminalKeys = new Set([
  importKey('wasi_snapshot_preview1', 'proc_exit'),
  ...importKeys('host_process', ['proc_exec', 'proc_fexec']),
]);
const scalarI32Keys = new Set([
  ...importKeys('host_fs', ['fd_mode', 'path_mode']),
  ...importKeys('host_tty', ['read', 'isatty']),
]);
const scalarI64Keys = new Set([
  ...importKeys('host_fs', ['fd_size', 'fd_blocks', 'path_size', 'path_blocks', 'path_rdev']),
]);
const scalarI64ZeroOnErrorKeys = new Set([
  importKey('host_fs', 'path_rdev'),
]);

function returnKind(key, definition) {
  if (definition.results.length === 0) return 'Void';
  if (scalarI32Keys.has(key)) return 'ScalarI32';
  if (scalarI64Keys.has(key)) return 'ScalarI64';
  return 'WasiErrno';
}

function executionClass(key) {
  if (bootstrapKeys.has(key)) return 'Bootstrap';
  if (terminalKeys.has(key)) return 'Terminal';
  if (localKeys.has(key)) return 'Local';
  if (waitKeys.has(key)) return 'Wait';
  return 'Host';
}

const encodeOverrides = groupMap([
  ['U64Output', importKeys('wasi_snapshot_preview1', ['clock_res_get', 'clock_time_get'])],
  ['DescriptorReadOutput', importKeys('wasi_snapshot_preview1', ['fd_pread', 'fd_read'])],
  ['DescriptorWriteOutput', importKeys('wasi_snapshot_preview1', ['fd_pwrite', 'fd_write'])],
  ['DescriptorOffsetOutput', importKeys('wasi_snapshot_preview1', ['fd_seek', 'fd_tell'])],
  ['ProcessIdOutput', importKeys('host_process', ['proc_spawn', 'proc_spawn_v2', 'proc_spawn_v3', 'proc_spawn_v4'])],
  ['NetworkAddressOutput', importKeys('host_net', ['net_getsockname', 'net_getpeername'])],
  ['IdentityScalarOutput', importKeys('host_user', ['getuid', 'getgid', 'geteuid', 'getegid'])],
  ['IdentityTripleOutput', importKeys('host_user', ['getresuid', 'getresgid'])],
  ['AccountRecordOutput', importKeys('host_user', [
    'getpwuid',
    'getpwnam',
    'getpwent',
    'getgrgid',
    'getgrnam',
    'getgrent',
  ])],
  ['TerminalU32Output', importKeys('host_tty', ['get_pgrp', 'get_sid'])],
], (definition) => {
  const key = importKey(definition.module, definition.name);
  const kind = returnKind(key, definition);
  if (!prevalidateOutputKeys.has(key)) {
    if (kind === 'ScalarI32') return 'ScalarI32ZeroOnError';
    if (kind === 'ScalarI64') {
      return scalarI64ZeroOnErrorKeys.has(key)
        ? 'ScalarI64ZeroOnError'
        : 'ScalarI64MaxOnError';
    }
    return kind;
  }
  return `${pascalCase(`${definition.module}_${definition.name}`)}Output`;
});

const signatureMap = new Map();
for (const definition of definitions) {
  const shape = `${definition.params.join(',')}->${definition.results.join(',')}`;
  const id = coreSignatureId(definition.params, definition.results);
  const previous = signatureMap.get(shape);
  if (previous != null && previous.id !== id) {
    throw new Error(`core signature ${shape} has conflicting ids ${previous.id} and ${id}`);
  }
  signatureMap.set(shape, { id, params: definition.params, results: definition.results });
}
const coreSignatures = [...signatureMap.values()].sort((a, b) => a.id.localeCompare(b.id));
if (new Set(coreSignatures.map((signature) => signature.id)).size !== coreSignatures.length) {
  throw new Error('generated core signature ids are not unique');
}

const definitionKeys = new Set(definitions.map((definition) => importKey(definition.module, definition.name)));
for (const keys of [
  prevalidateOutputKeys,
  transactionalKeys,
  waitKeys,
  restartableKeys,
  bootstrapKeys,
  localKeys,
  terminalKeys,
  scalarI32Keys,
  scalarI64Keys,
  scalarI64ZeroOnErrorKeys,
]) {
  for (const key of keys) {
    if (!definitionKeys.has(key)) throw new Error(`semantic metadata references unknown import ${key}`);
  }
}

const enrichedImports = definitions.map((definition) => {
  const key = importKey(definition.module, definition.name);
  const shape = `${definition.params.join(',')}->${definition.results.join(',')}`;
  const tiers = importPolicyOverrides[key] ?? modulePolicy[definition.module];
  if (tiers == null) throw new Error(`missing permission policy for ${key}`);
  return {
    id: pascalCase(`${definition.module}_${definition.name}`),
    ...definition,
    coreSignature: signatureMap.get(shape).id,
    binding: {
      handler: handlerId(definition),
      decode: decodeId(definition),
      encode: encodeOverrides(definition),
      returnKind: returnKind(key, definition),
      executionClass: executionClass(key),
      restartability: restartableKeys.has(key) ? 'SignalRestartable' : 'Never',
      transactional: transactionalKeys.has(key),
      prevalidateOutputs: prevalidateOutputKeys.has(key),
      permissionTiers: tiers,
    },
  };
});
const bindings = Object.fromEntries(enrichedImports.map((definition) => {
  const key = importKey(definition.module, definition.name);
  return [key, {
    id: definition.id,
    status: definition.status,
    coreSignature: definition.coreSignature,
    ...definition.binding,
  }];
}));

const manifest = {
  schemaVersion: 2,
  abiVersion: 'agentos-wasm-host-v1',
  source: {
    preview1Module: 'wasi_snapshot_preview1',
    preview1CompatibilityAlias: 'wasi_unstable',
    preview1WitxCommit: 'd4d3df3072b65ce43cb01c1add72b402d69a79d1',
    preview1Witx: [
      {
        path: 'crates/execution/abi/wasi_snapshot_preview1/typenames.witx',
        sha256: createHash('sha256').update(readFileSync(preview1TypesPath)).digest('hex'),
      },
      {
        path: 'crates/execution/abi/wasi_snapshot_preview1/wasi_snapshot_preview1.witx',
        sha256: createHash('sha256').update(readFileSync(preview1WitxPath)).digest('hex'),
      },
    ],
    preview1Generator: 'agentos-wasm-abi-generator@0.0.1 (witx=0.9.1)',
    wasiLibcCommit: '574b88da481569b65a237cb80daf9a2d5aeaf82d',
    customAbiInventory: 'docs/design/wasmtime-phase-0.md',
  },
  moduleAliases,
  representation: {
    byteOrder: 'little',
    pointerBits: 32,
    sizeBits: 32,
    layouts: loweredPreview1.layouts,
  },
  modulePolicy,
  importPolicyOverrides,
  coreSignatures,
  bindings,
  imports: definitions,
};

const output = `${JSON.stringify(manifest, null, 2)}\n`;
const rawRegistryOutput = execFileSync(
  'cargo',
  ['run', '--quiet', '-p', 'agentos-wasm-abi-generator', '--', '--render-registry'],
  { cwd: root, encoding: 'utf8', input: output, maxBuffer: 32 * 1024 * 1024 },
);
const registryOutput = execFileSync(
  'rustfmt',
  ['--edition', '2021', '--emit', 'stdout'],
  { cwd: root, encoding: 'utf8', input: rawRegistryOutput, maxBuffer: 32 * 1024 * 1024 },
);
if (process.argv.includes('--write')) {
  writeFileSync(outputPath, output);
  writeFileSync(registryOutputPath, registryOutput);
  process.stdout.write(`wrote ${outputPath}\nwrote ${registryOutputPath}\n`);
} else {
  let currentManifest;
  let currentRegistry;
  try {
    currentManifest = readFileSync(outputPath, 'utf8');
  } catch {
    process.stderr.write(`missing generated ABI manifest: ${outputPath}\n`);
    process.exit(1);
  }
  try {
    currentRegistry = readFileSync(registryOutputPath, 'utf8');
  } catch {
    process.stderr.write(`missing generated ABI Rust registry: ${registryOutputPath}\n`);
    process.exit(1);
  }
  if (currentManifest !== output || currentRegistry !== registryOutput) {
    process.stderr.write('generated WASM ABI manifest is stale; run node scripts/generate-wasm-abi-manifest.mjs --write\n');
    process.exit(1);
  }
  process.stdout.write(
    `WASM ABI manifest and Rust registry are current (${definitions.length} functions, ${coreSignatures.length} signatures)\n`,
  );
}
