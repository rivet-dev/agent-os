use super::*;

const DEFAULT_ALLOWED_NODE_BUILTINS: &[&str] = &[
    "assert",
    "buffer",
    "console",
    "child_process",
    "crypto",
    "dns",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "module",
    "os",
    "path",
    "perf_hooks",
    "querystring",
    "sqlite",
    "stream",
    "string_decoder",
    "timers",
    "tls",
    "tty",
    "url",
    "util",
    "zlib",
];
const EXECUTION_REQUEST_TTY_ENV: &str = "AGENTOS_EXEC_TTY";

fn resolve_execute_request(
    vm: &mut VmState,
    payload: &ExecuteRequest,
) -> Result<ResolvedChildProcessExecution, SidecarError> {
    let payload_env: BTreeMap<String, String> = payload
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(command) = payload.command.as_deref() {
        return resolve_command_execution(
            vm,
            command,
            &payload.args,
            &payload_env,
            payload.cwd.as_deref(),
            payload.wasm_permission_tier,
        );
    }

    let runtime = payload.runtime.clone().ok_or_else(|| {
        SidecarError::InvalidState(String::from("execute requires either command or runtime"))
    })?;
    let entrypoint = payload.entrypoint.clone().ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "execute requires either command or entrypoint",
        ))
    })?;
    let (guest_cwd, host_cwd, allow_host_path_overrides) =
        resolve_execution_cwds(vm, payload.cwd.as_deref());
    let mut env = vm.guest_env.clone();
    env.extend(payload_env.clone());

    let requested_host_entrypoint = resolve_host_entrypoint_within_vm_host_cwd(vm, &entrypoint);
    if requested_host_entrypoint.is_some() && !allow_host_path_overrides {
        let requested_cwd = payload.cwd.as_deref().unwrap_or(guest_cwd.as_str());
        return Err(SidecarError::InvalidState(format!(
            "execution cwd {requested_cwd} is outside sandbox root {}",
            vm.host_cwd.to_string_lossy()
        )));
    }
    let host_entrypoint_override = allow_host_path_overrides
        .then(|| resolve_host_entrypoint_within_vm_host_cwd(vm, &entrypoint))
        .flatten();

    let guest_entrypoint = host_entrypoint_override
        .as_ref()
        .map(|(guest_entrypoint, _)| guest_entrypoint.clone())
        .or_else(|| guest_entrypoint_for_specifier(&guest_cwd, &entrypoint));
    prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

    let adapter_policy = match runtime {
        GuestRuntimeKind::WebAssembly => ExecutionAdapterPolicy::KERNEL_HOST_CALL_POSIX,
        GuestRuntimeKind::JavaScript => ExecutionAdapterPolicy::DIRECT_RUNTIME,
        GuestRuntimeKind::Python => ExecutionAdapterPolicy::DIRECT_PYTHON_RUNTIME,
    };
    Ok(ResolvedChildProcessExecution {
        command: match runtime {
            GuestRuntimeKind::JavaScript => String::from(JAVASCRIPT_COMMAND),
            GuestRuntimeKind::Python => String::from(PYTHON_COMMAND),
            GuestRuntimeKind::WebAssembly => String::from(WASM_COMMAND),
        },
        process_args: std::iter::once(entrypoint.clone())
            .chain(payload.args.iter().cloned())
            .collect(),
        runtime,
        entrypoint: host_entrypoint_override
            .map(|(_, host_entrypoint)| host_entrypoint)
            .unwrap_or(entrypoint),
        execution_args: payload.args.clone(),
        env,
        guest_cwd,
        host_cwd,
        wasm_permission_tier: payload.wasm_permission_tier,
        binding_command: false,
        adapter_policy,
    })
}

fn resolve_command_execution(
    vm: &mut VmState,
    command: &str,
    args: &[String],
    extra_env: &BTreeMap<String, String>,
    cwd: Option<&str>,
    explicit_wasm_permission_tier: Option<WasmPermissionTier>,
) -> Result<ResolvedChildProcessExecution, SidecarError> {
    let (guest_cwd, host_cwd, allow_host_path_overrides) = resolve_execution_cwds(vm, cwd);
    let mut env = vm.guest_env.clone();
    env.extend(extra_env.clone());
    let args = apply_shell_cwd_prefix(command, args.to_vec(), &guest_cwd);

    if is_binding_command(vm, command) {
        let command =
            normalized_binding_command_name(command).unwrap_or_else(|| command.to_owned());
        return Ok(ResolvedChildProcessExecution {
            command: command.clone(),
            process_args: std::iter::once(command.clone())
                .chain(args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::JavaScript,
            entrypoint: command,
            execution_args: args,
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier: None,
            binding_command: true,
            adapter_policy: ExecutionAdapterPolicy::BINDING,
        });
    }

    if is_python_runtime_command(command) {
        return resolve_python_command_execution(vm, command, &args, env, guest_cwd, host_cwd);
    }

    if is_node_runtime_command(command) {
        if let Some(cli) = resolve_host_node_cli_entrypoint(command) {
            env.insert(
                String::from("AGENTOS_NODE_EVAL"),
                build_host_node_cli_eval(&cli),
            );
            prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;
            add_runtime_guest_path_mapping(&mut env, &cli.guest_root, &cli.package_root);
            add_runtime_host_access_path(
                &mut env,
                "AGENTOS_EXTRA_FS_READ_PATHS",
                &cli.package_root,
                true,
            );

            return Ok(ResolvedChildProcessExecution {
                command: String::from(JAVASCRIPT_COMMAND),
                process_args: std::iter::once(command.to_owned())
                    .chain(args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: String::from("-e"),
                execution_args: std::iter::once(cli.guest_entrypoint.clone())
                    .chain(args.iter().cloned())
                    .collect(),
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
                adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
            });
        }

        if args.is_empty() {
            env.insert(String::from("AGENTOS_NODE_EVAL"), String::new());
            prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;

            return Ok(ResolvedChildProcessExecution {
                command: String::from(JAVASCRIPT_COMMAND),
                process_args: vec![command.to_owned()],
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: String::from("-e"),
                execution_args: Vec::new(),
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
                adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
            });
        }

        if let Some((entrypoint, execution_args)) =
            resolve_special_node_cli_invocation(&args, &mut env)
        {
            prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;

            return Ok(ResolvedChildProcessExecution {
                command: String::from(JAVASCRIPT_COMMAND),
                process_args: std::iter::once(command.to_owned())
                    .chain(args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint,
                execution_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
                adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
            });
        }

        let Some(entrypoint_specifier) = args.first() else {
            return Err(SidecarError::InvalidState(format!(
                "{command} execution requires an entrypoint"
            )));
        };

        let (entrypoint, execution_args, guest_entrypoint) = {
            let requested_host_entrypoint =
                resolve_host_entrypoint_within_vm_host_cwd(vm, entrypoint_specifier);
            if requested_host_entrypoint.is_some() && !allow_host_path_overrides {
                let requested_cwd = cwd.unwrap_or(guest_cwd.as_str());
                return Err(SidecarError::InvalidState(format!(
                    "execution cwd {requested_cwd} is outside sandbox root {}",
                    vm.host_cwd.to_string_lossy()
                )));
            }
            let host_entrypoint_override = allow_host_path_overrides
                .then(|| resolve_host_entrypoint_within_vm_host_cwd(vm, entrypoint_specifier))
                .flatten();
            let guest_entrypoint = host_entrypoint_override
                .as_ref()
                .map(|(guest_entrypoint, _)| guest_entrypoint.clone())
                .or_else(|| guest_entrypoint_for_specifier(&guest_cwd, entrypoint_specifier));
            let entrypoint = host_entrypoint_override.map_or_else(
                || {
                    guest_entrypoint.as_ref().map_or_else(
                        || entrypoint_specifier.clone(),
                        |guest_entrypoint| {
                            runtime_launch_path_for_guest(vm, guest_entrypoint)
                                .to_string_lossy()
                                .into_owned()
                        },
                    )
                },
                |(_, host_entrypoint)| host_entrypoint,
            );
            (
                entrypoint,
                args.iter().skip(1).cloned().collect(),
                guest_entrypoint,
            )
        };

        prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

        return Ok(ResolvedChildProcessExecution {
            command: String::from(JAVASCRIPT_COMMAND),
            process_args: std::iter::once(command.to_owned())
                .chain(args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::JavaScript,
            entrypoint,
            execution_args,
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier: None,
            binding_command: false,
            adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
        });
    }

    if command.ends_with(".js") || command.ends_with(".mjs") || command.ends_with(".cjs") {
        let requested_host_entrypoint = resolve_host_entrypoint_within_vm_host_cwd(vm, command);
        if requested_host_entrypoint.is_some() && !allow_host_path_overrides {
            let requested_cwd = cwd.unwrap_or(guest_cwd.as_str());
            return Err(SidecarError::InvalidState(format!(
                "execution cwd {requested_cwd} is outside sandbox root {}",
                vm.host_cwd.to_string_lossy()
            )));
        }
        let host_entrypoint_override = allow_host_path_overrides
            .then(|| resolve_host_entrypoint_within_vm_host_cwd(vm, command))
            .flatten();
        let guest_entrypoint = host_entrypoint_override
            .as_ref()
            .map(|(guest_entrypoint, _)| guest_entrypoint.clone())
            .or_else(|| guest_entrypoint_for_specifier(&guest_cwd, command));
        let entrypoint = host_entrypoint_override.map_or_else(
            || {
                guest_entrypoint.as_ref().map_or_else(
                    || command.to_owned(),
                    |guest_entrypoint| {
                        runtime_launch_path_for_guest(vm, guest_entrypoint)
                            .to_string_lossy()
                            .into_owned()
                    },
                )
            },
            |(_, host_entrypoint)| host_entrypoint,
        );
        prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

        return Ok(ResolvedChildProcessExecution {
            command: String::from(JAVASCRIPT_COMMAND),
            process_args: std::iter::once(command.to_owned())
                .chain(args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::JavaScript,
            entrypoint,
            execution_args: args.to_vec(),
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier: None,
            binding_command: false,
            adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
        });
    }

    let guest_entrypoint = resolve_guest_command_entrypoint(
        vm,
        &guest_cwd,
        command,
        env.get("PATH").map(String::as_str),
    )
    .ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "command not found on native sidecar path: {command}"
        ))
    })?;
    let wasm_permission_tier = explicit_wasm_permission_tier
        .or_else(|| vm.command_permissions.get(command).copied())
        .or_else(|| {
            Path::new(&guest_entrypoint)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| vm.command_permissions.get(name).copied())
        });

    // Resolution is authoritative in the kernel VFS. The compatibility
    // engines receive only a VM-private snapshot path, populated after this
    // live lookup has completed.
    let host_entrypoint = runtime_asset_path_for_guest(vm, &guest_entrypoint);
    if let Some((javascript_guest_entrypoint, javascript_host_entrypoint)) =
        resolve_javascript_command_entrypoint(vm, &guest_entrypoint, &host_entrypoint)
    {
        prepare_guest_runtime_env(
            vm,
            &mut env,
            &guest_cwd,
            &host_cwd,
            Some(javascript_guest_entrypoint),
        )?;

        return Ok(ResolvedChildProcessExecution {
            command: command.to_owned(),
            process_args: std::iter::once(command.to_owned())
                .chain(args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::JavaScript,
            entrypoint: javascript_host_entrypoint.to_string_lossy().into_owned(),
            execution_args: args.to_vec(),
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier: None,
            binding_command: false,
            adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
        });
    }
    prepare_guest_runtime_env(
        vm,
        &mut env,
        &guest_cwd,
        &host_cwd,
        Some(guest_entrypoint.clone()),
    )?;

    Ok(ResolvedChildProcessExecution {
        command: command.to_owned(),
        process_args: std::iter::once(command.to_owned())
            .chain(args.iter().cloned())
            .collect(),
        runtime: GuestRuntimeKind::WebAssembly,
        entrypoint: host_entrypoint.to_string_lossy().into_owned(),
        execution_args: args.to_vec(),
        env,
        guest_cwd,
        host_cwd,
        wasm_permission_tier,
        binding_command: false,
        adapter_policy: ExecutionAdapterPolicy::KERNEL_HOST_CALL_POSIX,
    })
}

const MAX_JAVASCRIPT_COMMAND_REDIRECT_DEPTH: usize = 4;

pub(super) fn resolve_javascript_command_entrypoint(
    vm: &mut VmState,
    guest_entrypoint: &str,
    _host_entrypoint: &Path,
) -> Option<(String, PathBuf)> {
    let package_mount_roots = vm
        .configuration
        .mounts
        .iter()
        .filter(|mount| mount.plugin.id == "agentos_packages")
        .map(|mount| normalize_path(&mount.guest_path))
        .collect::<Vec<_>>();
    let resolved_guest_entrypoint = resolve_javascript_command_entrypoint_inner(
        &mut vm.kernel,
        guest_entrypoint,
        &package_mount_roots,
        MAX_JAVASCRIPT_COMMAND_REDIRECT_DEPTH,
    )?;
    let launch_asset = runtime_asset_path_for_guest(vm, &resolved_guest_entrypoint);
    Some((resolved_guest_entrypoint, launch_asset))
}

/// Resolve the main module filename the same way Node does by default.
///
/// npm and other package managers expose binaries as symlinks under `.bin`.
/// Node dereferences the main-module symlink unless
/// `--preserve-symlinks-main` was requested; module-relative loads and
/// `__dirname` must therefore use the target package path, not the `.bin`
/// link. Prefer the kernel's live view because guest package installation may
/// have created the symlink after the process shadow was initialized.
pub(super) fn resolve_javascript_main_entrypoint(
    vm: &VmState,
    guest_entrypoint: &str,
) -> (String, PathBuf) {
    let resolved_guest_entrypoint = vm
        .kernel
        .realpath(guest_entrypoint)
        .map(|path| normalize_path(&path))
        .unwrap_or_else(|_| normalize_path(guest_entrypoint));
    let resolved_host_entrypoint = runtime_asset_path_for_guest(vm, &resolved_guest_entrypoint);
    (resolved_guest_entrypoint, resolved_host_entrypoint)
}

fn resolve_javascript_command_entrypoint_inner(
    kernel: &mut SidecarKernel,
    guest_entrypoint: &str,
    package_mount_roots: &[String],
    redirects_remaining: usize,
) -> Option<String> {
    // Resolve and inspect the selected inode through the live kernel. Host
    // projections and previously materialized scratch files are deliberately
    // not consulted: once a kernel file is deleted or replaced, it cannot be
    // resurrected by stale engine-launch state.
    let initial_stat = kernel.lstat(guest_entrypoint).ok()?;
    if initial_stat.is_directory {
        return None;
    }
    let canonical_guest_entrypoint = normalize_path(&kernel.realpath(guest_entrypoint).ok()?);
    let canonical_stat = kernel.lstat(&canonical_guest_entrypoint).ok()?;
    if canonical_stat.is_directory || canonical_stat.is_symbolic_link {
        return None;
    }
    let script = load_executable_script_preview(kernel, &canonical_guest_entrypoint)?;
    if script.as_bytes().starts_with(b"\0asm") {
        return None;
    }
    let interpreter = parse_script_interpreter_name(&script);
    let is_package_entrypoint =
        guest_path_is_within_roots(&canonical_guest_entrypoint, package_mount_roots);

    if interpreter.is_none()
        && (is_package_entrypoint
            || is_probable_javascript_entrypoint(Path::new(&canonical_guest_entrypoint), &script))
    {
        return Some(canonical_guest_entrypoint);
    }

    let interpreter = interpreter?;
    if interpreter == "node" {
        return Some(canonical_guest_entrypoint);
    }

    if redirects_remaining > 0 && matches!(interpreter.as_str(), "sh" | "bash" | "dash") {
        if let Some(shim_target) = parse_node_shell_shim_target(&script) {
            let guest_parent = Path::new(&canonical_guest_entrypoint)
                .parent()
                .and_then(|path| path.to_str())
                .unwrap_or("/");
            let shim_guest_entrypoint = normalize_path(&format!("{guest_parent}/{shim_target}"));
            return resolve_javascript_command_entrypoint_inner(
                kernel,
                &shim_guest_entrypoint,
                package_mount_roots,
                redirects_remaining - 1,
            );
        }
    }

    // Preserve the package-driver contract for non-WASM package launchers.
    // Unlike the old extension-only decision, this fallback happens only after
    // the selected live regular file was read and ruled out as WebAssembly.
    is_package_entrypoint.then_some(canonical_guest_entrypoint)
}

fn load_executable_script_preview(kernel: &mut SidecarKernel, guest_path: &str) -> Option<String> {
    const MAX_SCRIPT_PREVIEW_BYTES: usize = 16 * 1024;
    let preview_limit = kernel
        .resource_limits()
        .max_pread_bytes
        .unwrap_or(MAX_SCRIPT_PREVIEW_BYTES)
        .min(MAX_SCRIPT_PREVIEW_BYTES);
    let bytes = kernel.pread_file(guest_path, 0, preview_limit).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn guest_path_is_within_roots(guest_path: &str, roots: &[String]) -> bool {
    let normalized = normalize_path(guest_path);
    roots
        .iter()
        .any(|root| normalized == *root || normalized.starts_with(&format!("{root}/")))
}

fn parse_script_interpreter_name(script: &str) -> Option<String> {
    let shebang = script.lines().next()?.strip_prefix("#!")?.trim();
    let mut tokens = shebang.split_whitespace();
    let command = tokens.next()?;
    let command_name = Path::new(command).file_name()?.to_str()?;
    if command_name == "env" {
        for token in tokens {
            if token.starts_with('-') {
                continue;
            }
            return Path::new(token)
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned);
        }
        return None;
    }

    Some(command_name.to_owned())
}

fn parse_node_shell_shim_target(script: &str) -> Option<String> {
    for line in script.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("exec ") {
            continue;
        }

        let mut remaining = trimmed;
        while let Some(start) = remaining.find("\"$basedir/") {
            let after_prefix = &remaining[start + "\"$basedir/".len()..];
            let end = after_prefix.find('"')?;
            let candidate = &after_prefix[..end];
            remaining = &after_prefix[end + 1..];

            if candidate.is_empty() || candidate == "node" || candidate.ends_with("/node") {
                continue;
            }

            return Some(candidate.to_owned());
        }
    }

    None
}

fn is_probable_javascript_entrypoint(path: &Path, script: &str) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if matches!(extension, "js" | "cjs" | "mjs") {
        return true;
    }

    if !path
        .components()
        .any(|component| component.as_os_str() == "node_modules")
    {
        return false;
    }

    let preview = script.trim_start_matches('\u{feff}').trim_start();
    !preview.is_empty()
        && !preview.starts_with("#!")
        && (preview.starts_with("\"use strict\"")
            || preview.starts_with("'use strict'")
            || preview.starts_with("import ")
            || preview.starts_with("export ")
            || preview.starts_with("const ")
            || preview.starts_with("let ")
            || preview.starts_with("var ")
            || preview.starts_with("Object.defineProperty(exports")
            || preview.starts_with("module.exports")
            || preview.starts_with("require("))
}

fn resolve_guest_execution_cwd(vm: &VmState, value: Option<&str>) -> String {
    value
        .map(normalize_path)
        .unwrap_or_else(|| vm.guest_cwd.clone())
}

fn resolve_execution_cwds(vm: &VmState, value: Option<&str>) -> (String, PathBuf, bool) {
    if let Some(raw_cwd) = value {
        let normalized_vm_host_cwd = normalize_host_path(&vm.host_cwd);
        let requested_host_cwd = normalize_host_path(Path::new(raw_cwd));
        if path_is_within_root(&requested_host_cwd, &normalized_vm_host_cwd) {
            let relative = requested_host_cwd
                .strip_prefix(&normalized_vm_host_cwd)
                .unwrap_or_else(|_| Path::new(""));
            let relative = relative.to_string_lossy().replace('\\', "/");
            let guest_cwd = if relative.is_empty() {
                String::from("/")
            } else {
                normalize_path(&format!("/{relative}"))
            };
            return (guest_cwd, requested_host_cwd, true);
        }
    }

    let guest_cwd = resolve_guest_execution_cwd(vm, value);
    let host_cwd = if value.is_none() {
        vm.host_cwd.clone()
    } else {
        runtime_launch_path_for_guest(vm, &guest_cwd)
    };
    (guest_cwd, host_cwd, value.is_none())
}

pub(super) fn runtime_launch_path_for_guest(vm: &VmState, guest_path: &str) -> PathBuf {
    host_mount_path_for_guest_path(vm, guest_path)
        .unwrap_or_else(|| runtime_asset_path_for_guest(vm, guest_path))
}

pub(super) fn runtime_asset_path_for_guest(vm: &VmState, guest_path: &str) -> PathBuf {
    let normalized = normalize_path(guest_path);
    let relative = normalized.trim_start_matches('/');
    if relative.is_empty() {
        return vm.runtime_scratch_root.clone();
    }
    vm.runtime_scratch_root.join(relative)
}

fn resolved_entrypoint_uses_kernel_launch_asset(
    vm: &VmState,
    resolved: &ResolvedChildProcessExecution,
    guest_entrypoint: &str,
) -> bool {
    normalize_host_path(Path::new(&resolved.entrypoint))
        == normalize_host_path(&runtime_asset_path_for_guest(vm, guest_entrypoint))
}

pub(super) fn apply_shell_cwd_prefix(
    command: &str,
    mut args: Vec<String>,
    guest_cwd: &str,
) -> Vec<String> {
    if !is_shell_command(command) {
        return args;
    }

    // Bash accepts login-shell flags between `-c` and its command text (for
    // example `bash -c -l 'echo ok'`). The compact shell shipped in agentOS
    // expects the command immediately after `-c`, so preserve Bash semantics
    // by folding the login flag into the option group before execution.
    if args.len() >= 3 && args[0] == "-c" && matches!(args[1].as_str(), "-l" | "--login") {
        args.remove(1);
        args[0] = String::from("-lc");
    }

    if guest_cwd == "/" {
        return args;
    }

    let Some(flag) = args.first() else {
        return args;
    };
    if !matches!(flag.as_str(), "-c" | "-lc") || args.len() < 2 {
        return args;
    }

    let command_text = args[1].clone();
    let quoted_cwd = shell_single_quote(guest_cwd);
    args[1] = format!("cd {quoted_cwd} && {command_text}");
    args
}

#[cfg(test)]
mod shell_argument_tests {
    use super::apply_shell_cwd_prefix;

    #[test]
    fn normalizes_bash_login_flag_after_command_option() {
        assert_eq!(
            apply_shell_cwd_prefix(
                "/bin/bash",
                vec![
                    String::from("-c"),
                    String::from("-l"),
                    String::from("echo ok")
                ],
                "/home/agentos",
            ),
            vec![
                String::from("-lc"),
                String::from("cd '/home/agentos' && echo ok"),
            ],
        );
    }

    #[test]
    fn preserves_shell_positional_arguments_after_command_text() {
        assert_eq!(
            apply_shell_cwd_prefix(
                "bash",
                vec![
                    String::from("-c"),
                    String::from("-l"),
                    String::from("printf '%s' \"$0\""),
                    String::from("argv-zero"),
                ],
                "/work",
            ),
            vec![
                String::from("-lc"),
                String::from("cd '/work' && printf '%s' \"$0\""),
                String::from("argv-zero"),
            ],
        );
    }
}

fn is_shell_command(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .trim_end_matches(".exe")
        .eq("sh")
        || Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(command)
            .trim_end_matches(".exe")
            .eq("bash")
}

fn shell_single_quote(value: &str) -> String {
    if value.is_empty() {
        return String::from("''");
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn resolve_path_like_guest_specifier(cwd: &str, specifier: &str) -> String {
    if specifier.starts_with("file://") {
        normalize_path(specifier.trim_start_matches("file://"))
    } else if specifier.starts_with("file:") {
        normalize_path(specifier.trim_start_matches("file:"))
    } else if specifier.starts_with('/') {
        normalize_path(specifier)
    } else {
        normalize_path(&format!("{cwd}/{specifier}"))
    }
}

fn guest_entrypoint_for_specifier(cwd: &str, specifier: &str) -> Option<String> {
    is_path_like_specifier(specifier).then(|| resolve_path_like_guest_specifier(cwd, specifier))
}

pub(super) fn is_node_runtime_command(command: &str) -> bool {
    matches!(command, "node" | "npm" | "npx")
        || Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "node" | "npm" | "npx"))
}

fn python_command_base_name(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

/// `python` / `python3` (and `pip` / `pip3`, which map to `python -m pip`) are
/// served by the embedded Pyodide runtime, mirroring how `node` is served by the
/// embedded V8 runtime.
pub(super) fn is_python_runtime_command(command: &str) -> bool {
    matches!(
        python_command_base_name(command),
        "python" | "python3" | "pip" | "pip3"
    )
}

/// Parse a `python` / `pip` command line into a Pyodide execution. Supports the
/// CPython program selectors `-c CODE`, `-m MODULE`, a `SCRIPT` path, `-` /
/// piped stdin programs, and a bare interpreter (interactive REPL). The chosen
/// mode plus `sys.argv` are forwarded to the runner as `AGENTOS_PYTHON_*` control
/// env, which the runner consumes and never exposes in the guest `os.environ`.
pub(super) fn resolve_python_command_execution(
    vm: &VmState,
    command: &str,
    args: &[String],
    mut env: BTreeMap<String, String>,
    guest_cwd: String,
    host_cwd: PathBuf,
) -> Result<ResolvedChildProcessExecution, SidecarError> {
    let base_name = python_command_base_name(command);
    let is_pip = matches!(base_name, "pip" | "pip3");

    let mut entrypoint = String::new();
    let mut argv: Vec<String> = Vec::new();
    let mut module: Option<String> = None;
    let mut stdin_program = false;
    let mut interactive = false;
    let mut guest_entrypoint: Option<String> = None;

    if is_pip {
        module = Some(String::from("pip"));
        argv.push(String::from("pip"));
        argv.extend(args.iter().cloned());
    } else {
        // Skip the value-less interpreter flags we can safely ignore so they do
        // not get mistaken for a script path.
        let mut idx = 0;
        while let Some(flag) = args.get(idx) {
            match flag.as_str() {
                "-B" | "-E" | "-I" | "-O" | "-OO" | "-q" | "-s" | "-S" | "-u" | "-v" | "-b"
                | "-d" | "-x" => idx += 1,
                _ => break,
            }
        }
        let rest = &args[idx..];
        match rest.first().map(String::as_str) {
            Some("-c") => {
                entrypoint = rest.get(1).cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("argument expected for the -c option"))
                })?;
                argv.push(String::from("-c"));
                argv.extend(rest.iter().skip(2).cloned());
            }
            Some("-m") => {
                let name = rest.get(1).cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("argument expected for the -m option"))
                })?;
                module = Some(name);
                argv.push(String::from("-m"));
                argv.extend(rest.iter().skip(2).cloned());
            }
            Some("-") => {
                stdin_program = true;
                argv.push(String::from("-"));
                argv.extend(rest.iter().skip(1).cloned());
            }
            Some(spec) if !spec.starts_with('-') => {
                let resolved_guest = guest_entrypoint_for_specifier(&guest_cwd, spec)
                    .unwrap_or_else(|| spec.to_string());
                entrypoint = resolved_guest.clone();
                env.insert(String::from("AGENTOS_PYTHON_FILE"), resolved_guest.clone());
                guest_entrypoint = Some(resolved_guest);
                argv.push(spec.to_string());
                argv.extend(rest.iter().skip(1).cloned());
            }
            Some(other) => {
                return Err(SidecarError::InvalidState(format!(
                    "unsupported python option: {other}"
                )));
            }
            None => {
                interactive = true;
                argv.push(String::new());
            }
        }
    }

    env.insert(
        String::from("AGENTOS_PYTHON_ARGV"),
        serde_json::to_string(&argv).unwrap_or_else(|_| String::from("[]")),
    );
    if let Some(module) = &module {
        env.insert(String::from("AGENTOS_PYTHON_MODULE"), module.clone());
    }
    if stdin_program {
        env.insert(
            String::from("AGENTOS_PYTHON_STDIN_PROGRAM"),
            String::from("1"),
        );
    }
    if interactive {
        env.insert(
            String::from("AGENTOS_PYTHON_INTERACTIVE"),
            String::from("1"),
        );
    }

    prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

    Ok(ResolvedChildProcessExecution {
        command: String::from(PYTHON_COMMAND),
        process_args: std::iter::once(command.to_owned())
            .chain(args.iter().cloned())
            .collect(),
        runtime: GuestRuntimeKind::Python,
        entrypoint,
        execution_args: args.to_vec(),
        env,
        guest_cwd,
        host_cwd,
        wasm_permission_tier: None,
        binding_command: false,
        adapter_policy: ExecutionAdapterPolicy::DIRECT_PYTHON_RUNTIME,
    })
}

pub(super) fn resolve_special_node_cli_invocation(
    args: &[String],
    env: &mut BTreeMap<String, String>,
) -> Option<(String, Vec<String>)> {
    let first = args.first()?;
    match first.as_str() {
        "-e" | "--eval" => {
            env.insert(
                String::from("AGENTOS_NODE_EVAL"),
                args.get(1).cloned().unwrap_or_default(),
            );
            Some((first.clone(), args.iter().skip(2).cloned().collect()))
        }
        "-v" | "--version" => {
            env.insert(
                String::from("AGENTOS_NODE_EVAL"),
                String::from("console.log(process.version);"),
            );
            Some((String::from("-e"), args.to_vec()))
        }
        "--test" => {
            env.insert(
                String::from("AGENTOS_NODE_EVAL"),
                String::from(
                    r#"
const fs = require("node:fs");
const path = require("node:path");
const { pathToFileURL } = require("node:url");

(async () => {
const { __agentOSRunTests } = await import("node:test");
const args = process.argv.slice(1);
const namePatternArg = args.find((arg) => arg.startsWith("--test-name-pattern="));
const namePattern = namePatternArg ? namePatternArg.slice("--test-name-pattern=".length) : undefined;
const requested = args.filter((arg) => !arg.startsWith("--"));
const root = process.cwd();
const discovered = [];
const visit = (entry) => {
  const stat = fs.lstatSync(entry);
  if (stat.isSymbolicLink()) return;
  if (stat.isDirectory()) {
    if ([".git", "bin", "node_modules"].includes(path.basename(entry))) return;
    for (const child of fs.readdirSync(entry)) visit(path.join(entry, child));
    return;
  }
  const normalized = entry.replaceAll("\\", "/");
  if (/(?:^|\/)(?:test\/.*|.*(?:\.test|-test))\.(?:js|mjs|cjs)$/u.test(normalized)) {
    discovered.push(entry);
  }
};
for (const entry of requested.length > 0 ? requested : [root]) {
  visit(path.isAbsolute(entry) ? entry : path.resolve(root, entry));
}
discovered.sort();
for (const entry of discovered) {
  await import(pathToFileURL(entry).href);
}
const summary = await __agentOSRunTests(namePattern);
if (summary.failed > 0) process.exitCode = 1;
})().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exitCode = 1;
});
"#,
                ),
            );
            Some((String::from("-e"), args.iter().skip(1).cloned().collect()))
        }
        _ => None,
    }
}

fn node_runtime_command_name(command: &str) -> Option<&str> {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())?;
    matches!(name, "node" | "npm" | "npx").then_some(name)
}

pub(super) struct ResolvedHostNodeCliEntrypoint {
    pub(super) command_name: String,
    pub(super) guest_root: String,
    pub(super) guest_entrypoint: String,
    pub(super) package_root: PathBuf,
}

const MAX_NODE_CLI_SHIM_BYTES: u64 = 16 * 1024;

fn is_npm_cli_entrypoint(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !matches!(file_name, "npm-cli.js" | "npx-cli.js") {
        return false;
    }
    let Some(package_root) = path.parent().and_then(Path::parent) else {
        return false;
    };
    package_root.join("package.json").is_file() && package_root.join("lib/npm.js").is_file()
}

fn node_cli_target_from_shim(script: &str, home: &Path, shim_directory: &Path) -> Option<PathBuf> {
    script.split_ascii_whitespace().find_map(|token| {
        let token = token.trim_matches(|ch| matches!(ch, '"' | '\'' | ';'));
        let expanded = token
            .strip_prefix("$HOME/")
            .or_else(|| token.strip_prefix("${HOME}/"))
            .map(|relative| home.join(relative))
            .or_else(|| {
                token
                    .strip_prefix("$basedir/")
                    .or_else(|| token.strip_prefix("${basedir}/"))
                    .map(|relative| shim_directory.join(relative))
            })
            .unwrap_or_else(|| PathBuf::from(token));
        let expanded = expanded.canonicalize().ok().unwrap_or(expanded);
        is_npm_cli_entrypoint(&expanded).then_some(expanded)
    })
}

fn resolve_node_cli_entrypoint(candidate: &Path) -> Option<PathBuf> {
    let direct = candidate
        .canonicalize()
        .ok()
        .unwrap_or_else(|| candidate.to_path_buf());
    if is_npm_cli_entrypoint(&direct) {
        return Some(direct);
    }

    let metadata = candidate.metadata().ok()?;
    if metadata.len() > MAX_NODE_CLI_SHIM_BYTES {
        return None;
    }
    let script = std::fs::read_to_string(candidate).ok()?;
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let shim_directory = candidate.parent()?;
    let target = node_cli_target_from_shim(&script, &home, shim_directory)?;
    let target = target.canonicalize().ok().unwrap_or(target);
    is_npm_cli_entrypoint(&target).then_some(target)
}

pub(super) fn resolve_host_node_cli_entrypoint(
    command: &str,
) -> Option<ResolvedHostNodeCliEntrypoint> {
    let command_name = node_runtime_command_name(command)?;
    if !matches!(command_name, "npm" | "npx") {
        return None;
    }

    let path = std::env::var_os("PATH")?;
    for root in std::env::split_paths(&path) {
        let candidate = root.join(command_name);
        if !candidate.is_file() {
            continue;
        }
        let Some(entrypoint) = resolve_node_cli_entrypoint(&candidate) else {
            continue;
        };
        let package_root = entrypoint.parent()?.parent()?.to_path_buf();
        let guest_root = format!("/__agentos/node-runtime/{command_name}");
        let relative_entrypoint = entrypoint.strip_prefix(&package_root).ok()?;
        let guest_entrypoint = normalize_path(&format!(
            "{guest_root}/{}",
            relative_entrypoint.to_string_lossy().replace('\\', "/")
        ));
        return Some(ResolvedHostNodeCliEntrypoint {
            command_name: command_name.to_owned(),
            guest_root,
            guest_entrypoint,
            package_root,
        });
    }

    None
}

#[cfg(test)]
mod node_cli_shim_tests {
    use super::{
        build_host_node_cli_eval, node_cli_target_from_shim, ResolvedHostNodeCliEntrypoint,
    };
    use std::{fs, path::PathBuf, time::SystemTime};

    #[test]
    fn expands_home_in_npm_shell_shim() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "agentos-node-cli-shim-{}-{nonce}",
            std::process::id()
        ));
        let package_root = home.join("node_modules/npm");
        fs::create_dir_all(package_root.join("bin")).expect("create npm bin");
        fs::create_dir_all(package_root.join("lib")).expect("create npm lib");
        fs::write(package_root.join("package.json"), "{}").expect("write package json");
        fs::write(package_root.join("lib/npm.js"), "").expect("write npm main");
        fs::write(package_root.join("bin/npm-cli.js"), "").expect("write npm cli");

        let target = node_cli_target_from_shim(
            "#!/bin/sh\nexec node \"$HOME/node_modules/npm/bin/npm-cli.js\" \"$@\"\n",
            &home,
            &home.join("bin"),
        );

        assert_eq!(target, Some(package_root.join("bin/npm-cli.js")));
        fs::remove_dir_all(home).expect("remove npm shim fixture");
    }

    #[test]
    fn expands_pnpm_basedir_in_npm_shell_shim() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "agentos-pnpm-node-cli-shim-{}-{nonce}",
            std::process::id()
        ));
        let shim_directory = root.join("node_modules/.bin");
        let package_root = root.join("node_modules/npm");
        fs::create_dir_all(&shim_directory).expect("create shim directory");
        fs::create_dir_all(package_root.join("bin")).expect("create npm bin");
        fs::create_dir_all(package_root.join("lib")).expect("create npm lib");
        fs::write(package_root.join("package.json"), "{}").expect("write package json");
        fs::write(package_root.join("lib/npm.js"), "").expect("write npm main");
        fs::write(package_root.join("bin/npm-cli.js"), "").expect("write npm cli");

        let target = node_cli_target_from_shim(
            "#!/bin/sh\nexec node \"$basedir/../npm/bin/npm-cli.js\" \"$@\"\n",
            &root,
            &shim_directory,
        );

        assert_eq!(target, Some(package_root.join("bin/npm-cli.js")));
        fs::remove_dir_all(root).expect("remove pnpm shim fixture");
    }

    #[test]
    fn npm_display_stub_filters_buffered_messages_by_configured_loglevel() {
        let source = build_host_node_cli_eval(&ResolvedHostNodeCliEntrypoint {
            command_name: String::from("npm"),
            guest_root: String::from("/opt/agentos/npm"),
            guest_entrypoint: String::from("/opt/agentos/npm/bin/npm-cli.js"),
            package_root: PathBuf::from("/opt/agentos/npm"),
        });

        assert!(source.contains("arg.startsWith('--loglevel=')"));
        assert!(source.contains("process.env.npm_config_loglevel"));
        assert!(source.contains("this._logBuffer.push([level, args])"));
        assert!(source.contains("this._shouldLog(bufferLevel)"));
        assert!(source.contains(
            "responseBody.once('resume', () => queueMicrotask(() => responseBody.end(responseBuffer)))"
        ));
        assert!(source.contains(
            "clonedBody.once('resume', () => queueMicrotask(() => clonedBody.end(clonedBuffer)))"
        ));
    }
}

pub(super) fn build_host_node_cli_eval(cli: &ResolvedHostNodeCliEntrypoint) -> String {
    let guest_npm_main = normalize_path(&format!("{}/lib/npm.js", cli.guest_root));
    let guest_npm_cli = normalize_path(&format!("{}/bin/npm-cli.js", cli.guest_root));
    let guest_package_json = normalize_path(&format!("{}/package.json", cli.guest_root));
    let guest_display_module = normalize_path(&format!("{}/lib/utils/display.js", cli.guest_root));
    let guest_log_file_module =
        normalize_path(&format!("{}/lib/utils/log-file.js", cli.guest_root));
    let debug_preamble = "const __agentOSDebugNpmCli = !!process.env.CODEX_DEBUG_NPM_CLI; const __agentOSDebugLog = (...args) => { if (__agentOSDebugNpmCli) { console.error('[agentos npm debug]', ...args); } }; const __agentOSIsProcessExitError = (error) => !!(error && typeof error === 'object' && (error._isProcessExit === true || error.name === 'ProcessExitError')); const __agentOSResolveExitCode = (code) => Number.isFinite(code) ? code : (Number.isFinite(process.exitCode) ? process.exitCode : 0); const __agentOSFinish = (code) => { process.exitCode = __agentOSResolveExitCode(code); }; if (__agentOSDebugNpmCli) { const __agentOSWrapAsyncFsMethod = (__agentOSTarget, __agentOSMethod) => { const __agentOSOriginal = __agentOSTarget[__agentOSMethod]; if (typeof __agentOSOriginal !== 'function' || __agentOSOriginal.__agentOSDebugWrapped) { return; } const __agentOSWrapped = async (...args) => { const target = args.length > 0 ? args[0] : '<none>'; __agentOSDebugLog(`fs.${__agentOSMethod}:start`, String(target)); try { const result = await __agentOSOriginal.apply(__agentOSTarget, args); __agentOSDebugLog(`fs.${__agentOSMethod}:done`, String(target)); return result; } catch (error) { __agentOSDebugLog(`fs.${__agentOSMethod}:error`, String(target), error && error.stack ? error.stack : String(error)); throw error; } }; __agentOSWrapped.__agentOSDebugWrapped = true; __agentOSTarget[__agentOSMethod] = __agentOSWrapped; }; const __agentOSWrapSyncFsMethod = (__agentOSTarget, __agentOSMethod) => { const __agentOSOriginal = __agentOSTarget[__agentOSMethod]; if (typeof __agentOSOriginal !== 'function' || __agentOSOriginal.__agentOSDebugWrapped) { return; } const __agentOSWrapped = (...args) => { const target = args.length > 0 ? args[0] : '<none>'; __agentOSDebugLog(`fs.${__agentOSMethod}:start`, String(target)); try { const result = __agentOSOriginal.apply(__agentOSTarget, args); __agentOSDebugLog(`fs.${__agentOSMethod}:done`, String(target)); return result; } catch (error) { __agentOSDebugLog(`fs.${__agentOSMethod}:error`, String(target), error && error.stack ? error.stack : String(error)); throw error; } }; __agentOSWrapped.__agentOSDebugWrapped = true; __agentOSTarget[__agentOSMethod] = __agentOSWrapped; }; const __agentOSFsPromiseModules = [require('fs/promises'), require('node:fs/promises')]; for (const __agentOSFsPromises of __agentOSFsPromiseModules) { for (const __agentOSMethod of ['access', 'lstat', 'mkdir', 'open', 'readFile', 'readdir', 'readlink', 'realpath', 'rename', 'rm', 'rmdir', 'stat', 'symlink', 'unlink', 'writeFile']) { __agentOSWrapAsyncFsMethod(__agentOSFsPromises, __agentOSMethod); } } const __agentOSFsModules = [require('fs'), require('node:fs')]; for (const __agentOSFs of __agentOSFsModules) { for (const __agentOSMethod of ['accessSync', 'existsSync', 'lstatSync', 'mkdirSync', 'openSync', 'readFileSync', 'readdirSync', 'readlinkSync', 'realpathSync', 'renameSync', 'rmSync', 'rmdirSync', 'statSync', 'symlinkSync', 'unlinkSync', 'writeFileSync']) { __agentOSWrapSyncFsMethod(__agentOSFs, __agentOSMethod); } } }";
    let display_stub = format!(
        "const __agentOSDisplayModulePath = require.resolve({display_module}); const __agentOSLogFileModulePath = require.resolve({log_file_module}); const __agentOSColorPassthrough = new Proxy((value) => value, {{ get: () => __agentOSColorPassthrough, apply: (_target, _thisArg, args) => args[0] }}); class __AgentOSNpmDisplayStub {{ constructor() {{ this.chalk = {{ noColor: __agentOSColorPassthrough, stdout: __agentOSColorPassthrough, stderr: __agentOSColorPassthrough }}; this._logPaused = true; this._logBuffer = []; this._outputBuffer = []; const levels = {{ silent: 0, error: 1, warn: 2, notice: 3, http: 4, info: 5, verbose: 6, silly: 7 }}; const loglevelIndex = process.argv.findIndex((arg) => arg === '--loglevel' || arg.startsWith('--loglevel=')); const loglevelArg = loglevelIndex < 0 ? undefined : process.argv[loglevelIndex]; const configuredLevel = loglevelArg && loglevelArg.includes('=') ? loglevelArg.slice(loglevelArg.indexOf('=') + 1) : process.argv[loglevelIndex + 1]; this._logThreshold = levels[String(configuredLevel || process.env.npm_config_loglevel || 'notice').toLowerCase()] ?? levels.notice; this._shouldLog = (level) => levels[level] === undefined || levels[level] <= this._logThreshold; this._write = (stream, values) => {{ if (!Array.isArray(values) || values.length === 0) {{ return; }} const text = values.map((value) => typeof value === 'string' ? value : String(value)).join(' '); if (text.length === 0) {{ return; }} const normalized = text.replace(/\\r\\n/g, '\\n'); if (/^\\n?> npx\\n> /u.test(normalized)) {{ return; }} stream.write(text.endsWith('\\n') ? text : `${{text}}\\n`); }}; this._inputHandler = (level, ...args) => {{ if (level !== 'read') {{ return; }} const [resolve, reject, callback] = args; Promise.resolve().then(() => callback()).then(resolve, reject); }}; this._logHandler = (level, ...args) => {{ if (level === 'resume') {{ this._logPaused = false; for (const [bufferLevel, bufferArgs] of this._logBuffer.splice(0)) {{ if (this._shouldLog(bufferLevel)) {{ this._write(process.stderr, bufferArgs); }} }} return; }} if (level === 'pause') {{ this._logPaused = true; return; }} if (!this._shouldLog(level)) {{ return; }} if (this._logPaused) {{ this._logBuffer.push([level, args]); return; }} this._write(process.stderr, args); }}; this._outputHandler = (level, ...args) => {{ if (level === 'buffer') {{ this._outputBuffer.push(['standard', args]); return; }} if (level === 'flush') {{ for (const [bufferLevel, bufferArgs] of this._outputBuffer.splice(0)) {{ this._write(bufferLevel === 'error' ? process.stderr : process.stdout, bufferArgs); }} return; }} this._write(level === 'error' ? process.stderr : process.stdout, args); }}; process.on('input', this._inputHandler); process.on('log', this._logHandler); process.on('output', this._outputHandler); }} async load() {{ process.emit('log', 'resume'); process.emit('output', 'flush'); }} off() {{ if (this._inputHandler) {{ process.off('input', this._inputHandler); }} if (this._logHandler) {{ process.off('log', this._logHandler); }} if (this._outputHandler) {{ process.off('output', this._outputHandler); }} this._logBuffer.length = 0; this._outputBuffer.length = 0; }} }} class __AgentOSNpmLogFileStub {{ constructor() {{ this.files = []; }} async load() {{ return []; }} off() {{}} }} globalThis._moduleCache[__agentOSDisplayModulePath] = {{ exports: __AgentOSNpmDisplayStub }}; globalThis._moduleCache[__agentOSLogFileModulePath] = {{ exports: __AgentOSNpmLogFileStub }};",
        display_module = serde_json::to_string(&guest_display_module)
            .unwrap_or_else(|_| format!("\"{guest_display_module}\"")),
        log_file_module = serde_json::to_string(&guest_log_file_module)
            .unwrap_or_else(|_| format!("\"{guest_log_file_module}\"")),
    );
    let registry_fetch_stub = "const { createRequire: __agentOSCreateRequire } = require('module'); const __agentOSNpmRequire = __agentOSCreateRequire(require.resolve(__AGENTOS_NPM_MAIN__)); try { const __agentOSMinipassFetchPath = __agentOSNpmRequire.resolve('minipass-fetch'); const __agentOSMinipassFetch = __agentOSNpmRequire(__agentOSMinipassFetchPath); const { FetchError: __agentOSFetchError, Headers: __agentOSFetchHeaders, Request: __agentOSFetchRequest, Response: __agentOSFetchResponse, AbortError: __agentOSAbortError } = __agentOSMinipassFetch; const { Minipass: __agentOSMinipass } = __agentOSNpmRequire('minipass'); const __agentOSCreateBinaryMinipass = () => new __agentOSMinipass({ objectMode: false, encoding: null }); const __agentOSCloneBuffer = (buffer) => Buffer.isBuffer(buffer) ? Buffer.from(buffer) : Buffer.from(buffer ?? []); const __agentOSBufferToArrayBuffer = (buffer) => { const bytes = __agentOSCloneBuffer(buffer); return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength); }; const __agentOSAttachBufferedBodyMethods = (response, responseBuffer) => { const __agentOSReadBuffer = async () => __agentOSCloneBuffer(responseBuffer); response.__agentOSBufferedBody = __agentOSCloneBuffer(responseBuffer); response.buffer = __agentOSReadBuffer; response.text = async () => (await __agentOSReadBuffer()).toString('utf8'); response.json = async () => JSON.parse(await response.text()); response.arrayBuffer = async () => __agentOSBufferToArrayBuffer(await __agentOSReadBuffer()); response.clone = () => { const clonedBody = __agentOSCreateBinaryMinipass(); const clonedBuffer = __agentOSCloneBuffer(responseBuffer); queueMicrotask(() => clonedBody.end(clonedBuffer)); const clonedResponse = new __agentOSFetchResponse(clonedBody, { url: response.url, status: response.status, statusText: response.statusText, headers: response.headers, size: response.size, timeout: response.timeout, counter: response.counter, trailer: response.trailer }); return __agentOSAttachBufferedBodyMethods(clonedResponse, clonedBuffer); }; return response; }; const __agentOSNormalizeHeaders = (__agentOSHeaders) => { const normalized = {}; __agentOSHeaders.forEach((value, key) => { if (normalized[key] === undefined) { normalized[key] = value; return; } if (Array.isArray(normalized[key])) { normalized[key].push(value); return; } normalized[key] = [normalized[key], value]; }); return normalized; }; const __agentOSPatchedMinipassFetch = async (input, opts = {}) => { const request = input instanceof __agentOSFetchRequest ? input : new __agentOSFetchRequest(input, opts); const __agentOSController = !request.signal && typeof AbortController === 'function' ? new AbortController() : null; const __agentOSSignal = request.signal ?? __agentOSController?.signal; let __agentOSTimer = null; if (__agentOSController && Number.isFinite(request.timeout) && request.timeout > 0) { __agentOSTimer = setTimeout(() => __agentOSController.abort(new Error(`network timeout at: ${request.url}`)), request.timeout); __agentOSTimer.unref?.(); } try { const requestHeaders = {}; request.headers.forEach((value, key) => { requestHeaders[key] = value; }); const response = await fetch(request.url, { method: request.method, headers: requestHeaders, body: request.body ?? undefined, redirect: request.redirect ?? opts.redirect ?? 'follow', signal: __agentOSSignal, ...(request.body ? { duplex: 'half' } : {}) }); const responseBody = __agentOSCreateBinaryMinipass(); const contentType = String(response.headers.get('content-type') || '').toLowerCase(); const responseBuffer = contentType.includes('json') ? Buffer.from(JSON.stringify(await response.json())) : contentType.startsWith('text/') ? Buffer.from(await response.text()) : Buffer.from(await response.arrayBuffer()); queueMicrotask(() => responseBody.end(responseBuffer)); return __agentOSAttachBufferedBodyMethods(new __agentOSFetchResponse(responseBody, { url: response.url, status: response.status, statusText: response.statusText, headers: __agentOSNormalizeHeaders(response.headers), size: request.size, timeout: request.timeout, counter: request.counter ?? opts.counter ?? 0, trailer: Promise.resolve(new __agentOSFetchHeaders()) }), responseBuffer); } catch (error) { if (error instanceof Error) { throw error; } throw new __agentOSFetchError(String(error), 'system', error); } finally { if (__agentOSTimer) { clearTimeout(__agentOSTimer); } } }; globalThis.__agentOSPatchedMinipassFetch = __agentOSPatchedMinipassFetch; __agentOSPatchedMinipassFetch.isRedirect = typeof __agentOSMinipassFetch.isRedirect === 'function' ? __agentOSMinipassFetch.isRedirect.bind(__agentOSMinipassFetch) : (code) => code === 301 || code === 302 || code === 303 || code === 307 || code === 308; __agentOSPatchedMinipassFetch.FetchError = __agentOSFetchError; __agentOSPatchedMinipassFetch.Headers = __agentOSFetchHeaders; __agentOSPatchedMinipassFetch.Request = __agentOSFetchRequest; __agentOSPatchedMinipassFetch.Response = __agentOSFetchResponse; __agentOSPatchedMinipassFetch.AbortError = __agentOSAbortError; globalThis._moduleCache[__agentOSMinipassFetchPath] = { exports: __agentOSPatchedMinipassFetch }; __agentOSDebugLog('patched-minipass-fetch', __agentOSMinipassFetchPath); const __agentOSCheckResponsePath = __agentOSNpmRequire.resolve('npm-registry-fetch/lib/check-response.js'); const __agentOSCheckResponse = __agentOSNpmRequire(__agentOSCheckResponsePath); const __agentOSEnsureResponseBodyStream = (response) => { if (!response || (response.body && typeof response.body.on === 'function')) { return response; } const body = __agentOSCreateBinaryMinipass(); const finishWithError = (error) => body.emit('error', error instanceof Error ? error : new Error(String(error))); try { if (typeof response.buffer === 'function') { Promise.resolve(response.buffer()).then((buffer) => body.end(buffer), finishWithError); } else if (Buffer.isBuffer(response.body) || typeof response.body === 'string') { body.end(response.body); } else if (response.body && typeof response.body[Symbol.asyncIterator] === 'function') { (async () => { try { for await (const chunk of response.body) { body.write(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)); } body.end(); } catch (error) { finishWithError(error); body.end(); } })(); } else { body.end(); } } catch (error) { finishWithError(error); body.end(); } return new __agentOSFetchResponse(body, response); }; globalThis._moduleCache[__agentOSCheckResponsePath] = { exports: (payload) => { const normalized = { ...payload, res: __agentOSEnsureResponseBodyStream(payload.res) }; __agentOSDebugLog('check-response-body', normalized.res && normalized.res.status, typeof (normalized.res && normalized.res.body), normalized.res && normalized.res.body && typeof normalized.res.body.on, normalized.res && normalized.res.body && normalized.res.body.constructor && normalized.res.body.constructor.name, !!(normalized.res && normalized.res.__agentOSBufferedBody), normalized.res && typeof normalized.res.json); return __agentOSCheckResponse(normalized); } }; __agentOSDebugLog('patched-check-response', __agentOSCheckResponsePath); } catch (error) { __agentOSDebugLog('patch-minipass-fetch-failed', error && error.stack ? error.stack : String(error)); } try { const __agentOSRegistryFetchPath = __agentOSNpmRequire.resolve('npm-registry-fetch'); const __agentOSRegistryFetch = __agentOSNpmRequire(__agentOSRegistryFetchPath); const __agentOSWrapRegistryFetch = (fn) => { const wrapResult = (promise) => Promise.resolve(promise).then((res) => { __agentOSDebugLog('registry-fetch-result', res && res.status, typeof (res && res.body), res && res.body && typeof res.body.on, res && res.body && res.body.constructor && res.body.constructor.name, !!(res && res.__agentOSBufferedBody), res && typeof res.json); return res; }); const wrapped = (uri, opts = {}) => wrapResult(globalThis.__agentOSPatchedMinipassFetch(uri, { method: opts.method, headers: opts.headers, body: opts.body, redirect: opts.redirect, signal: opts.signal, timeout: opts.timeout, size: opts.size, counter: opts.counter })); if (typeof fn.json === 'function') { wrapped.json = (uri, opts = {}) => wrapped(uri, opts).then((res) => res.json()); } if (fn.json && typeof fn.json.stream === 'function') { wrapped.json = wrapped.json || {}; wrapped.json.stream = (uri, path, opts = {}) => fn.json.stream(uri, path, { ...opts, agent: false }); } if (typeof fn.pickRegistry === 'function') { wrapped.pickRegistry = fn.pickRegistry.bind(fn); } if (typeof fn.getAuth === 'function') { wrapped.getAuth = fn.getAuth.bind(fn); } return wrapped; }; globalThis._moduleCache[__agentOSRegistryFetchPath] = { exports: __agentOSWrapRegistryFetch(__agentOSRegistryFetch) }; __agentOSDebugLog('patched-npm-registry-fetch', __agentOSRegistryFetchPath); } catch (error) { __agentOSDebugLog('patch-npm-registry-fetch-failed', error && error.stack ? error.stack : String(error)); }";
    // Keep npm-registry-fetch in charge of resolving relative registry paths,
    // applying auth and cache headers, and retrying requests. The transport
    // adapter above already replaces minipass-fetch; wrapping the higher-level
    // registry client as well bypasses those semantics and turns requests such
    // as "/" into invalid absolute URLs.
    let registry_fetch_stub = registry_fetch_stub.replace(
        "const wrapped = (uri, opts = {}) => wrapResult(globalThis.__agentOSPatchedMinipassFetch(uri, { method: opts.method, headers: opts.headers, body: opts.body, redirect: opts.redirect, signal: opts.signal, timeout: opts.timeout, size: opts.size, counter: opts.counter }));",
        "const wrapped = (uri, opts = {}) => wrapResult(fn(uri, { ...opts, cache: 'no-store' }));",
    );
    let registry_fetch_stub = registry_fetch_stub.replace(
        "} catch (error) { if (error instanceof Error) { throw error; } throw new __agentOSFetchError(String(error), 'system', error); } finally {",
        "} catch (error) { __agentOSDebugLog('minipass-fetch-error', error && error.stack ? error.stack : String(error)); if (error instanceof Error) { throw error; } throw new __agentOSFetchError(String(error), 'system', error); } finally {",
    );
    let registry_fetch_stub = registry_fetch_stub.replace(
        "const __agentOSPatchedMinipassFetch = async (input, opts = {}) => {",
        "const __agentOSPatchedMinipassFetch = async (input, opts = {}) => { __agentOSDebugLog('minipass-fetch-start', String(input && input.url ? input.url : input));",
    );
    let registry_fetch_stub = registry_fetch_stub.replace(
        "request.headers.forEach((value, key) => { requestHeaders[key] = value; }); const response",
        "request.headers.forEach((value, key) => { requestHeaders[key] = value; }); const npmCommand = process.argv[2]; if (['install', 'ci', 'prune'].includes(npmCommand) && /^https:\\/\\/registry\\.npmjs\\.org\\//.test(request.url) && !String(requestHeaders.accept || '').includes('application/vnd.npm.install-v1+json')) { requestHeaders.accept = 'application/vnd.npm.install-v1+json'; __agentOSDebugLog('forced-abbreviated-metadata', request.url); } const response",
    );
    let registry_fetch_stub = registry_fetch_stub.replace(
        "__agentOSDebugLog('patched-minipass-fetch', __agentOSMinipassFetchPath); const __agentOSCheckResponsePath",
        "const __agentOSMakeFetchHappenPath = __agentOSNpmRequire.resolve('make-fetch-happen'); const __agentOSSleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)); const __agentOSMakeFetchHappen = async (url, opts = {}) => { const configuredRetries = Number(opts.retry && opts.retry.retries); const retries = Number.isFinite(configuredRetries) ? Math.max(0, Math.min(5, configuredRetries)) : 2; let lastError; for (let attempt = 0; attempt <= retries; attempt++) { try { const response = await __agentOSPatchedMinipassFetch(url, opts); response.headers.set('x-fetch-attempts', String(attempt + 1)); if (![408, 420, 429].includes(response.status) && response.status < 500 || attempt === retries) { return response; } lastError = new __agentOSFetchError(`HTTP ${response.status} while fetching ${url}`, 'system'); } catch (error) { lastError = error; if (attempt === retries) { throw error; } } await __agentOSSleep(Math.min(2000, 100 * (2 ** attempt))); } throw lastError; }; __agentOSMakeFetchHappen.defaults = (defaultUrl, defaultOptions = {}, wrappedFetch = __agentOSMakeFetchHappen) => { if (typeof defaultUrl === 'object') { defaultOptions = defaultUrl; defaultUrl = null; } const defaultedFetch = (url, options = {}) => wrappedFetch(url || defaultUrl, { ...defaultOptions, ...options, headers: { ...defaultOptions.headers, ...options.headers } }); defaultedFetch.defaults = (nextUrl, nextOptions = {}) => __agentOSMakeFetchHappen.defaults(nextUrl, nextOptions, defaultedFetch); return defaultedFetch; }; __agentOSMakeFetchHappen.FetchError = __agentOSFetchError; __agentOSMakeFetchHappen.Headers = __agentOSFetchHeaders; __agentOSMakeFetchHappen.Request = __agentOSFetchRequest; __agentOSMakeFetchHappen.Response = __agentOSFetchResponse; globalThis._moduleCache[__agentOSMakeFetchHappenPath] = { exports: __agentOSMakeFetchHappen }; __agentOSDebugLog('patched-minipass-fetch', __agentOSMinipassFetchPath); __agentOSDebugLog('patched-make-fetch-happen', __agentOSMakeFetchHappenPath); const __agentOSCheckResponsePath",
    );
    let registry_fetch_stub = registry_fetch_stub.replace(
        "}); return normalized; };",
        "}); normalized['cache-control'] = 'no-store'; return normalized; };",
    );
    // The native fetch bridge buffers responses, but minipass consumers still
    // require a live stream. Do not end that stream before npm's cache pipeline
    // has attached and resumed it.
    let registry_fetch_stub = registry_fetch_stub
        .replace(
            "queueMicrotask(() => responseBody.end(responseBuffer));",
            "responseBody.once('resume', () => queueMicrotask(() => responseBody.end(responseBuffer)));",
        )
        .replace(
            "queueMicrotask(() => clonedBody.end(clonedBuffer));",
            "clonedBody.once('resume', () => queueMicrotask(() => clonedBody.end(clonedBuffer)));",
        );
    match cli.command_name.as_str() {
        "npx" => format!(
            "{debug_preamble} {display_stub} {registry_fetch_stub} process.argv[1] = require.resolve({npm_cli}); process.argv.splice(2, 0, 'exec'); __agentOSDebugLog('argv', JSON.stringify(process.argv), 'cwd', process.cwd()); (async () => {{ const pkg = require({package_json}); if (process.argv.includes('--version') || process.argv.includes('-v')) {{ __agentOSDebugLog('version-shortcut'); console.log(pkg.version); __agentOSFinish(0); return; }} const Npm = require({npm_main}); const npm = new Npm(); __agentOSDebugLog('before-load'); const loaded = await npm.load(); __agentOSDebugLog('after-load', loaded && loaded.command, JSON.stringify(loaded && loaded.args)); if (!loaded.exec) {{ __agentOSDebugLog('no-exec'); __agentOSFinish(); return; }} if (!loaded.command) {{ __agentOSDebugLog('no-command'); const {{ output }} = require('proc-log'); output.standard(npm.usage); __agentOSFinish(1); return; }} __agentOSDebugLog('before-exec', loaded.command, JSON.stringify(loaded.args)); await npm.exec(loaded.command, loaded.args); __agentOSDebugLog('after-exec', __agentOSResolveExitCode()); __agentOSFinish(); }})().catch((error) => {{ if (__agentOSIsProcessExitError(error)) {{ __agentOSDebugLog('process-exit-error', __agentOSResolveExitCode(error.code)); __agentOSFinish(error.code); return; }} console.error(error && error.stack ? error.stack : String(error)); __agentOSFinish(error && typeof error === 'object' && Number.isFinite(error.exitCode) ? error.exitCode : 1); }});",
            debug_preamble = debug_preamble,
            display_stub = display_stub,
            registry_fetch_stub = registry_fetch_stub.replace(
                "__AGENTOS_NPM_MAIN__",
                &serde_json::to_string(&guest_npm_main)
                    .unwrap_or_else(|_| format!("\"{guest_npm_main}\"")),
            ),
            npm_main = serde_json::to_string(&guest_npm_main)
                .unwrap_or_else(|_| format!("\"{guest_npm_main}\"")),
            npm_cli = serde_json::to_string(&guest_npm_cli)
                .unwrap_or_else(|_| format!("\"{guest_npm_cli}\"")),
            package_json = serde_json::to_string(&guest_package_json)
                .unwrap_or_else(|_| format!("\"{guest_package_json}\"")),
        ),
        _ => format!(
            "{debug_preamble} {display_stub} {registry_fetch_stub} __agentOSDebugLog('argv', JSON.stringify(process.argv), 'cwd', process.cwd()); (async () => {{ const pkg = require({package_json}); if (process.argv.includes('--version') || process.argv.includes('-v')) {{ __agentOSDebugLog('version-shortcut'); console.log(pkg.version); __agentOSFinish(0); return; }} const Npm = require({npm_main}); const npm = new Npm(); __agentOSDebugLog('before-load'); const loaded = await npm.load(); __agentOSDebugLog('after-load', loaded && loaded.command, JSON.stringify(loaded && loaded.args)); if (!loaded.exec) {{ __agentOSDebugLog('no-exec'); __agentOSFinish(); return; }} if (!loaded.command) {{ __agentOSDebugLog('no-command'); const {{ output }} = require('proc-log'); output.standard(npm.usage); __agentOSFinish(1); return; }} __agentOSDebugLog('before-exec', loaded.command, JSON.stringify(loaded.args)); await npm.exec(loaded.command, loaded.args); __agentOSDebugLog('after-exec', __agentOSResolveExitCode()); __agentOSFinish(); }})().catch((error) => {{ if (__agentOSIsProcessExitError(error)) {{ __agentOSDebugLog('process-exit-error', __agentOSResolveExitCode(error.code)); __agentOSFinish(error.code); return; }} console.error(error && error.stack ? error.stack : String(error)); __agentOSFinish(error && typeof error === 'object' && Number.isFinite(error.exitCode) ? error.exitCode : 1); }});",
            debug_preamble = debug_preamble,
            display_stub = display_stub,
            registry_fetch_stub = registry_fetch_stub.replace(
                "__AGENTOS_NPM_MAIN__",
                &serde_json::to_string(&guest_npm_main)
                    .unwrap_or_else(|_| format!("\"{guest_npm_main}\"")),
            ),
            npm_main = serde_json::to_string(&guest_npm_main)
                .unwrap_or_else(|_| format!("\"{guest_npm_main}\"")),
            package_json = serde_json::to_string(&guest_package_json)
                .unwrap_or_else(|_| format!("\"{guest_package_json}\"")),
        ),
    }
}

pub(super) fn rewrite_javascript_shebang_request(
    vm: &mut VmState,
    resolved: &ResolvedChildProcessExecution,
    request: &mut ProcessLaunchRequest,
) -> Result<bool, SidecarError> {
    const MAX_SHEBANG_LINE_BYTES: usize = 256;

    if !matches!(resolved.runtime, GuestRuntimeKind::WebAssembly) {
        return Ok(false);
    }
    let Some(script_path) = resolved
        .env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .map(|path| normalize_path(path))
    else {
        return Ok(false);
    };
    let is_registered_command = registered_command_name_for_path(&vm.kernel, &resolved.command)
        .is_some()
        || (!is_path_like_specifier(&resolved.command)
            && vm.kernel.commands().contains_key(&resolved.command));
    if !is_registered_command {
        let stat = vm.kernel.stat(&script_path).map_err(kernel_error)?;
        if stat.is_directory || stat.mode & 0o111 == 0 {
            return Err(SidecarError::host(
                "EACCES",
                format!("permission denied, execute '{script_path}'"),
            ));
        }
    }
    let header = vm
        .kernel
        .pread_file(&script_path, 0, MAX_SHEBANG_LINE_BYTES + 1)
        .map_err(kernel_error)?;
    let Some((command, args)) =
        parse_javascript_shebang(&script_path, &header, &resolved.execution_args)?
    else {
        return Ok(false);
    };
    request.command = command;
    request.args = args;
    request.options.shell = false;
    Ok(true)
}

fn parse_javascript_shebang(
    script_path: &str,
    header: &[u8],
    execution_args: &[String],
) -> Result<Option<(String, Vec<String>)>, SidecarError> {
    const MAX_SHEBANG_LINE_BYTES: usize = 256;

    if !header.starts_with(b"#!") {
        return Ok(None);
    }

    let line_end = match header.iter().position(|byte| *byte == b'\n') {
        Some(index) if index > MAX_SHEBANG_LINE_BYTES => {
            return Err(SidecarError::host(
                "ENOEXEC",
                format!("shebang line exceeds {MAX_SHEBANG_LINE_BYTES} bytes: {script_path}"),
            ));
        }
        Some(index) => index,
        None if header.len() > MAX_SHEBANG_LINE_BYTES => {
            return Err(SidecarError::host(
                "ENOEXEC",
                format!("shebang line exceeds {MAX_SHEBANG_LINE_BYTES} bytes: {script_path}"),
            ));
        }
        None => header.len(),
    };
    let line = header[2..line_end]
        .strip_suffix(b"\r")
        .unwrap_or(&header[2..line_end]);
    let text = std::str::from_utf8(line).map_err(|_| {
        SidecarError::host("ENOEXEC", format!("invalid shebang line: {script_path}"))
    })?;
    let text = text.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    let (interpreter, optional_arg) = text
        .find(|ch: char| ch.is_ascii_whitespace())
        .map(|index| {
            (
                &text[..index],
                text[index..].trim_matches(|ch: char| ch.is_ascii_whitespace()),
            )
        })
        .map(|(interpreter, optional_arg)| {
            (
                interpreter,
                (!optional_arg.is_empty()).then_some(optional_arg),
            )
        })
        .unwrap_or((text, None));
    if interpreter.is_empty() {
        return Err(SidecarError::host(
            "ENOEXEC",
            format!("invalid shebang line: {script_path}"),
        ));
    }
    let (command, mut interpreter_args) = if matches!(interpreter, "/usr/bin/env" | "/bin/env") {
        let optional_arg = optional_arg.ok_or_else(|| {
            SidecarError::host(
                "ENOENT",
                format!("missing interpreter after {interpreter} in shebang: {script_path}"),
            )
        })?;
        if let Some(split_string) = optional_arg
            .strip_prefix("-S")
            .filter(|rest| rest.starts_with(|ch: char| ch.is_ascii_whitespace()))
        {
            let mut words = shlex::split(split_string.trim()).ok_or_else(|| {
                SidecarError::host(
                    "ENOEXEC",
                    format!("invalid /usr/bin/env -S quoting in shebang: {script_path}"),
                )
            })?;
            if words.is_empty() {
                return Err(SidecarError::host(
                    "ENOENT",
                    format!("missing interpreter after /usr/bin/env -S in shebang: {script_path}"),
                ));
            }
            let command = words.remove(0);
            (command, words)
        } else {
            if optional_arg.starts_with('-')
                || optional_arg.chars().any(|ch| ch.is_ascii_whitespace())
            {
                return Err(SidecarError::host(
                    "ENOEXEC",
                    format!("/usr/bin/env shebang arguments require -S: {script_path}"),
                ));
            }
            (optional_arg.to_owned(), Vec::new())
        }
    } else {
        (
            interpreter.to_owned(),
            optional_arg
                .map(|arg| vec![arg.to_owned()])
                .unwrap_or_default(),
        )
    };
    interpreter_args.push(script_path.to_owned());
    interpreter_args.extend(execution_args.iter().cloned());
    Ok(Some((command, interpreter_args)))
}

#[cfg(test)]
mod javascript_shebang_tests {
    use super::parse_javascript_shebang;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn preserves_linux_optional_argument_and_crlf() {
        let parsed = parse_javascript_shebang(
            "/workspace/test.sh",
            b"#!/bin/sh -e -x\r\necho ignored",
            &strings(&["one", "two"]),
        )
        .expect("parse direct shebang")
        .expect("shebang should be detected");

        assert_eq!(parsed.0, "/bin/sh");
        assert_eq!(
            parsed.1,
            strings(&["-e -x", "/workspace/test.sh", "one", "two"])
        );
    }

    #[test]
    fn parses_env_and_quoted_env_split_strings() {
        let env = parse_javascript_shebang("/workspace/env.sh", b"#!/usr/bin/env sh\n", &[])
            .expect("parse env shebang")
            .expect("env shebang should be detected");
        assert_eq!(env, (String::from("sh"), strings(&["/workspace/env.sh"])));

        let env_split = parse_javascript_shebang(
            "/workspace/env-s.sh",
            b"#! /usr/bin/env -S sh -c 'printf \"%s\" \"$1\"' shell\n",
            &strings(&["tail"]),
        )
        .expect("parse env -S shebang")
        .expect("env -S shebang should be detected");
        assert_eq!(
            env_split,
            (
                String::from("sh"),
                strings(&[
                    "-c",
                    "printf \"%s\" \"$1\"",
                    "shell",
                    "/workspace/env-s.sh",
                    "tail"
                ])
            )
        );
    }

    #[test]
    fn rejects_invalid_or_unbounded_shebangs() {
        assert!(
            parse_javascript_shebang("/workspace/plain", b"plain text", &[])
                .expect("parse plain file")
                .is_none()
        );

        let missing = parse_javascript_shebang("/workspace/missing", b"#!/usr/bin/env\n", &[])
            .expect_err("env without interpreter must fail");
        assert!(missing.to_string().contains("ENOENT"));

        let malformed = parse_javascript_shebang(
            "/workspace/malformed",
            b"#!/usr/bin/env -S sh 'unterminated\n",
            &[],
        )
        .expect_err("unterminated env -S quote must fail");
        assert!(malformed.to_string().contains("ENOEXEC"));

        let overlong = format!("#!/{}\n", "x".repeat(257));
        let too_long = parse_javascript_shebang("/workspace/long", overlong.as_bytes(), &[])
            .expect_err("overlong shebang must fail");
        assert!(too_long.to_string().contains("ENOEXEC"));
    }
}

pub(super) fn resolve_guest_command_entrypoint(
    vm: &mut VmState,
    guest_cwd: &str,
    command: &str,
    path_env: Option<&str>,
) -> Option<String> {
    if !is_path_like_specifier(command) {
        for search_dir in guest_command_search_dirs(vm, guest_cwd, path_env) {
            let candidate = normalize_path(&format!("{search_dir}/{command}"));
            if let Some(entrypoint) =
                resolve_guest_command_path_candidate(&mut vm.kernel, &candidate)
            {
                return Some(entrypoint);
            }
        }

        return None;
    }

    let normalized = resolve_path_like_guest_specifier(guest_cwd, command);
    resolve_guest_command_path_candidate(&mut vm.kernel, &normalized)
}

pub(super) fn resolve_exact_guest_command_entrypoint(
    vm: &mut VmState,
    guest_cwd: &str,
    command: &str,
) -> Option<String> {
    if !is_path_like_specifier(command) {
        return None;
    }

    let normalized = resolve_path_like_guest_specifier(guest_cwd, command);
    resolve_guest_command_path_candidate(&mut vm.kernel, &normalized)
}

pub(super) fn registered_command_name_for_path(
    kernel: &SidecarKernel,
    path: &str,
) -> Option<String> {
    let normalized = normalize_path(path);
    let name = ["/bin/", "/usr/bin/", "/usr/local/bin/", "/opt/agentos/bin/"]
        .into_iter()
        .find_map(|prefix| normalized.strip_prefix(prefix))
        .or_else(|| {
            normalized
                .strip_prefix("/__agentos/commands/")
                .and_then(|suffix| suffix.rsplit('/').next())
        })?;
    (!name.is_empty() && !name.contains('/') && kernel.commands().contains_key(name))
        .then(|| name.to_owned())
}

const LINUX_BINPRM_BUF_SIZE: usize = 256;
const LINUX_MAX_INTERPRETER_DEPTH: usize = 4;

struct LinuxShebang {
    interpreter: String,
    optional_argument: Option<String>,
}

fn parse_linux_shebang(header: &[u8], path: &str) -> Result<Option<LinuxShebang>, SidecarError> {
    if !header.starts_with(b"#!") {
        return Ok(None);
    }

    let payload = &header[2..];
    let newline = payload.iter().position(|byte| *byte == b'\n');
    let line = newline.map_or(payload, |index| &payload[..index]);
    let line_end = line
        .iter()
        .rposition(|byte| !matches!(*byte, b' ' | b'\t'))
        .map(|index| index + 1)
        .ok_or_else(|| SidecarError::host("ENOEXEC", format!("invalid shebang line: {path}")))?;
    let line = &line[..line_end];
    let interpreter_start = line
        .iter()
        .position(|byte| !matches!(*byte, b' ' | b'\t'))
        .ok_or_else(|| SidecarError::host("ENOEXEC", format!("invalid shebang line: {path}")))?;
    let interpreter_tail = &line[interpreter_start..];
    let separator = interpreter_tail
        .iter()
        .position(|byte| matches!(*byte, b' ' | b'\t'));
    if newline.is_none() && header.len() >= LINUX_BINPRM_BUF_SIZE && separator.is_none() {
        return Err(SidecarError::host(
            "ENOEXEC",
            format!("shebang interpreter path exceeds the Linux header limit: {path}"),
        ));
    }

    let interpreter_end = separator.unwrap_or(interpreter_tail.len());
    let interpreter = std::str::from_utf8(&interpreter_tail[..interpreter_end])
        .map_err(|_| SidecarError::host("ENOEXEC", format!("invalid shebang line: {path}")))?;
    if interpreter.is_empty() {
        return Err(SidecarError::host(
            "ENOEXEC",
            format!("invalid shebang line: {path}"),
        ));
    }
    let optional_argument = separator
        .map(|index| &interpreter_tail[index..])
        .map(|value| {
            let start = value
                .iter()
                .position(|byte| !matches!(*byte, b' ' | b'\t'))
                .unwrap_or(value.len());
            let end = value
                .iter()
                .rposition(|byte| !matches!(*byte, b' ' | b'\t'))
                .map(|index| index + 1)
                .unwrap_or(start);
            &value[start..end]
        })
        .filter(|value| !value.is_empty())
        .map(|value| {
            std::str::from_utf8(value)
                .map(str::to_owned)
                .map_err(|_| SidecarError::host("ENOEXEC", format!("invalid shebang line: {path}")))
        })
        .transpose()?;

    Ok(Some(LinuxShebang {
        interpreter: interpreter.to_owned(),
        optional_argument,
    }))
}

struct SpawnPathCandidate {
    lookup_path: String,
    script_argument: String,
}

fn spawn_request_guest_cwd(parent_guest_cwd: &str, request: &ProcessLaunchRequest) -> String {
    request
        .options
        .cwd
        .as_deref()
        .map(|cwd| {
            if cwd.starts_with('/') {
                normalize_path(cwd)
            } else {
                normalize_path(&format!("{parent_guest_cwd}/{cwd}"))
            }
        })
        .unwrap_or_else(|| parent_guest_cwd.to_owned())
}

/// Resolve a bare `posix_spawnp` name with the same candidate selection rules
/// as Linux `execvpe`: the caller's PATH is authoritative, empty entries name
/// the current working directory, permission-denied candidates are skipped in
/// case a later entry succeeds, and EACCES wins if every usable candidate was
/// denied. `script_argument` preserves the candidate spelling Linux places in
/// argv when the selected image is a shebang script (notably `name`, rather
/// than `./name`, for an empty PATH entry).
fn resolve_posix_spawn_path_candidate(
    vm: &mut VmState,
    guest_cwd: &str,
    command: &str,
    search_path: &str,
) -> Result<SpawnPathCandidate, SidecarError> {
    if command.is_empty() {
        return Err(SidecarError::host(
            "ENOENT",
            "posix_spawnp command is empty",
        ));
    }

    let mut permission_error = None;
    for segment in search_path.split(':') {
        // PATH entries are literal. Do not trim whitespace: a directory whose
        // name starts or ends with a space is valid on Linux.
        let script_argument = if segment.is_empty() {
            command.to_owned()
        } else {
            format!("{segment}/{command}")
        };
        let lookup_path = if segment.is_empty() {
            format!("./{command}")
        } else {
            script_argument.clone()
        };
        match vm.kernel.validate_executable_path(&lookup_path, guest_cwd) {
            Ok(_) => {
                return Ok(SpawnPathCandidate {
                    lookup_path,
                    script_argument,
                });
            }
            Err(error) if error.code() == "EACCES" => permission_error = Some(error),
            Err(error) if matches!(error.code(), "ENOENT" | "ENOTDIR") => {}
            Err(error) => return Err(kernel_error(error)),
        }
    }

    if let Some(error) = permission_error {
        Err(kernel_error(error))
    } else {
        Err(SidecarError::host(
            "ENOENT",
            format!("posix_spawnp command not found in PATH: {command}"),
        ))
    }
}

/// Finish pathname and shebang resolution after POSIX file actions have run.
/// `posix_spawnp` searches PATH exactly once in this staged child state, then
/// follows the same recursive shebang rules as literal `posix_spawn`.
pub(super) fn resolve_posix_spawn_program(
    vm: &mut VmState,
    parent_guest_cwd: &str,
    request: &mut ProcessLaunchRequest,
) -> Result<(), SidecarError> {
    if request.options.spawn_exact_path {
        return resolve_spawn_shebang(vm, parent_guest_cwd, request, None);
    }

    let Some(search_path) = request.options.spawn_search_path.clone() else {
        // Ordinary Node child_process resolution retains its existing package
        // and runtime-command behavior. Only proc_spawn_v4/posix_spawnp sends
        // spawnSearchPath and requests Linux execvpe semantics here.
        return Ok(());
    };

    if is_path_like_specifier(&request.command) {
        // POSIX specifies that a name containing '/' bypasses PATH search.
        request.options.spawn_exact_path = true;
        request.options.spawn_search_path = None;
        return resolve_spawn_shebang(vm, parent_guest_cwd, request, None);
    }

    let guest_cwd = spawn_request_guest_cwd(parent_guest_cwd, request);
    let candidate =
        resolve_posix_spawn_path_candidate(vm, &guest_cwd, &request.command, &search_path)?;
    request.command = candidate.lookup_path;
    request.options.spawn_exact_path = true;
    request.options.spawn_search_path = None;
    resolve_spawn_shebang(
        vm,
        parent_guest_cwd,
        request,
        Some(candidate.script_argument),
    )
}

fn resolve_spawn_shebang(
    vm: &mut VmState,
    parent_guest_cwd: &str,
    request: &mut ProcessLaunchRequest,
    mut initial_script_argument: Option<String>,
) -> Result<(), SidecarError> {
    let guest_cwd = spawn_request_guest_cwd(parent_guest_cwd, request);
    let mut interpreter_depth = 0;

    loop {
        let script_argument = initial_script_argument
            .take()
            .unwrap_or_else(|| request.command.clone());
        let resolved_path = vm
            .kernel
            .validate_executable_path(&request.command, &guest_cwd)
            .map_err(kernel_error)?;
        if registered_command_name_for_path(&vm.kernel, &resolved_path).is_some() {
            return Ok(());
        }

        let header = vm
            .kernel
            .pread_file(&resolved_path, 0, LINUX_BINPRM_BUF_SIZE)
            .map_err(kernel_error)?;
        if header.starts_with(b"\0asm") {
            return Ok(());
        }
        let Some(shebang) = parse_linux_shebang(&header, &resolved_path)? else {
            return Err(SidecarError::host(
                "ENOEXEC",
                format!("exec format error: {resolved_path}"),
            ));
        };
        if interpreter_depth >= LINUX_MAX_INTERPRETER_DEPTH {
            return Err(SidecarError::host(
                "ELOOP",
                format!("interpreter recursion for {resolved_path} exceeds the Linux limit"),
            ));
        }
        interpreter_depth += 1;

        if matches!(shebang.interpreter.as_str(), "/usr/bin/env" | "/bin/env") {
            let (command, args) =
                parse_javascript_shebang(&script_argument, &header, &request.args)?.ok_or_else(
                    || {
                        SidecarError::Kernel(format!(
                            "ENOEXEC: invalid env shebang line: {resolved_path}"
                        ))
                    },
                )?;
            request.command = if is_path_like_specifier(&command) {
                resolve_path_like_guest_specifier(&guest_cwd, &command)
            } else {
                let path_env = request
                    .options
                    .env
                    .get("PATH")
                    .or_else(|| vm.guest_env.get("PATH"))
                    .cloned()
                    .unwrap_or_else(|| String::from("/bin:/usr/bin"));
                resolve_posix_spawn_path_candidate(vm, &guest_cwd, &command, &path_env)?.lookup_path
            };
            request.args = args;
            request.options.argv0 = Some(command);
            continue;
        }

        let mut interpreter_args = Vec::with_capacity(request.args.len() + 2);
        if let Some(argument) = shebang.optional_argument {
            interpreter_args.push(argument);
        }
        interpreter_args.push(script_argument);
        interpreter_args.append(&mut request.args);
        request.command = shebang.interpreter;
        request.args = interpreter_args;
        // Linux discards the caller-supplied script argv[0]. The final
        // interpreter pathname becomes argv[0], including across a nested
        // shebang chain.
        request.options.argv0 = Some(request.command.clone());
    }
}

pub(super) fn validate_exact_exec_image_format(
    vm: &mut VmState,
    path: &str,
    runtime: &GuestRuntimeKind,
) -> Result<(), SidecarError> {
    let header = vm.kernel.pread_file(path, 0, 4).map_err(kernel_error)?;
    let valid = exact_exec_image_header_is_valid(runtime, &header);
    if valid {
        Ok(())
    } else {
        Err(SidecarError::host(
            "ENOEXEC",
            format!("exec format error: {path}"),
        ))
    }
}

fn exact_exec_image_header_is_valid(runtime: &GuestRuntimeKind, header: &[u8]) -> bool {
    match runtime {
        GuestRuntimeKind::WebAssembly => header == b"\0asm",
        // Linux recognizes scripts through their shebang, not their filename
        // extension. Runtime resolution has already checked that the shebang
        // selects the corresponding supported interpreter.
        GuestRuntimeKind::JavaScript | GuestRuntimeKind::Python => header.starts_with(b"#!"),
    }
}

fn guest_command_search_dirs(vm: &VmState, guest_cwd: &str, path_env: Option<&str>) -> Vec<String> {
    let mut search_dirs = Vec::new();
    let mut seen = BTreeSet::new();

    if let Some(path) = path_env.or_else(|| vm.guest_env.get("PATH").map(String::as_str)) {
        for segment in path.split(':') {
            let trimmed = segment.trim();
            if trimmed.is_empty() {
                continue;
            }
            let normalized = if trimmed.starts_with('/') {
                normalize_path(trimmed)
            } else {
                normalize_path(&format!("{guest_cwd}/{trimmed}"))
            };
            if seen.insert(normalized.clone()) {
                search_dirs.push(normalized);
            }
        }
    }

    for fallback in ["/bin", "/usr/bin", "/usr/local/bin"] {
        let normalized = String::from(fallback);
        if seen.insert(normalized.clone()) {
            search_dirs.push(normalized);
        }
    }

    search_dirs
}

fn resolve_guest_command_path_candidate(
    kernel: &mut SidecarKernel,
    candidate: &str,
) -> Option<String> {
    let normalized = normalize_path(candidate);

    // Standard command directories and `/opt/agentos/bin` contain
    // kernel-created driver shims. Preserve their command identity, but select
    // the executable image from the live package projection/legacy command
    // mount rather than a configuration-time name -> path cache.
    let registered_shim_name = ["/bin/", "/usr/bin/", "/usr/local/bin/", "/opt/agentos/bin/"]
        .into_iter()
        .find_map(|prefix| normalized.strip_prefix(prefix))
        .filter(|name| !name.is_empty() && !name.contains('/'))
        .filter(|name| kernel.commands().contains_key(*name))
        .map(ToOwned::to_owned);
    if let Some(name) = registered_shim_name {
        if let Some(entrypoint) = resolve_live_registered_command_entrypoint(kernel, &name) {
            return Some(entrypoint);
        }
    }

    resolve_live_kernel_file(kernel, &normalized)
}

fn resolve_live_registered_command_entrypoint(
    kernel: &mut SidecarKernel,
    command: &str,
) -> Option<String> {
    // Match the existing command-discovery precedence: ordered legacy roots
    // win, followed by the `/opt/agentos/bin` package projection.
    let mut roots = kernel
        .read_dir("/__agentos/commands")
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| !entry.is_empty() && entry.chars().all(|ch| ch.is_ascii_digit()))
        .collect::<Vec<_>>();
    roots.sort();
    for root in roots {
        let candidate = normalize_path(&format!("/__agentos/commands/{root}/{command}"));
        if let Some(entrypoint) = resolve_live_kernel_file(kernel, &candidate) {
            return Some(entrypoint);
        }
    }

    let projected = normalize_path(&format!(
        "{}/{command}",
        crate::package_projection::OPT_AGENTOS_BIN
    ));
    resolve_live_kernel_file(kernel, &projected)
}

fn resolve_live_kernel_file(kernel: &SidecarKernel, candidate: &str) -> Option<String> {
    let canonical = normalize_path(&kernel.realpath(candidate).ok()?);
    let stat = kernel.lstat(&canonical).ok()?;
    (!stat.is_directory && !stat.is_symbolic_link).then_some(canonical)
}

#[cfg(test)]
mod live_kernel_command_resolution_tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::KernelVmConfig;
    use agentos_kernel::mount_table::{MountOptions, MountTable};
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};

    fn test_kernel(commands: impl IntoIterator<Item = &'static str>) -> SidecarKernel {
        let mut config = KernelVmConfig::new("vm-live-command-resolution");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, commands))
            .expect("register command-resolution test driver");
        kernel
    }

    #[test]
    fn registered_command_resolution_observes_late_mounts() {
        let mut kernel = test_kernel(["late"]);
        assert_eq!(
            resolve_guest_command_path_candidate(&mut kernel, "/bin/late").as_deref(),
            Some("/bin/late"),
            "the live kernel driver shim remains the fallback before a package is mounted"
        );

        let mut package = MemoryFileSystem::new();
        package
            .write_file("/bin/late", b"#!/usr/bin/env node\n".to_vec())
            .expect("seed late-mounted command");
        kernel
            .mount_filesystem(
                "/opt/agentos",
                package,
                MountOptions::new("late-command-test"),
            )
            .expect("mount command package after kernel configuration");

        assert_eq!(
            resolve_guest_command_path_candidate(&mut kernel, "/bin/late").as_deref(),
            Some("/opt/agentos/bin/late")
        );

        kernel
            .mkdir("/__agentos/commands/001", true)
            .expect("create late legacy command root");
        kernel
            .write_file(
                "/__agentos/commands/001/late",
                b"#!/usr/bin/env node\n".to_vec(),
            )
            .expect("write late legacy command");
        assert_eq!(
            resolve_guest_command_path_candidate(&mut kernel, "/bin/late").as_deref(),
            Some("/__agentos/commands/001/late"),
            "live legacy roots retain their established precedence"
        );
        kernel
            .remove_file("/__agentos/commands/001/late")
            .expect("delete late legacy command");
        assert_eq!(
            resolve_guest_command_path_candidate(&mut kernel, "/bin/late").as_deref(),
            Some("/opt/agentos/bin/late"),
            "deleting the preferred live entrypoint must reveal the live package projection"
        );
    }

    #[test]
    fn registered_command_resolution_does_not_reuse_deleted_backing_paths() {
        let mut kernel = test_kernel(["legacy"]);
        kernel
            .mkdir("/__agentos/commands/001", true)
            .expect("create legacy command root");
        kernel
            .write_file(
                "/__agentos/commands/001/legacy",
                b"#!/usr/bin/env node\n".to_vec(),
            )
            .expect("write legacy command");
        assert_eq!(
            resolve_guest_command_path_candidate(&mut kernel, "/bin/legacy").as_deref(),
            Some("/__agentos/commands/001/legacy")
        );

        kernel
            .remove_file("/__agentos/commands/001/legacy")
            .expect("delete legacy backing command");
        assert_eq!(
            resolve_guest_command_path_candidate(&mut kernel, "/bin/legacy").as_deref(),
            Some("/bin/legacy"),
            "deletion must expose only the live driver shim, never the stale backing image"
        );
    }

    #[test]
    fn javascript_classification_observes_live_symlink_targets_and_updates() {
        let mut kernel = test_kernel([]);
        kernel
            .mkdir("/commands", true)
            .expect("create command directory");
        kernel
            .write_file("/commands/tool", b"\0asm\x01\0\0\0".to_vec())
            .expect("write initial WebAssembly command");
        kernel
            .symlink("/commands/tool", "/tool")
            .expect("create command symlink");

        assert_eq!(
            resolve_javascript_command_entrypoint_inner(
                &mut kernel,
                "/tool",
                &[],
                MAX_JAVASCRIPT_COMMAND_REDIRECT_DEPTH,
            ),
            None
        );

        kernel
            .write_file(
                "/commands/tool",
                b"#!/usr/bin/env node\nconsole.log('updated');\n".to_vec(),
            )
            .expect("replace command content after configuration");
        assert_eq!(
            resolve_javascript_command_entrypoint_inner(
                &mut kernel,
                "/tool",
                &[],
                MAX_JAVASCRIPT_COMMAND_REDIRECT_DEPTH,
            )
            .as_deref(),
            Some("/commands/tool")
        );

        kernel
            .remove_file("/commands/tool")
            .expect("delete command target");
        assert_eq!(
            resolve_javascript_command_entrypoint_inner(
                &mut kernel,
                "/tool",
                &[],
                MAX_JAVASCRIPT_COMMAND_REDIRECT_DEPTH,
            ),
            None
        );
    }

    #[test]
    fn host_files_do_not_resurrect_kernel_command_misses() {
        let mut kernel = test_kernel([]);
        let host = tempfile::NamedTempFile::new().expect("create stale host launch asset");
        std::fs::write(host.path(), b"#!/usr/bin/env node\n")
            .expect("seed stale host launch asset");
        let host_path = host.path().to_string_lossy();

        assert!(host.path().is_file());
        assert_eq!(
            resolve_guest_command_path_candidate(&mut kernel, &host_path),
            None,
            "host existence must not satisfy a kernel command lookup"
        );
    }
}

fn resolve_host_entrypoint_within_vm_host_cwd(
    vm: &VmState,
    specifier: &str,
) -> Option<(String, String)> {
    let candidate = Path::new(specifier);
    if !candidate.is_absolute() {
        return None;
    }

    let normalized_entrypoint = normalize_host_path(candidate);
    let normalized_host_cwd = normalize_host_path(&vm.host_cwd);
    if !path_is_within_root(&normalized_entrypoint, &normalized_host_cwd) {
        return None;
    }

    let relative = normalized_entrypoint
        .strip_prefix(&normalized_host_cwd)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let guest_entrypoint = if relative.is_empty() {
        String::from("/")
    } else {
        normalize_path(&format!("/{relative}"))
    };
    Some((
        guest_entrypoint,
        normalized_entrypoint.to_string_lossy().into_owned(),
    ))
}

pub(super) fn prepare_guest_runtime_env(
    vm: &VmState,
    env: &mut BTreeMap<String, String>,
    guest_cwd: &str,
    _host_cwd: &Path,
    guest_entrypoint: Option<String>,
) -> Result<(), SidecarError> {
    let user = vm.kernel.user_profile();
    let path_mappings = runtime_guest_path_mappings(vm);
    let read_paths = expand_host_access_paths(
        path_mappings
            .iter()
            .map(|mapping| PathBuf::from(&mapping.host_path))
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let write_paths = dedupe_host_paths(&runtime_guest_writable_host_paths(vm));
    let allowed_node_builtins = configured_allowed_node_builtins(vm);
    let loopback_exempt_ports = configured_loopback_exempt_ports(vm);

    env.insert(
        String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
        serde_json::to_string(&path_mappings).map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode guest path mappings: {error}"))
        })?,
    );
    env.entry(String::from(EXECUTION_SANDBOX_ROOT_ENV))
        .or_insert_with(|| {
            normalize_host_path(&vm.runtime_scratch_root)
                .to_string_lossy()
                .into_owned()
        });
    env.insert(
        String::from("AGENTOS_EXTRA_FS_READ_PATHS"),
        serde_json::to_string(
            &read_paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode read paths: {error}"))
        })?,
    );
    env.insert(
        String::from("AGENTOS_EXTRA_FS_WRITE_PATHS"),
        serde_json::to_string(
            &write_paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode write paths: {error}"))
        })?,
    );
    env.insert(
        String::from("AGENTOS_ALLOWED_NODE_BUILTINS"),
        serde_json::to_string(&allowed_node_builtins).map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode allowed builtins: {error}"))
        })?,
    );
    // The guest JS host platform drives subtractive global scrubbing in the
    // per-execution runtime shim (see prepend_v8_runtime_shim).
    env.insert(
        String::from("AGENTOS_JS_PLATFORM"),
        js_runtime_platform_env(vm).to_owned(),
    );
    // Module-resolution mode (omitted when full Node resolution / the default).
    if let Some(resolution) = js_runtime_module_resolution_env(vm) {
        env.insert(
            String::from("AGENTOS_JS_MODULE_RESOLUTION"),
            resolution.to_owned(),
        );
    }
    // Builtin allow-list gate for the live resolver. Present only when builtins
    // should be restricted (non-node platform => deny all; node + explicit
    // allow-list => exactly those). Absent => unrestricted (node default).
    if let Some(allowlist) = js_runtime_enforced_builtins(vm) {
        env.insert(
            String::from("AGENTOS_JS_BUILTIN_ALLOWLIST"),
            serde_json::to_string(&allowlist).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "failed to encode jsRuntime builtin allow-list: {error}"
                ))
            })?,
        );
    }
    // Virtual OS identity (os.cpus/totalmem/freemem/homedir/userInfo/...) now
    // rides the typed `guest_runtime` (see `guest_runtime_identity`), exposed to
    // the guest as the `__agentOSVirtualOs` structured global by the runtime
    // shim — no longer the `AGENTOS_VIRTUAL_OS_*` env vars.
    // Virtual process uid/gid now ride the typed `guest_runtime` identity
    // (see `guest_runtime_identity`), not the `AGENTOS_VIRTUAL_PROCESS_*` env.
    env.entry(String::from("HOME"))
        .or_insert_with(|| user.homedir.clone());
    env.entry(String::from("USER"))
        .or_insert_with(|| user.username.clone());
    env.entry(String::from("LOGNAME"))
        .or_insert_with(|| user.username.clone());
    env.entry(String::from("SHELL"))
        .or_insert_with(|| user.shell.clone());
    env.entry(String::from("PATH")).or_insert_with(|| {
        vm.guest_env
            .get("PATH")
            .cloned()
            .unwrap_or_else(|| crate::vm::DEFAULT_GUEST_PATH_ENV.to_owned())
    });
    env.entry(String::from("TMPDIR"))
        .or_insert_with(|| String::from("/tmp"));
    env.insert(String::from("PWD"), guest_cwd.to_owned());
    if !loopback_exempt_ports.is_empty() {
        env.insert(
            String::from(LOOPBACK_EXEMPT_PORTS_ENV),
            serde_json::to_string(&loopback_exempt_ports).map_err(|error| {
                SidecarError::InvalidState(format!("failed to encode loopback exemptions: {error}"))
            })?,
        );
    }
    if let Some(guest_entrypoint) = guest_entrypoint {
        env.insert(String::from("AGENTOS_GUEST_ENTRYPOINT"), guest_entrypoint);
    }
    Ok(())
}

/// Build the typed per-execution JavaScript limits from the per-VM `VmLimits`
/// (sourced from `CreateVmConfig` on the BARE wire). These ride the execution
/// request, not `AGENTOS_*` env vars — see the env-vs-wire rule in
/// `crates/sidecar/CLAUDE.md`.
pub(super) fn javascript_execution_limits(vm: &VmState) -> JavascriptExecutionLimits {
    JavascriptExecutionLimits {
        v8_heap_limit_mb: vm.limits.js_runtime.v8_heap_limit_mb,
        sync_rpc_wait_timeout_ms: vm.limits.js_runtime.sync_rpc_wait_timeout_ms,
        cpu_time_limit_ms: Some(vm.limits.js_runtime.cpu_time_limit_ms),
        wall_clock_limit_ms: Some(vm.limits.js_runtime.wall_clock_limit_ms),
        import_cache_materialize_timeout_ms: Some(
            vm.limits.js_runtime.import_cache_materialize_timeout_ms,
        ),
        max_timers: Some(vm.limits.js_runtime.max_timers),
        reactor_work_quantum: vm_reactor_work_quantum(&vm.limits),
        bridge_call_timeout_ms: Some(bridge_call_timeout_ms(&vm.limits)),
    }
}

/// Build the typed per-execution guest-runtime identity (virtual `process.*`)
/// from kernel state. Replaces the `AGENTOS_VIRTUAL_PROCESS_{UID,GID,PID,PPID}`
/// env round-trip: the runtime shim reads these from `guest_runtime`, not env.
/// `uid`/`gid` come from the VM user profile (applied to every guest);
/// `pid`/`ppid` are per-process and only set for paths that assigned them.
pub(super) fn guest_runtime_identity(
    vm: &VmState,
    virtual_pid: Option<u64>,
    virtual_ppid: Option<u64>,
) -> GuestRuntimeConfig {
    let user = vm.kernel.user_profile();
    let resource_limits = vm.kernel.resource_limits();
    let mut identity = shared_guest_runtime_identity_with_system(
        &user,
        resource_limits,
        vm.kernel.system_identity(),
        virtual_pid,
        virtual_ppid,
    );
    if let Some(pid) = virtual_pid.and_then(|pid| u32::try_from(pid).ok()) {
        if let Ok(process_identity) = vm.kernel.process_identity(EXECUTION_DRIVER_NAME, pid) {
            identity.virtual_uid = u64::from(process_identity.uid);
            identity.virtual_gid = u64::from(process_identity.gid);
            if let Some(account) = user.account(process_identity.uid) {
                identity.os_user = account.username.clone();
                identity.os_homedir = account.homedir.clone();
                identity.os_shell = account.shell.clone();
            }
        }
    }
    GuestRuntimeConfig {
        virtual_uid: Some(identity.virtual_uid),
        virtual_gid: Some(identity.virtual_gid),
        virtual_pid: identity.virtual_pid,
        virtual_ppid: identity.virtual_ppid,
        virtual_exec_path: None,
        os_cpu_count: Some(identity.os_cpu_count),
        os_totalmem: Some(identity.os_totalmem),
        os_freemem: Some(identity.os_freemem),
        os_homedir: Some(identity.os_homedir),
        os_hostname: Some(identity.os_hostname),
        os_tmpdir: Some(identity.os_tmpdir),
        os_type: Some(identity.os_type),
        os_release: Some(identity.os_release),
        os_version: Some(identity.os_version),
        os_machine: Some(identity.os_machine),
        os_shell: Some(identity.os_shell),
        os_user: Some(identity.os_user),
        high_resolution_time: vm
            .configuration
            .js_runtime
            .as_ref()
            .is_some_and(|cfg| cfg.high_resolution_time.unwrap_or(false)),
        // Userland bundle to bake into the per-sidecar snapshot. The sidecar
        // derives this from configured agent packages with `agent.snapshot`.
        snapshot_userland_code: vm.configuration.snapshot_userland_code.clone(),
    }
}

/// The guest's virtual home directory, sourced from the VM user profile (the
/// same value carried to the guest as `os.homedir()` via `guest_runtime`). Used
/// by sidecar-internal `~`-path resolution; falls back to `/root` for a
/// non-absolute profile value.
pub(super) fn guest_virtual_home(vm: &VmState) -> String {
    let homedir = vm.kernel.user_profile().homedir;
    if homedir.starts_with('/') {
        homedir
    } else {
        String::from("/root")
    }
}

/// Build the typed per-execution Python limits from the per-VM `VmLimits`.
pub(super) fn python_execution_limits(vm: &VmState) -> PythonExecutionLimits {
    python_execution_limits_with_env(vm, &BTreeMap::new())
}

/// Build Python limits while honoring a sidecar-owned per-execution timeout.
pub(super) fn python_execution_limits_with_env(
    vm: &VmState,
    env: &BTreeMap<String, String>,
) -> PythonExecutionLimits {
    PythonExecutionLimits {
        output_buffer_max_bytes: Some(vm.limits.python.output_buffer_max_bytes),
        execution_timeout_ms: env
            .get("AGENTOS_PYTHON_EXECUTION_TIMEOUT_MS")
            .and_then(|value| value.parse().ok())
            .or(Some(vm.limits.python.execution_timeout_ms)),
        max_old_space_mb: Some(vm.limits.python.max_old_space_mb),
        vfs_rpc_timeout_ms: Some(vm.limits.python.vfs_rpc_timeout_ms),
        reactor_work_quantum: vm_reactor_work_quantum(&vm.limits),
        bridge_call_timeout_ms: Some(bridge_call_timeout_ms(&vm.limits)),
        max_open_fds: vm.kernel.resource_limits().max_open_fds,
    }
}

/// Build the typed per-execution WebAssembly limits from normalized per-VM
/// limits and the kernel-owned resource caps. Replaces the old env round-trip;
/// notably this is the path that finally enforces the stack cap that the
/// `AGENTOS_WASM_MAX_STACK_BYTES` env knob set but no reader consumed.
pub(super) fn wasm_execution_limits(vm: &VmState) -> WasmExecutionLimits {
    let resource_limits = vm.kernel.resource_limits();
    WasmExecutionLimits {
        active_cpu_time_limit_ms: Some(vm.limits.wasm.active_cpu_time_limit_ms),
        wall_clock_limit_ms: vm.limits.wasm.wall_clock_limit_ms,
        deterministic_fuel: vm.limits.wasm.deterministic_fuel,
        max_memory_bytes: resource_limits.max_wasm_memory_bytes,
        max_stack_bytes: resource_limits
            .max_wasm_stack_bytes
            .map(|value| value as u64),
        max_module_file_bytes: Some(vm.limits.wasm.max_module_file_bytes),
        max_spawn_file_actions: Some(vm.limits.process.max_spawn_file_actions as u64),
        max_spawn_file_action_bytes: Some(vm.limits.process.max_spawn_file_action_bytes as u64),
        max_open_fds: resource_limits.max_open_fds.map(|value| value as u64),
        max_sockets: resource_limits.max_sockets.map(|value| value as u64),
        max_blocking_read_ms: resource_limits.max_blocking_read_ms,
        prewarm_timeout_ms: Some(vm.limits.wasm.prewarm_timeout_ms),
        runner_heap_limit_mb: Some(vm.limits.wasm.runner_heap_limit_mb),
        reactor_work_quantum: vm_reactor_work_quantum(&vm.limits),
        bridge_call_timeout_ms: Some(bridge_call_timeout_ms(&vm.limits)),
        max_sync_rpc_response_line_bytes: Some(vm.limits.reactor.max_bridge_response_bytes as u64),
        pending_event_count: Some(vm.limits.process.pending_event_count),
        pending_event_bytes: Some(vm.limits.process.pending_event_bytes),
        max_threads: Some(vm.limits.wasm.max_threads),
    }
}

/// The bridge watchdog is a last-resort guard around a sidecar operation, not
/// the operation's user-visible deadline. Give the sidecar a bounded window to
/// publish its typed timeout before the outer V8 wait cancels the call.
const BRIDGE_CALL_DEADLINE_GRACE_MS: u64 = 1_000;

fn bridge_call_timeout_ms(limits: &crate::limits::VmLimits) -> u64 {
    limits
        .reactor
        .operation_deadline_ms
        .saturating_add(BRIDGE_CALL_DEADLINE_GRACE_MS)
}

fn vm_reactor_work_quantum(limits: &crate::limits::VmLimits) -> Option<usize> {
    Some(limits.reactor.work_quantum)
}

#[cfg(test)]
mod reactor_work_quantum_tests {
    use super::{bridge_call_timeout_ms, vm_reactor_work_quantum, BRIDGE_CALL_DEADLINE_GRACE_MS};

    #[test]
    fn native_execution_forwards_vm_reactor_work_quantum_override() {
        let mut limits = crate::limits::VmLimits::default();
        limits.reactor.work_quantum = 3;
        assert_eq!(vm_reactor_work_quantum(&limits), Some(3));
    }

    #[test]
    fn bridge_watchdog_runs_after_the_typed_operation_deadline() {
        let mut limits = crate::limits::VmLimits::default();
        limits.reactor.operation_deadline_ms = 50;
        assert_eq!(
            bridge_call_timeout_ms(&limits),
            50 + BRIDGE_CALL_DEADLINE_GRACE_MS
        );

        limits.reactor.operation_deadline_ms = u64::MAX;
        assert_eq!(bridge_call_timeout_ms(&limits), u64::MAX);
    }
}

/// The guest JavaScript host platform configured for this VM, defaulting to
/// full Node.js emulation when no `jsRuntime` config was supplied at create.
fn js_runtime_platform(vm: &VmState) -> vm_config::JsRuntimePlatform {
    vm.configuration
        .js_runtime
        .as_ref()
        .map(|cfg| cfg.platform)
        .unwrap_or(vm_config::JsRuntimePlatform::Node)
}

/// Lowercase wire name for the configured platform, mirroring the serde
/// representation of `vm_config::JsRuntimePlatform`.
fn js_runtime_platform_env(vm: &VmState) -> &'static str {
    match js_runtime_platform(vm) {
        vm_config::JsRuntimePlatform::Node => "node",
        vm_config::JsRuntimePlatform::Browser => "browser",
        vm_config::JsRuntimePlatform::Neutral => "neutral",
        vm_config::JsRuntimePlatform::Bare => "bare",
    }
}

/// Wire name for the configured module-resolution mode, or `None` when it is the
/// full-Node default (which the live resolver also assumes when the env is unset).
fn js_runtime_module_resolution_env(vm: &VmState) -> Option<&'static str> {
    let resolution = vm
        .configuration
        .js_runtime
        .as_ref()
        .map(|cfg| cfg.module_resolution)
        .unwrap_or(vm_config::JsModuleResolution::Node);
    match resolution {
        vm_config::JsModuleResolution::Node => None,
        vm_config::JsModuleResolution::Relative => Some("relative"),
        vm_config::JsModuleResolution::None => Some("none"),
    }
}

/// The builtin allow-list the live resolver should enforce, or `None` to leave
/// builtins unrestricted (full Node default — preserving today's behavior).
/// Non-node platforms enforce an empty list (deny all builtins).
fn js_runtime_enforced_builtins(vm: &VmState) -> Option<Vec<String>> {
    if js_runtime_platform(vm) != vm_config::JsRuntimePlatform::Node {
        return Some(Vec::new());
    }
    vm.configuration
        .js_runtime
        .as_ref()
        .and_then(|cfg| cfg.allowed_builtins.clone())
}

fn configured_allowed_node_builtins(vm: &VmState) -> Vec<String> {
    // Non-node platforms expose no Node builtin modules at all.
    if js_runtime_platform(vm) != vm_config::JsRuntimePlatform::Node {
        return Vec::new();
    }
    // Under the node platform an explicit allow-list wins — including an explicit
    // empty list, which means deny all. Absence falls back to the engine default.
    let configured = match vm
        .configuration
        .js_runtime
        .as_ref()
        .and_then(|cfg| cfg.allowed_builtins.as_ref())
    {
        Some(list) => list.clone(),
        None => DEFAULT_ALLOWED_NODE_BUILTINS
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>(),
    };
    dedupe_strings(&configured)
}

fn configured_loopback_exempt_ports(vm: &VmState) -> Vec<String> {
    if !vm.configuration.loopback_exempt_ports.is_empty() {
        return vm
            .configuration
            .loopback_exempt_ports
            .iter()
            .map(ToString::to_string)
            .collect();
    }

    vm.create_loopback_exempt_ports
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// Extract the `hostPath` string from a mount plugin's JSON-encoded config.
fn mount_config_host_path(config: &str) -> Option<String> {
    serde_json::from_str::<Value>(config)
        .ok()?
        .get("hostPath")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Host path backing a mount for HOST-SIDE resolution (entrypoint launch, import
/// cache location). `agentos_packages` is deliberately excluded by callers:
/// package tar mounts are guest-native and resolve through the kernel VFS.
fn mount_config_host_backing_path(config: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(config).ok()?;
    value
        .get("hostPath")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn runtime_guest_writable_host_paths(vm: &VmState) -> Vec<PathBuf> {
    vm.configuration
        .mounts
        .iter()
        .filter(|mount| !mount.read_only)
        .filter_map(|mount| {
            ((mount.plugin.id == "host_dir") || (mount.plugin.id == "module_access"))
                .then(|| mount_config_host_path(&mount.plugin.config))
                .flatten()
                .map(PathBuf::from)
        })
        .collect()
}

fn runtime_guest_path_mappings(vm: &VmState) -> Vec<RuntimeGuestPathMapping> {
    let mut mappings = vm
        .configuration
        .mounts
        .iter()
        .filter_map(|mount| {
            ((mount.plugin.id == "host_dir") || (mount.plugin.id == "module_access"))
                .then(|| {
                    mount_config_host_path(&mount.plugin.config).map(|host_path| {
                        RuntimeGuestPathMapping {
                            guest_path: normalize_path(&mount.guest_path),
                            host_path,
                            read_only: mount.read_only,
                        }
                    })
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    let mut extra_node_modules_roots = mappings
        .iter()
        .filter(|mapping| mapping.guest_path.starts_with("/root/node_modules/"))
        .filter_map(|mapping| {
            host_node_modules_root(Path::new(&mapping.host_path)).map(|host_root| {
                RuntimeGuestPathMapping {
                    guest_path: String::from("/root/node_modules"),
                    host_path: host_root.to_string_lossy().into_owned(),
                    read_only: mapping.read_only,
                }
            })
        })
        .collect::<Vec<_>>();
    mappings.append(&mut extra_node_modules_roots);
    mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.guest_path.len()));
    mappings.dedup_by(|left, right| {
        left.guest_path == right.guest_path && left.host_path == right.host_path
    });
    mappings
}

fn host_node_modules_root(path: &Path) -> Option<PathBuf> {
    if let Some(root) = path
        .ancestors()
        .filter(|candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("node_modules")
        })
        .last()
        .map(Path::to_path_buf)
    {
        return Some(root);
    }

    fs::canonicalize(path)
        .ok()?
        .ancestors()
        .filter(|candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("node_modules")
        })
        .last()
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod runtime_guest_path_mapping_tests {
    use super::{host_node_modules_root, javascript_sync_rpc_option_bool};
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn host_node_modules_root_prefers_workspace_root_over_pnpm_package_node_modules() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let temp =
            std::env::temp_dir().join(format!("agentos-native-sidecar-node-modules-{unique}"));
        let workspace_node_modules = temp.join("node_modules");
        let package_root = workspace_node_modules
            .join(".pnpm")
            .join("example@1.0.0")
            .join("node_modules")
            .join("@scope")
            .join("pkg");
        fs::create_dir_all(&package_root).expect("package root should be created");

        let resolved =
            host_node_modules_root(&package_root).expect("node_modules root should resolve");

        assert_eq!(resolved, workspace_node_modules);

        fs::remove_dir_all(&temp).expect("temp tree should be removed");
    }

    #[test]
    fn host_node_modules_root_preserves_symlinked_workspace_node_modules_path() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "agentos-native-sidecar-node-modules-symlink-{unique}"
        ));
        let workspace_node_modules = temp.join("node_modules");
        let package_link = workspace_node_modules.join("@scope").join("pkg");
        let real_package = temp.join("registry").join("agent").join("pkg");
        fs::create_dir_all(package_link.parent().expect("package parent should exist"))
            .expect("scoped parent should be created");
        fs::create_dir_all(&real_package).expect("real package root should be created");
        std::os::unix::fs::symlink(&real_package, &package_link)
            .expect("package symlink should be created");

        let resolved =
            host_node_modules_root(&package_link).expect("node_modules root should resolve");

        assert_eq!(resolved, workspace_node_modules);

        fs::remove_dir_all(&temp).expect("temp tree should be removed");
    }

    #[test]
    fn javascript_sync_rpc_option_bool_accepts_boolean_recursive_argument() {
        assert_eq!(
            javascript_sync_rpc_option_bool(&[json!("/workspace"), json!(true)], 1, "recursive"),
            Some(true)
        );
        assert_eq!(
            javascript_sync_rpc_option_bool(
                &[json!("/workspace"), json!({ "recursive": false })],
                1,
                "recursive"
            ),
            Some(false)
        );
    }
}

#[cfg(test)]
mod kernel_poll_sync_rpc_tests {
    use super::{
        install_kernel_stdin_pipe, parse_kernel_poll_args, parse_kernel_stdin_read_args,
        rollback_failed_top_level_process_start, service_javascript_kernel_poll_sync_rpc,
        ActiveExecution, ActiveExecutionEvent, ActiveProcess, BindingExecution, HostRpcRequest,
        KernelPollFdResponse, SidecarKernel, EXECUTION_DRIVER_NAME, JAVASCRIPT_COMMAND,
    };
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::poll::{POLLHUP, POLLIN};
    use agentos_kernel::vfs::MemoryFileSystem;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};
    use tokio::sync::Notify;

    fn test_runtime_context() -> agentos_runtime_tokio::RuntimeContext {
        agentos_runtime_tokio::SidecarRuntime::process(
            &agentos_runtime_tokio::RuntimeConfig::default(),
        )
        .expect("create test runtime")
        .context()
    }

    #[test]
    fn explicit_null_kernel_wait_timeouts_mean_indefinite_readiness_waits() {
        let stdin_request = HostRpcRequest {
            id: 1,
            method: String::from("__kernel_stdin_read"),
            raw_bytes_args: HashMap::new(),
            args: vec![json!(4096), Value::Null],
        };
        assert_eq!(
            parse_kernel_stdin_read_args(&stdin_request).expect("parse stdin wait"),
            (4096, None)
        );

        let poll_request = HostRpcRequest {
            id: 2,
            method: String::from("__kernel_poll"),
            raw_bytes_args: HashMap::new(),
            args: vec![json!([{ "fd": 0, "events": POLLIN.bits() }]), Value::Null],
        };
        let (_, timeout_ms) = parse_kernel_poll_args(&poll_request).expect("parse poll wait");
        assert_eq!(timeout_ms, -1);
    }

    #[test]
    fn javascript_kernel_poll_sync_rpc_reports_multiple_kernel_fds() {
        let mut config = KernelVmConfig::new("vm-js-kernel-poll");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND],
            ))
            .expect("register execution driver");

        let kernel_handle = kernel
            .spawn_process(
                JAVASCRIPT_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn javascript kernel process");
        let pid = kernel_handle.pid();

        let (stdin_read_fd, stdin_write_fd) = kernel
            .open_pipe(EXECUTION_DRIVER_NAME, pid)
            .expect("open kernel stdin pipe");
        kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, pid, stdin_read_fd, 0)
            .expect("dup stdin pipe onto fd 0");
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, pid, stdin_read_fd)
            .expect("close original stdin read fd");

        let process = ActiveProcess::new(
            pid,
            kernel_handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            super::GuestRuntimeKind::JavaScript,
            ActiveExecution::Binding(BindingExecution::default()),
        );

        kernel
            .fd_write(EXECUTION_DRIVER_NAME, pid, stdin_write_fd, b"poll-ready")
            .expect("write kernel stdin payload");
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, pid, stdin_write_fd)
            .expect("close kernel stdin writer");

        let response = service_javascript_kernel_poll_sync_rpc(
            &mut kernel,
            &process,
            &HostRpcRequest {
                id: 1,
                method: String::from("__kernel_poll"),
                raw_bytes_args: HashMap::new(),
                args: vec![
                    json!([
                        { "fd": 0, "events": POLLIN.bits() },
                        { "fd": 1, "events": POLLIN.bits() }
                    ]),
                    json!(250),
                ],
            },
        )
        .expect("poll kernel fds");

        assert_eq!(response["readyCount"], Value::from(1));
        let fds: Vec<KernelPollFdResponse> =
            serde_json::from_value(response["fds"].clone()).expect("kernel poll fd response");
        assert_eq!(
            fds,
            vec![
                KernelPollFdResponse {
                    fd: 0,
                    events: POLLIN.bits(),
                    revents: (POLLIN | POLLHUP).bits(),
                },
                KernelPollFdResponse {
                    fd: 1,
                    events: POLLIN.bits(),
                    revents: 0,
                },
            ]
        );

        process.kernel_handle.finish(0);
        kernel.waitpid(pid).expect("wait javascript kernel process");
    }

    #[test]
    fn queued_process_event_wakes_shared_process_pump() {
        let mut config = KernelVmConfig::new("vm-process-event-notify");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND],
            ))
            .expect("register execution driver");
        let kernel_handle = kernel
            .spawn_process(
                JAVASCRIPT_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn javascript kernel process");
        let pid = kernel_handle.pid();
        let event_notify = Arc::new(Notify::new());
        let mut process = ActiveProcess::new(
            pid,
            kernel_handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            super::GuestRuntimeKind::JavaScript,
            ActiveExecution::Binding(BindingExecution::default()),
        )
        .with_event_notify(Arc::clone(&event_notify));

        process
            .queue_pending_execution_event(ActiveExecutionEvent::Stdout(b"echo".to_vec()))
            .expect("queue durable process event");

        let mut notified = Box::pin(event_notify.notified());
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(notified.as_mut().poll(&mut context), Poll::Ready(()));
        assert!(matches!(
            process.pending_execution_events.pop_front(),
            Some(ActiveExecutionEvent::Stdout(bytes)) if bytes == b"echo"
        ));

        process.kernel_handle.finish(0);
        kernel.waitpid(pid).expect("wait javascript kernel process");
    }

    #[test]
    fn top_level_setup_failure_reaps_process_pty_and_descriptors() {
        let mut config = KernelVmConfig::new("vm-top-level-setup-rollback");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND],
            ))
            .expect("register execution driver");
        let baseline = kernel.resource_snapshot();
        let kernel_handle = kernel
            .spawn_process(
                JAVASCRIPT_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn startup process");
        let pid = kernel_handle.pid();
        let notify = Arc::new(Notify::new());
        let _runtime_control =
            ActiveProcess::attach_runtime_control_before_start(&kernel_handle, notify)
                .expect("attach startup endpoint");
        let (master_fd, slave_fd, _) = kernel
            .open_pty(EXECUTION_DRIVER_NAME, pid)
            .expect("allocate startup PTY");
        kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, pid, slave_fd, 0)
            .expect("install startup PTY stdin");
        assert_ne!(kernel.resource_snapshot(), baseline);
        assert!(kernel.list_processes().contains_key(&pid));

        rollback_failed_top_level_process_start(
            &mut kernel,
            &kernel_handle,
            None,
            "test setup failure",
        );

        assert!(kernel.list_processes().is_empty());
        assert_eq!(kernel.resource_snapshot(), baseline);
        let _ = master_fd;
    }

    #[test]
    fn top_level_engine_start_failure_reaps_process_and_stdin_pipe() {
        let mut config = KernelVmConfig::new("vm-top-level-engine-start-rollback");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND],
            ))
            .expect("register execution driver");
        let baseline = kernel.resource_snapshot();
        let kernel_handle = kernel
            .spawn_process(
                JAVASCRIPT_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn startup process");
        let pid = kernel_handle.pid();
        let notify = Arc::new(Notify::new());
        let _runtime_control =
            ActiveProcess::attach_runtime_control_before_start(&kernel_handle, notify)
                .expect("attach startup endpoint");
        install_kernel_stdin_pipe(&mut kernel, pid).expect("install startup stdin pipe");
        let binding = BindingExecution::default();
        let cancelled = Arc::clone(&binding.cancelled);
        let mut execution = ActiveExecution::Binding(binding);
        assert_ne!(kernel.resource_snapshot(), baseline);

        rollback_failed_top_level_process_start(
            &mut kernel,
            &kernel_handle,
            Some(&mut execution),
            "test engine-start failure",
        );

        assert!(cancelled.load(Ordering::Acquire));
        assert!(kernel.list_processes().is_empty());
        assert_eq!(kernel.resource_snapshot(), baseline);
    }
}

fn dedupe_strings(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value.clone());
        }
    }
    deduped
}

fn dedupe_host_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let normalized = normalize_host_path(path);
        let key = normalized.to_string_lossy().into_owned();
        if seen.insert(key) {
            deduped.push(normalized);
        }
    }
    deduped
}

fn expand_host_access_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut expanded = Vec::new();
    let mut seen = BTreeSet::new();

    let mut add_path = |candidate: PathBuf| {
        let normalized = normalize_host_path(&candidate);
        let key = normalized.to_string_lossy().into_owned();
        if seen.insert(key) {
            expanded.push(normalized);
        }
    };

    for host_path in paths {
        add_path(host_path.clone());
        if let Ok(realpath) = fs::canonicalize(host_path) {
            add_path(realpath);
        }

        if host_path.file_name().and_then(|name| name.to_str()) != Some("node_modules") {
            continue;
        }

        let mut current = host_path.parent();
        while let Some(parent) = current {
            let candidate = parent.join("node_modules");
            if candidate.exists() {
                add_path(candidate.clone());
                if let Ok(realpath) = fs::canonicalize(&candidate) {
                    add_path(realpath);
                }
            }
            current = parent.parent();
        }
    }

    expanded
}

/// Classify a package command from the authoritative kernel VFS. Resolution
/// follows symlinks with the launch authority and reads only the four-byte WASM
/// magic prefix. The sole full-module read remains
/// `stage_kernel_wasm_launch_asset`, where `maxModuleFileBytes` is enforced.
pub(super) fn stage_agentos_package_command(
    vm: &mut VmState,
    resolved: &mut ResolvedChildProcessExecution,
    authority: WasmLaunchAuthority,
) -> Result<(), SidecarError> {
    const WASM_MAGIC: &[u8] = b"\0asm";
    if resolved.binding_command
        || !matches!(
            resolved.runtime,
            GuestRuntimeKind::JavaScript | GuestRuntimeKind::WebAssembly
        )
    {
        return Ok(());
    }
    let Some(guest_entrypoint) = resolved
        .env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .map(|path| normalize_path(path))
    else {
        return Ok(());
    };
    if !guest_path_is_within_agentos_package_mount(vm, &guest_entrypoint) {
        return Ok(());
    }
    // `node script.mjs` reads the script as interpreter input; it does not
    // execute the script pathname. Classifying that input through the runtime
    // image loader would incorrectly require an execute bit (and would make a
    // normal 0644 JavaScript module fail with EACCES). Bare/path command
    // launches still pass through the executable-image classifier below so a
    // projected command containing WASM is selected without weakening Linux
    // exec permission checks.
    if resolved.runtime == GuestRuntimeKind::JavaScript
        && is_node_runtime_command(&resolved.command)
    {
        return Ok(());
    }
    let prefix = match authority {
        WasmLaunchAuthority::TrustedInitialImage => vm
            .kernel
            .load_trusted_initial_runtime_image_prefix(&guest_entrypoint, WASM_MAGIC.len()),
        WasmLaunchAuthority::GuestProcessImage { requester_pid } => {
            vm.kernel.load_process_runtime_image_prefix(
                EXECUTION_DRIVER_NAME,
                requester_pid,
                &guest_entrypoint,
                WASM_MAGIC.len(),
            )
        }
    }
    .map_err(kernel_error)?;
    let real_entrypoint = normalize_path(&prefix.canonical_path);
    if !guest_path_is_within_agentos_package_mount(vm, &real_entrypoint) {
        return Err(SidecarError::host(
            "EACCES",
            format!(
                "agentOS package command resolved outside its package mount: {guest_entrypoint} -> {real_entrypoint}"
            ),
        ));
    }
    if prefix.bytes != WASM_MAGIC {
        return Ok(());
    }
    resolved.runtime = GuestRuntimeKind::WebAssembly;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WasmLaunchAuthority {
    /// A module selected directly by the trusted client Execute request may be
    /// admitted once from that request's host source path.
    TrustedInitialImage,
    /// A guest spawn/exec replacement must already exist in the kernel VFS and
    /// remains subject to the guest process's execute DAC checks.
    GuestProcessImage { requester_pid: u32 },
}

/// Stage a WebAssembly module from the authoritative kernel filesystem into
/// the VM-private launch-asset tree consumed by the compatibility engine.
pub(super) fn stage_kernel_wasm_launch_asset(
    vm: &mut VmState,
    resolved: &mut ResolvedChildProcessExecution,
    authority: WasmLaunchAuthority,
) -> Result<(), SidecarError> {
    if resolved.binding_command || resolved.runtime != GuestRuntimeKind::WebAssembly {
        return Ok(());
    }
    let Some(guest_entrypoint) = resolved
        .env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .map(|path| normalize_path(path))
    else {
        return Ok(());
    };
    let maximum_bytes = vm.limits.wasm.max_module_file_bytes;
    let image = match authority {
        WasmLaunchAuthority::TrustedInitialImage => vm
            .kernel
            .load_trusted_initial_runtime_image(&guest_entrypoint, maximum_bytes),
        WasmLaunchAuthority::GuestProcessImage { requester_pid } => {
            vm.kernel.load_process_runtime_image(
                EXECUTION_DRIVER_NAME,
                requester_pid,
                &guest_entrypoint,
                maximum_bytes,
            )
        }
    }
    .map_err(kernel_error)?;
    let real_entrypoint = normalize_path(&image.canonical_path);
    if let Some(format) =
        agentos_execution::wasm::detect_native_binary_format(image.bytes.as_slice())
    {
        let header = image.bytes.iter().copied().take(4).collect();
        return Err(wasm_error(
            agentos_execution::wasm::WasmExecutionError::NativeBinaryNotSupported {
                path: PathBuf::from(&real_entrypoint),
                header,
                format,
            },
        ));
    }
    let asset_path = runtime_asset_path_for_guest(vm, &real_entrypoint);
    if let Some(parent) = asset_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create runtime launch-asset parent: {error}"
            ))
        })?;
    }
    fs::write(&asset_path, image.bytes).map_err(|error| {
        SidecarError::Io(format!("failed to stage runtime launch image: {error}"))
    })?;
    fs::set_permissions(&asset_path, fs::Permissions::from_mode(image.mode & 0o7777)).map_err(
        |error| {
            SidecarError::Io(format!(
                "failed to set runtime launch image mode on {}: {error}",
                asset_path.display()
            ))
        },
    )?;
    resolved.entrypoint = asset_path.to_string_lossy().into_owned();
    Ok(())
}

async fn admit_trusted_initial_wasm_source_if_missing(
    vm: &mut VmState,
    resolved: &ResolvedChildProcessExecution,
) -> Result<(), SidecarError> {
    if resolved.binding_command || resolved.runtime != GuestRuntimeKind::WebAssembly {
        return Ok(());
    }
    let Some(guest_entrypoint) = resolved
        .env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .map(|path| normalize_path(path))
    else {
        return Ok(());
    };
    match vm
        .kernel
        .load_trusted_initial_runtime_image(&guest_entrypoint, vm.limits.wasm.max_module_file_bytes)
    {
        Ok(_) => return Ok(()),
        Err(error)
            if error.code() == "ENOENT"
                && !resolved_entrypoint_uses_kernel_launch_asset(
                    vm,
                    resolved,
                    &guest_entrypoint,
                ) => {}
        Err(error) => return Err(kernel_error(error)),
    }

    // A low-level Execute request may name a trusted caller-supplied host
    // module while selecting a guest cwd such as `/`. Open and read that exact
    // source on the fixed blocking executor, then admit it once. The opened
    // handle pins the selected inode across metadata validation and the
    // bounded read; after admission no guest operation consults the host path.
    let host_entrypoint = {
        let candidate = Path::new(&resolved.entrypoint);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            resolved.host_cwd.join(candidate)
        }
    };
    let source = read_bounded_host_launch_source_async(
        vm,
        host_entrypoint,
        vm.limits.wasm.max_module_file_bytes,
    )
    .await?;
    vm.kernel
        .admit_trusted_initial_runtime_image(
            &guest_entrypoint,
            source.bytes,
            source.mode,
            vm.limits.wasm.max_module_file_bytes,
        )
        .map_err(kernel_error)
}

pub(super) fn prepare_javascript_launch_assets(
    vm: &mut VmState,
    resolved: &ResolvedChildProcessExecution,
    env: &BTreeMap<String, String>,
    authority: WasmLaunchAuthority,
    prepared_source: Option<&str>,
) -> Result<(), SidecarError> {
    let guest_entrypoint = env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .cloned()
        // An absolute `entrypoint` may be a host path that lives inside the VM's
        // host cwd (callers can pass a fully-qualified host path). The guest sees
        // it at its translated guest path (host_cwd -> guest_cwd), so the private
        // launch asset must be keyed by that guest path rather than the raw host path. Falling
        // back to the host path here would materialize the file at the wrong guest
        // location and the runtime's `require()` would fail with "Cannot find
        // module".
        .or_else(|| {
            resolve_host_entrypoint_within_vm_host_cwd(vm, &resolved.entrypoint)
                .map(|(guest_entrypoint, _)| guest_entrypoint)
        })
        .or_else(|| {
            resolved
                .entrypoint
                .starts_with('/')
                .then(|| normalize_path(&resolved.entrypoint))
        });
    let Some(guest_entrypoint) = guest_entrypoint else {
        return Ok(());
    };
    let initial_stat = match authority {
        WasmLaunchAuthority::TrustedInitialImage => vm.kernel.lstat(&guest_entrypoint),
        WasmLaunchAuthority::GuestProcessImage { requester_pid } => {
            vm.kernel
                .lstat_for_process(EXECUTION_DRIVER_NAME, requester_pid, &guest_entrypoint)
        }
    };
    if matches!(
        initial_stat.as_ref().err().map(|error| error.code()),
        Some("ENOENT" | "ENOTDIR")
    ) && authority == WasmLaunchAuthority::TrustedInitialImage
        && !resolved_entrypoint_uses_kernel_launch_asset(vm, resolved, &guest_entrypoint)
    {
        let host_entrypoint = {
            let candidate = Path::new(&resolved.entrypoint);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                resolved.host_cwd.join(candidate)
            }
        };
        match fs::metadata(&host_entrypoint) {
            Ok(_) => {
                import_host_entrypoint_to_kernel(vm, &guest_entrypoint, &host_entrypoint, None)?;
                return materialize_guest_launch_asset(
                    vm,
                    &guest_entrypoint,
                    authority,
                    prepared_source,
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(SidecarError::Io(format!(
                    "failed to inspect trusted JavaScript entrypoint {}: {error}",
                    host_entrypoint.display()
                )));
            }
        }
    }
    initial_stat.map_err(kernel_error)?;
    materialize_guest_launch_asset(vm, &guest_entrypoint, authority, prepared_source)
}

pub(super) fn resolve_agentos_package_javascript_launch_entrypoint(
    vm: &mut VmState,
    requester_pid: u32,
    env: &mut BTreeMap<String, String>,
) -> Result<Option<String>, SidecarError> {
    let Some(guest_entrypoint) = env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .map(|path| normalize_path(path))
    else {
        return Ok(None);
    };
    let package_entrypoint = guest_path_is_within_agentos_package_mount(vm, &guest_entrypoint);
    let resolved_entrypoint =
        vm.kernel
            .realpath_for_process(EXECUTION_DRIVER_NAME, requester_pid, &guest_entrypoint);
    let module_mode_entrypoint = if package_entrypoint {
        let real_entrypoint = normalize_path(&resolved_entrypoint.map_err(kernel_error)?);
        if !guest_path_is_within_agentos_package_mount(vm, &real_entrypoint) {
            return Err(SidecarError::host(
                "EACCES",
                format!(
                    "agentOS package JavaScript entrypoint resolved outside its package mount: {guest_entrypoint} -> {real_entrypoint}"
                ),
            ));
        }
        env.insert(
            String::from("AGENTOS_GUEST_ENTRYPOINT"),
            real_entrypoint.clone(),
        );
        real_entrypoint
    } else {
        match resolved_entrypoint {
            Ok(real_entrypoint) => normalize_path(&real_entrypoint),
            Err(error) if matches!(error.code(), "ENOENT" | "ENOTDIR") => guest_entrypoint.clone(),
            Err(error) => return Err(kernel_error(error)),
        }
    };

    let module_mode = guest_javascript_entrypoint_module_mode_override(
        &mut vm.kernel,
        requester_pid,
        &module_mode_entrypoint,
    )?;
    match module_mode {
        Some(uses_module_mode) => {
            env.insert(
                String::from("AGENTOS_GUEST_ENTRYPOINT_MODULE_MODE"),
                if uses_module_mode {
                    String::from("1")
                } else {
                    String::from("0")
                },
            );
        }
        None => {
            // This is per-launch adapter state, not guest environment. Remove
            // any selector inherited from a parent and let the execution
            // engine preserve its inline-source compatibility detection when
            // neither an extension nor a package scope chooses a mode.
            env.remove("AGENTOS_GUEST_ENTRYPOINT_MODULE_MODE");
        }
    }
    Ok(package_entrypoint.then_some(module_mode_entrypoint))
}

fn guest_path_is_within_agentos_package_mount(vm: &VmState, guest_path: &str) -> bool {
    let normalized = normalize_path(guest_path);
    vm.configuration.mounts.iter().any(|mount| {
        mount.plugin.id == "agentos_packages" && {
            let guest_root = normalize_path(&mount.guest_path);
            normalized == guest_root || normalized.starts_with(&format!("{guest_root}/"))
        }
    })
}

fn guest_javascript_entrypoint_module_mode_override(
    kernel: &mut SidecarKernel,
    requester_pid: u32,
    guest_path: &str,
) -> Result<Option<bool>, SidecarError> {
    match Path::new(guest_path)
        .extension()
        .and_then(|ext| ext.to_str())
    {
        Some("mjs" | "mts") => Ok(Some(true)),
        Some("cjs" | "cts") => Ok(Some(false)),
        Some("js") => {
            Ok(
                nearest_guest_package_json_module_mode(kernel, requester_pid, guest_path)?
                    .map(|mode| mode == GuestPackageModuleMode::Module),
            )
        }
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuestPackageModuleMode {
    Module,
    CommonJs,
}

fn nearest_guest_package_json_module_mode(
    kernel: &mut SidecarKernel,
    requester_pid: u32,
    guest_path: &str,
) -> Result<Option<GuestPackageModuleMode>, SidecarError> {
    let mut dir = dirname(guest_path);
    loop {
        let package_json_path = if dir == "/" {
            String::from("/package.json")
        } else {
            normalize_path(&format!("{dir}/package.json"))
        };
        let bytes = match kernel.read_file_for_process(
            EXECUTION_DRIVER_NAME,
            requester_pid,
            &package_json_path,
        ) {
            Ok(bytes) => Some(bytes),
            Err(error) if matches!(error.code(), "ENOENT" | "ENOTDIR") => None,
            Err(error) => return Err(kernel_error(error)),
        };
        if let Some(bytes) = bytes {
            let contents = String::from_utf8(bytes).map_err(|error| {
                SidecarError::host(
                    "EILSEQ",
                    format!(
                        "package configuration {package_json_path} is not valid UTF-8: {error}"
                    ),
                )
            })?;
            let value = serde_json::from_str::<Value>(&contents).map_err(|error| {
                SidecarError::host(
                    "ERR_INVALID_PACKAGE_CONFIG",
                    format!("invalid package configuration {package_json_path}: {error}"),
                )
            })?;
            return Ok(Some(
                if value.get("type").and_then(Value::as_str) == Some("module") {
                    GuestPackageModuleMode::Module
                } else {
                    GuestPackageModuleMode::CommonJs
                },
            ));
        }
        if dir == "/"
            || Path::new(&dir).file_name().and_then(|name| name.to_str()) == Some("node_modules")
        {
            return Ok(None);
        }
        dir = dirname(&dir);
    }
}

#[cfg(test)]
mod package_json_launch_tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::resource_accounting::ResourceLimits;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn package_json_test_kernel(max_pread_bytes: usize) -> (SidecarKernel, u32) {
        let mut config = KernelVmConfig::new("vm-package-json-launch");
        config.permissions = Permissions::allow_all();
        config.resources = ResourceLimits {
            max_pread_bytes: Some(max_pread_bytes),
            ..ResourceLimits::default()
        };
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND],
            ))
            .expect("register JavaScript test driver");
        let process = kernel
            .spawn_process(
                JAVASCRIPT_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn JavaScript test process");
        kernel
            .mkdir("/pkg/sub", true)
            .expect("create package test directory");
        (kernel, process.pid())
    }

    #[test]
    fn package_json_type_read_accepts_exact_limit_and_rejects_plus_one() {
        let exact = br#"{"type":"module"}"#;
        let (mut kernel, pid) = package_json_test_kernel(exact.len());
        kernel
            .write_file("/pkg/package.json", exact.to_vec())
            .expect("write exact package config");
        assert_eq!(
            nearest_guest_package_json_module_mode(&mut kernel, pid, "/pkg/sub/main.js")
                .expect("read exact package config"),
            Some(GuestPackageModuleMode::Module)
        );

        let mut plus_one = exact.to_vec();
        plus_one.push(b' ');
        kernel
            .write_file("/pkg/package.json", plus_one)
            .expect("write plus-one package config");
        let error = nearest_guest_package_json_module_mode(&mut kernel, pid, "/pkg/sub/main.js")
            .expect_err("plus-one package config must exceed the bounded read");
        assert_eq!(error.code(), Some("EINVAL"));
        assert!(error.to_string().contains("limits.resources.maxPreadBytes"));
    }

    #[test]
    fn package_json_type_read_preserves_typed_failures() {
        let (mut kernel, pid) = package_json_test_kernel(128);
        kernel
            .write_file("/pkg/package.json", vec![0xff])
            .expect("write invalid UTF-8 package config");
        assert_eq!(
            nearest_guest_package_json_module_mode(&mut kernel, pid, "/pkg/sub/main.js")
                .expect_err("invalid UTF-8 must fail")
                .code(),
            Some("EILSEQ")
        );

        kernel
            .write_file("/pkg/package.json", b"{".to_vec())
            .expect("write malformed package config");
        assert_eq!(
            nearest_guest_package_json_module_mode(&mut kernel, pid, "/pkg/sub/main.js")
                .expect_err("malformed JSON must fail")
                .code(),
            Some("ERR_INVALID_PACKAGE_CONFIG")
        );

        kernel
            .write_file("/pkg/package.json", br#"{"type":"module"}"#.to_vec())
            .expect("restore package config");
        kernel
            .chmod("/pkg/package.json", 0)
            .expect("deny package config read");
        assert_eq!(
            nearest_guest_package_json_module_mode(&mut kernel, pid, "/pkg/sub/main.js")
                .expect_err("package config DAC failure must propagate")
                .code(),
            Some("EACCES")
        );

        kernel
            .remove_file("/pkg/package.json")
            .expect("remove package config");
        kernel
            .symlink("/pkg/package-loop", "/pkg/package.json")
            .expect("create first package config loop link");
        kernel
            .symlink("/pkg/package.json", "/pkg/package-loop")
            .expect("create second package config loop link");
        assert_eq!(
            nearest_guest_package_json_module_mode(&mut kernel, pid, "/pkg/sub/main.js")
                .expect_err("package config symlink loop must propagate")
                .code(),
            Some("ELOOP")
        );
    }

    #[test]
    fn javascript_entrypoint_module_mode_uses_guest_package_scope() {
        let (mut kernel, pid) = package_json_test_kernel(128);
        kernel
            .write_file("/pkg/package.json", br#"{"type":"module"}"#.to_vec())
            .expect("write package config");

        assert_eq!(
            guest_javascript_entrypoint_module_mode_override(&mut kernel, pid, "/pkg/sub/main.js")
                .expect("detect package module mode"),
            Some(true)
        );
        assert_eq!(
            guest_javascript_entrypoint_module_mode_override(&mut kernel, pid, "/pkg/sub/main.mjs")
                .expect("detect extension module mode"),
            Some(true)
        );
        assert_eq!(
            guest_javascript_entrypoint_module_mode_override(&mut kernel, pid, "/pkg/sub/main.cjs")
                .expect("preserve CommonJS extension mode"),
            Some(false)
        );
        assert_eq!(
            guest_javascript_entrypoint_module_mode_override(&mut kernel, pid, "/unscoped/main.js")
                .expect("leave unscoped JavaScript unspecified"),
            None
        );
    }

    #[test]
    fn javascript_entrypoint_module_mode_uses_resolved_symlink_package_scope() {
        let (mut kernel, pid) = package_json_test_kernel(128);
        kernel
            .write_file("/package.json", br#"{"type":"module"}"#.to_vec())
            .expect("write root package config");
        kernel
            .mkdir("/store/tool/bin", true)
            .expect("create real package directory");
        kernel
            .write_file("/store/tool/package.json", b"{}".to_vec())
            .expect("write real package config without a type");
        kernel
            .write_file("/store/tool/bin/tool.js", b"module.exports = 1;\n".to_vec())
            .expect("write real CommonJS entrypoint");
        kernel
            .mkdir("/pkg/node_modules", true)
            .expect("create node_modules directory");
        kernel
            .symlink("/store/tool", "/pkg/node_modules/tool")
            .expect("create package symlink");

        let guest_entrypoint = String::from("/pkg/node_modules/tool/bin/tool.js");
        let resolved = kernel
            .realpath_for_process(EXECUTION_DRIVER_NAME, pid, &guest_entrypoint)
            .expect("resolve package symlink");
        assert_eq!(resolved, "/store/tool/bin/tool.js");
        assert_eq!(
            guest_javascript_entrypoint_module_mode_override(&mut kernel, pid, &resolved)
                .expect("classify resolved package entrypoint"),
            Some(false),
            "the real package scope must win over the module-typed project root"
        );
    }
}

/// Import a trusted caller-supplied host entrypoint once into the authoritative
/// kernel VFS. This is VM configuration/launch input, not a mutable host mount;
/// subsequent guest reads and writes never synchronize back to the host path.
#[derive(Debug)]
struct OpenHostLaunchSource {
    file: fs::File,
    path: PathBuf,
    observed_bytes: u64,
    mode: u32,
}

#[derive(Debug)]
struct HostLaunchSource {
    bytes: Vec<u8>,
    mode: u32,
}

fn host_launch_io_error(operation: &str, path: &Path, error: std::io::Error) -> SidecarError {
    let code = match error.raw_os_error() {
        Some(libc::EPERM) => "EPERM",
        Some(libc::ENOENT) => "ENOENT",
        Some(libc::EACCES) => "EACCES",
        Some(libc::ENOTDIR) => "ENOTDIR",
        Some(libc::EISDIR) => "EISDIR",
        Some(libc::ELOOP) => "ELOOP",
        _ => "EIO",
    };
    SidecarError::host(
        code,
        format!("{operation} host launch source {}: {error}", path.display()),
    )
}

fn open_host_launch_source(
    path: PathBuf,
    maximum_bytes: u64,
) -> Result<OpenHostLaunchSource, SidecarError> {
    let file = fs::File::open(&path).map_err(|error| host_launch_io_error("open", &path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| host_launch_io_error("stat", &path, error))?;
    if !metadata.is_file() {
        return Err(SidecarError::host(
            "EINVAL",
            format!(
                "host launch source {} is not a regular file",
                path.display()
            ),
        ));
    }
    admit_host_launch_source_bytes(&path, metadata.len(), Some(maximum_bytes))?;
    Ok(OpenHostLaunchSource {
        file,
        path,
        observed_bytes: metadata.len(),
        mode: metadata.permissions().mode() & 0o7777,
    })
}

fn host_launch_read_reservation(opened: &OpenHostLaunchSource) -> Result<usize, SidecarError> {
    usize::try_from(opened.observed_bytes)
        .ok()
        .and_then(|observed| observed.checked_add(1))
        .ok_or_else(|| {
            SidecarError::host(
                "E2BIG",
                format!(
                    "host launch source {} cannot fit in the host address space",
                    opened.path.display()
                ),
            )
        })
}

fn read_open_host_launch_source(
    opened: OpenHostLaunchSource,
    maximum_bytes: u64,
) -> Result<HostLaunchSource, SidecarError> {
    let capacity = usize::try_from(opened.observed_bytes).map_err(|_| {
        SidecarError::host(
            "E2BIG",
            format!(
                "host launch source {} cannot fit in the host address space",
                opened.path.display()
            ),
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut bounded = opened.file.take(opened.observed_bytes.saturating_add(1));
    bounded
        .read_to_end(&mut bytes)
        .map_err(|error| host_launch_io_error("read", &opened.path, error))?;
    if bytes.len() as u64 > opened.observed_bytes {
        return Err(SidecarError::host(
            "ESTALE",
            format!(
                "host launch source {} grew after admission metadata was captured; retry the launch",
                opened.path.display()
            ),
        ));
    }
    admit_host_launch_source_bytes(&opened.path, bytes.len() as u64, Some(maximum_bytes))?;
    Ok(HostLaunchSource {
        bytes,
        mode: opened.mode,
    })
}

async fn read_bounded_host_launch_source_async(
    vm: &VmState,
    path: PathBuf,
    maximum_bytes: u64,
) -> Result<HostLaunchSource, SidecarError> {
    let blocking = vm.runtime_context.blocking().clone();
    let path_reservation = path.to_string_lossy().len().saturating_add(1);
    let opened = blocking
        .run(path_reservation, move || {
            open_host_launch_source(path, maximum_bytes)
        })
        .await
        .map_err(SidecarError::from)??;
    let read_reservation = host_launch_read_reservation(&opened)?;
    blocking
        .run(read_reservation, move || {
            read_open_host_launch_source(opened, maximum_bytes)
        })
        .await
        .map_err(SidecarError::from)?
}

fn import_host_entrypoint_to_kernel(
    vm: &mut VmState,
    guest_entrypoint: &str,
    host_entrypoint: &Path,
    maximum_bytes: Option<u64>,
) -> Result<(), SidecarError> {
    // JavaScript guest-replacement paths still enter through synchronous
    // compatibility RPCs. Keep their unavoidable host file I/O on the same
    // fixed, bounded blocking executor; trusted initial WASM admission uses
    // the async counterpart above and never blocks a Tokio worker.
    let maximum_bytes = maximum_bytes.unwrap_or(vm.limits.wasm.max_module_file_bytes);
    let blocking = vm.runtime_context.blocking().clone();
    let timeout = vm.runtime_context.blocking_job_timeout();
    let host_entrypoint = host_entrypoint.to_path_buf();
    let path_reservation = host_entrypoint.to_string_lossy().len().saturating_add(1);
    let opened = blocking
        .run_sync(path_reservation, timeout, move || {
            open_host_launch_source(host_entrypoint, maximum_bytes)
        })
        .map_err(SidecarError::from)??;
    let read_reservation = host_launch_read_reservation(&opened)?;
    let source = blocking
        .run_sync(read_reservation, timeout, move || {
            read_open_host_launch_source(opened, maximum_bytes)
        })
        .map_err(SidecarError::from)??;
    match vm.kernel.admit_trusted_initial_runtime_image(
        guest_entrypoint,
        source.bytes,
        source.mode,
        maximum_bytes,
    ) {
        Ok(()) => Ok(()),
        // Kernel state wins if another trusted configuration path already
        // admitted the same guest entrypoint.
        Err(error) if error.code() == "EEXIST" => Ok(()),
        Err(error) => Err(kernel_error(error)),
    }
}

fn admit_host_launch_source_bytes(
    host_entrypoint: &Path,
    observed: u64,
    maximum: Option<u64>,
) -> Result<(), SidecarError> {
    let Some(maximum) = maximum else {
        return Ok(());
    };
    if observed > maximum {
        return Err(SidecarError::Host(
            HostServiceError::new(
                "E2BIG",
                format!(
                    "WASM launch source {} is {observed} bytes, exceeding limits.wasm.maxModuleFileBytes ({maximum})",
                    host_entrypoint.display(),
                ),
            )
            .with_details(json!({
                "limitName": "limits.wasm.maxModuleFileBytes",
                "limit": maximum,
                "requested": observed,
            })),
        ));
    }
    if observed >= maximum.saturating_mul(4) / 5 {
        eprintln!(
            "WARN_AGENTOS_WASM_LAUNCH_SOURCE_NEAR_LIMIT: path={} observed={} maximum={} limit=limits.wasm.maxModuleFileBytes",
            host_entrypoint.display(),
            observed,
            maximum,
        );
    }
    Ok(())
}

#[cfg(test)]
mod bounded_host_launch_source_tests {
    use super::{
        open_host_launch_source, read_open_host_launch_source, remove_existing_launch_asset,
    };
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "agentos-native-sidecar-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create launch source test directory");
        path
    }

    #[test]
    fn opened_launch_source_pins_the_admitted_inode_across_path_replacement() {
        let directory = temp_dir("launch-source-provenance");
        let source_path = directory.join("guest.wasm");
        let replacement_path = directory.join("replacement.wasm");
        fs::write(&source_path, b"original-image").expect("write original launch source");

        let opened =
            open_host_launch_source(source_path.clone(), 64).expect("open original launch source");
        fs::write(&replacement_path, b"replaced-image").expect("write replacement source");
        fs::rename(&replacement_path, &source_path).expect("replace launch source pathname");

        let admitted = read_open_host_launch_source(opened, 64)
            .expect("read the inode selected during admission");
        assert_eq!(admitted.bytes, b"original-image");
        assert_eq!(
            fs::read(&source_path).expect("read replacement pathname"),
            b"replaced-image"
        );

        fs::remove_dir_all(directory).expect("remove launch source test directory");
    }

    #[test]
    fn launch_source_growth_is_typed_estale_and_oversize_is_rejected_before_read() {
        let directory = temp_dir("launch-source-bounds");
        let source_path = directory.join("guest.wasm");
        fs::write(&source_path, b"abc").expect("write bounded launch source");
        let opened =
            open_host_launch_source(source_path.clone(), 3).expect("open bounded launch source");
        OpenOptions::new()
            .append(true)
            .open(&source_path)
            .expect("open launch source for growth")
            .write_all(b"d")
            .expect("grow launch source after metadata admission");
        let stale = read_open_host_launch_source(opened, 3)
            .expect_err("growth after admission metadata must be rejected");
        assert_eq!(stale.code(), Some("ESTALE"));

        let oversize_path = directory.join("oversize.wasm");
        fs::write(&oversize_path, b"abcde").expect("write oversized launch source");
        let oversize = open_host_launch_source(oversize_path, 4)
            .expect_err("oversized source must fail from handle metadata before reading");
        assert_eq!(oversize.code(), Some("E2BIG"));

        fs::remove_dir_all(directory).expect("remove launch source test directory");
    }

    #[test]
    fn replacing_stale_launch_asset_never_follows_its_final_symlink() {
        let directory = temp_dir("launch-asset-replacement");
        let target = directory.join("target.js");
        let asset = directory.join("asset.js");
        fs::write(&target, b"preserve me").expect("write symlink target");
        std::os::unix::fs::symlink(&target, &asset).expect("create stale asset symlink");

        remove_existing_launch_asset(&asset).expect("remove stale launch asset");
        assert!(fs::symlink_metadata(&asset).is_err());
        assert_eq!(
            fs::read(&target).expect("read preserved target"),
            b"preserve me"
        );

        fs::create_dir_all(asset.join("nested")).expect("create stale asset directory");
        fs::write(asset.join("nested/file"), b"stale").expect("write stale nested asset");
        remove_existing_launch_asset(&asset).expect("remove stale launch asset directory");
        assert!(!asset.exists());

        fs::remove_dir_all(directory).expect("remove launch asset test directory");
    }
}

fn materialize_guest_launch_asset(
    vm: &mut VmState,
    guest_path: &str,
    authority: WasmLaunchAuthority,
    prepared_source: Option<&str>,
) -> Result<(), SidecarError> {
    let stat = match authority {
        WasmLaunchAuthority::TrustedInitialImage => vm.kernel.lstat(guest_path),
        WasmLaunchAuthority::GuestProcessImage { requester_pid } => {
            vm.kernel
                .lstat_for_process(EXECUTION_DRIVER_NAME, requester_pid, guest_path)
        }
    }
    .map_err(kernel_error)?;
    let asset_path = runtime_asset_path_for_guest(vm, guest_path);
    remove_existing_launch_asset(&asset_path)?;

    if stat.is_symbolic_link {
        if let Some(parent) = asset_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to create launch-asset symlink parent: {error}"
                ))
            })?;
        }
        let target = match authority {
            WasmLaunchAuthority::TrustedInitialImage => vm.kernel.read_link(guest_path),
            WasmLaunchAuthority::GuestProcessImage { requester_pid } => vm
                .kernel
                .read_link_for_process(EXECUTION_DRIVER_NAME, requester_pid, guest_path),
        }
        .map_err(kernel_error)?;
        std::os::unix::fs::symlink(&target, &asset_path).map_err(|error| {
            SidecarError::Io(format!("failed to stage launch symlink: {error}"))
        })?;
        return Ok(());
    }

    if stat.is_directory {
        fs::create_dir_all(&asset_path).map_err(|error| {
            SidecarError::Io(format!("failed to create launch-asset directory: {error}"))
        })?;
        fs::set_permissions(&asset_path, fs::Permissions::from_mode(stat.mode & 0o7777)).map_err(
            |error| {
                SidecarError::Io(format!(
                    "failed to set launch-asset directory mode on {}: {error}",
                    asset_path.display()
                ))
            },
        )?;
        return Ok(());
    }

    if let Some(parent) = asset_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!("failed to create launch-asset parent: {error}"))
        })?;
    }
    let owned_bytes;
    let bytes = if let Some(source) = prepared_source {
        source.as_bytes()
    } else {
        owned_bytes = match authority {
            WasmLaunchAuthority::TrustedInitialImage => {
                vm.kernel
                    .load_trusted_initial_runtime_image(
                        guest_path,
                        vm.limits.wasm.max_module_file_bytes,
                    )
                    .map_err(kernel_error)?
                    .bytes
            }
            WasmLaunchAuthority::GuestProcessImage { requester_pid } => vm
                .kernel
                .read_file_for_process(EXECUTION_DRIVER_NAME, requester_pid, guest_path)
                .map_err(kernel_error)?,
        };
        owned_bytes.as_slice()
    };
    fs::write(&asset_path, bytes).map_err(|error| {
        SidecarError::Io(format!("failed to stage guest launch asset: {error}"))
    })?;
    fs::set_permissions(&asset_path, fs::Permissions::from_mode(stat.mode & 0o7777)).map_err(
        |error| {
            SidecarError::Io(format!(
                "failed to set launch-asset file mode on {}: {error}",
                asset_path.display()
            ))
        },
    )?;
    Ok(())
}

/// Remove an old projection without following its final symlink. A guest may
/// replace an entrypoint between launches (symlink -> file, file -> directory,
/// and so on); every new projection must replace the old inode before writing
/// so host `fs::write`/`create_dir_all` cannot follow stale projection state.
fn remove_existing_launch_asset(asset_path: &Path) -> Result<(), SidecarError> {
    match fs::symlink_metadata(asset_path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(asset_path).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to replace launch-asset directory {}: {error}",
                    asset_path.display()
                ))
            })
        }
        Ok(_) => fs::remove_file(asset_path).map_err(|error| {
            SidecarError::Io(format!(
                "failed to replace launch-asset file {}: {error}",
                asset_path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SidecarError::Io(format!(
            "failed to inspect existing launch asset {}: {error}",
            asset_path.display()
        ))),
    }
}

pub(super) fn load_javascript_entrypoint_source(
    vm: &mut VmState,
    kernel_pid: u32,
    guest_cwd: &str,
    entrypoint: &str,
    env: &BTreeMap<String, String>,
) -> Result<Option<String>, SidecarError> {
    let mut read_guest_file = |path: &str| {
        let bytes = match vm
            .kernel
            .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
        {
            Ok(bytes) => bytes,
            Err(error) if matches!(error.code(), "ENOENT" | "ENOTDIR") => return Ok(None),
            Err(error) => return Err(kernel_error(error)),
        };
        String::from_utf8(bytes).map(Some).map_err(|error| {
            SidecarError::host(
                "EILSEQ",
                format!("JavaScript entrypoint {path} is not valid UTF-8: {error}"),
            )
        })
    };

    if let Some(path) = env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
    {
        if let Some(source) = read_guest_file(path)? {
            return Ok(Some(source));
        }
    }

    if entrypoint.starts_with('/') {
        return read_guest_file(entrypoint);
    }
    read_guest_file(&normalize_path(&format!("{guest_cwd}/{entrypoint}")))
}

pub(super) fn python_file_entrypoint(entrypoint: &str) -> Option<PathBuf> {
    let path = Path::new(entrypoint);
    (path.extension().and_then(|extension| extension.to_str()) == Some("py"))
        .then(|| path.to_path_buf())
}

pub(super) fn add_runtime_guest_path_mapping(
    env: &mut BTreeMap<String, String>,
    guest_path: &str,
    host_path: &Path,
) {
    let mut mappings = env
        .get("AGENTOS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value).ok())
        .unwrap_or_default();
    mappings.retain(|mapping| {
        mapping
            .get("guestPath")
            .and_then(Value::as_str)
            .map(|existing| normalize_path(existing) != normalize_path(guest_path))
            .unwrap_or(true)
    });
    mappings.push(json!({
        "guestPath": normalize_path(guest_path),
        "hostPath": host_path.display().to_string(),
    }));
    if let Ok(serialized) = serde_json::to_string(&mappings) {
        env.insert(String::from("AGENTOS_GUEST_PATH_MAPPINGS"), serialized);
    }
}

pub(super) fn add_runtime_host_access_path(
    env: &mut BTreeMap<String, String>,
    key: &str,
    host_path: &Path,
    expand: bool,
) {
    let existing = env
        .get(key)
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let mut paths = existing;
    paths.push(host_path.to_path_buf());
    let normalized = if expand {
        expand_host_access_paths(&paths)
    } else {
        dedupe_host_paths(&paths)
    };
    let serialized = normalized
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if let Ok(serialized) = serde_json::to_string(&serialized) {
        env.insert(key.to_owned(), serialized);
    }
}

pub(super) fn is_path_like_specifier(specifier: &str) -> bool {
    specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with("file:")
}

pub(super) fn kernel_process_permission_tier(tier: WasmPermissionTier) -> ProcessPermissionTier {
    match tier {
        WasmPermissionTier::Full => ProcessPermissionTier::Full,
        WasmPermissionTier::ReadWrite => ProcessPermissionTier::ReadWrite,
        WasmPermissionTier::ReadOnly => ProcessPermissionTier::ReadOnly,
        WasmPermissionTier::Isolated => ProcessPermissionTier::Isolated,
    }
}

pub(super) fn execution_wasm_permission_tier(
    tier: ProcessPermissionTier,
) -> ExecutionWasmPermissionTier {
    match tier {
        ProcessPermissionTier::Full => ExecutionWasmPermissionTier::Full,
        ProcessPermissionTier::ReadWrite => ExecutionWasmPermissionTier::ReadWrite,
        ProcessPermissionTier::ReadOnly => ExecutionWasmPermissionTier::ReadOnly,
        ProcessPermissionTier::Isolated => ExecutionWasmPermissionTier::Isolated,
    }
}

pub(super) fn tokenize_shell_free_command(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn is_posix_shell_builtin(command: &str) -> bool {
    matches!(
        command,
        "." | ":"
            | "break"
            | "cd"
            | "continue"
            | "eval"
            | "exec"
            | "exit"
            | "export"
            | "readonly"
            | "return"
            | "set"
            | "shift"
            | "times"
            | "trap"
            | "umask"
            | "unset"
    )
}

/// Single-token checks for shell-mode commands whose first word forces a real
/// shell even when the command string has no shell metacharacters. This is not
/// a parser: env-assignment prefixes (`FOO=bar cmd`) and shell reserved words
/// have no meaning outside `sh`, so whitespace-tokenizing them would silently
/// run the wrong program.
pub(super) fn shell_first_token_requires_shell(token: &str) -> bool {
    token.contains('=') || is_shell_reserved_word(token)
}

fn is_shell_reserved_word(token: &str) -> bool {
    matches!(
        token,
        "if" | "then"
            | "elif"
            | "else"
            | "fi"
            | "for"
            | "in"
            | "do"
            | "done"
            | "while"
            | "until"
            | "case"
            | "esac"
            | "{"
            | "}"
            | "!"
    )
}

pub(super) fn command_requires_shell(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '&'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '$'
                | '`'
                | '*'
                | '?'
                | '['
                | ']'
                | '{'
                | '}'
                | '~'
                | '\''
                | '"'
                | '\\'
                | '\n'
        )
    })
}

fn host_mount_path_for_guest_path(vm: &VmState, guest_path: &str) -> Option<PathBuf> {
    let normalized = normalize_path(guest_path);

    let mut mounts = vm
        .configuration
        .mounts
        .iter()
        .filter_map(|mount| {
            ((mount.plugin.id == "host_dir") || (mount.plugin.id == "module_access"))
                .then(|| {
                    mount_config_host_backing_path(&mount.plugin.config)
                        .map(|host_path| (mount.guest_path.as_str(), host_path))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    mounts.sort_by_key(|mount| std::cmp::Reverse(mount.0.len()));

    for (guest_root, host_root) in mounts {
        if normalized != guest_root && !normalized.starts_with(&format!("{guest_root}/")) {
            continue;
        }

        let suffix = normalized
            .strip_prefix(guest_root)
            .unwrap_or_default()
            .trim_start_matches('/');
        let mut path = PathBuf::from(host_root);
        if !suffix.is_empty() {
            path.push(suffix);
        }
        return Some(path);
    }

    None
}

pub(super) fn host_runtime_path_for_guest_path_with_env(
    vm: &VmState,
    runtime_env: &BTreeMap<String, String>,
    guest_path: &str,
    default_host_cwd: &Path,
) -> Option<PathBuf> {
    if let Some(path) = host_mount_path_for_guest_path(vm, guest_path) {
        return Some(path);
    }
    if let Some(path) = host_path_from_runtime_guest_mappings(runtime_env, guest_path) {
        return Some(path);
    }

    let normalized = normalize_path(guest_path);
    let virtual_home = guest_virtual_home(vm);

    if normalized == virtual_home || normalized.starts_with(&format!("{virtual_home}/")) {
        let suffix = normalized
            .strip_prefix(&virtual_home)
            .unwrap_or_default()
            .trim_start_matches('/');
        let mut host_path = default_host_cwd.to_path_buf();
        if !suffix.is_empty() {
            host_path.push(suffix);
        }
        return Some(host_path);
    }

    None
}

#[derive(Deserialize, Serialize)]
struct RuntimeGuestPathMapping {
    #[serde(rename = "guestPath")]
    guest_path: String,
    #[serde(rename = "hostPath")]
    host_path: String,
    #[serde(rename = "readOnly", default)]
    read_only: bool,
}

pub(crate) fn host_path_from_runtime_guest_mappings(
    runtime_env: &BTreeMap<String, String>,
    guest_path: &str,
) -> Option<PathBuf> {
    let mappings = runtime_env
        .get("AGENTOS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<RuntimeGuestPathMapping>>(value).ok())?;
    let normalized = normalize_path(guest_path);

    let mut sorted_mappings = mappings
        .into_iter()
        .filter_map(|mapping| {
            (!mapping.guest_path.is_empty() && !mapping.host_path.is_empty()).then_some((
                normalize_path(&mapping.guest_path),
                PathBuf::from(mapping.host_path),
            ))
        })
        .collect::<Vec<_>>();
    sorted_mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.0.len()));

    for (guest_root, mut host_root) in sorted_mappings {
        if guest_root != "/"
            && normalized != guest_root
            && !normalized.starts_with(&format!("{guest_root}/"))
        {
            continue;
        }
        if guest_root == "/" && !normalized.starts_with('/') {
            continue;
        }

        if host_root.is_relative() {
            host_root = std::env::current_dir().ok()?.join(host_root);
        }

        let suffix = if guest_root == "/" {
            normalized.trim_start_matches('/')
        } else {
            normalized
                .strip_prefix(&guest_root)
                .unwrap_or_default()
                .trim_start_matches('/')
        };
        if !suffix.is_empty() {
            host_root.push(suffix);
        }
        return Some(host_root);
    }

    None
}

pub(super) fn host_mount_path_for_guest_path_from_mounts(
    mounts: &[crate::protocol::MountDescriptor],
    guest_path: &str,
) -> Option<PathBuf> {
    let normalized = normalize_path(guest_path);

    let mut host_mounts = mounts
        .iter()
        .filter_map(|mount| {
            ((mount.plugin.id == "host_dir") || (mount.plugin.id == "module_access"))
                .then(|| {
                    mount_config_host_backing_path(&mount.plugin.config)
                        .map(|host_path| (mount.guest_path.as_str(), host_path))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    host_mounts.sort_by_key(|mount| std::cmp::Reverse(mount.0.len()));

    for (guest_root, host_root) in host_mounts {
        if normalized != guest_root && !normalized.starts_with(&format!("{guest_root}/")) {
            continue;
        }

        let suffix = normalized
            .strip_prefix(guest_root)
            .unwrap_or_default()
            .trim_start_matches('/');
        let mut path = PathBuf::from(host_root);
        if !suffix.is_empty() {
            path.push(suffix);
        }
        return Some(path);
    }

    None
}

#[cfg(test)]
mod host_mount_path_for_guest_path_from_mounts_tests {
    use super::host_mount_path_for_guest_path_from_mounts;
    use crate::protocol::{MountDescriptor, MountPluginDescriptor};
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn resolves_module_access_mount_paths() {
        let mounts = vec![MountDescriptor {
            guest_path: String::from("/root/node_modules"),
            guest_source: String::from("module_access"),
            guest_fstype: String::from("module_access"),
            read_only: true,
            plugin: MountPluginDescriptor {
                id: String::from("module_access"),
                config: json!({
                    "hostPath": "/tmp/workspace/node_modules",
                })
                .to_string(),
            },
        }];

        let resolved =
            host_mount_path_for_guest_path_from_mounts(&mounts, "/root/node_modules/pkg/index.js")
                .expect("module_access mount should resolve");

        assert_eq!(
            resolved,
            PathBuf::from("/tmp/workspace/node_modules/pkg/index.js")
        );
    }

    #[test]
    fn does_not_resolve_agentos_packages_as_host_paths() {
        let mounts = vec![MountDescriptor {
            guest_path: String::from("/opt/agentos/bin/pi"),
            guest_source: String::from("agentos_packages"),
            guest_fstype: String::from("agentos_packages"),
            read_only: true,
            plugin: MountPluginDescriptor {
                id: String::from("agentos_packages"),
                config: json!({
                    "kind": "singleSymlink",
                    "target": "../pkgs/pi/current/bin/pi",
                })
                .to_string(),
            },
        }];

        assert!(
            host_mount_path_for_guest_path_from_mounts(&mounts, "/opt/agentos/bin/pi").is_none()
        );
    }
}

pub(super) fn resolve_guest_socket_host_path(
    context: &SocketPathContext,
    guest_path: &str,
) -> PathBuf {
    if let Some(path) = host_mount_path_for_guest_path_from_mounts(&context.mounts, guest_path) {
        return path;
    }

    let normalized = normalize_path(guest_path);
    let mut host_path = context.sandbox_root.clone();
    let suffix = normalized.trim_start_matches('/');
    if !suffix.is_empty() {
        host_path.push(suffix);
    }
    host_path
}

// ProcessLaunchOptions and ProcessLaunchRequest live in the runtime-neutral
// agentos-execution host contract.
// ResolvedChildProcessExecution moved to crate::state

pub(crate) fn sanitize_javascript_child_process_internal_bootstrap_env(
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    const ALLOWED_KEYS: &[&str] = &[
        "AGENTOS_ALLOWED_NODE_BUILTINS",
        "AGENTOS_GUEST_PATH_MAPPINGS",
        "AGENTOS_LOOPBACK_EXEMPT_PORTS",
        "AGENTOS_VIRTUAL_PROCESS_EXEC_PATH",
        "AGENTOS_VIRTUAL_PROCESS_UID",
        "AGENTOS_VIRTUAL_PROCESS_GID",
        "AGENTOS_VIRTUAL_PROCESS_VERSION",
    ];

    env.iter()
        .filter(|(key, _)| {
            ALLOWED_KEYS.contains(&key.as_str()) || key.starts_with("AGENTOS_VIRTUAL_OS_")
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn rollback_failed_top_level_process_start(
    kernel: &mut SidecarKernel,
    kernel_handle: &agentos_kernel::kernel::KernelProcessHandle,
    execution: Option<&mut ActiveExecution>,
    context: &str,
) {
    if let Some(execution) = execution {
        if let Err(error) = execution.terminate() {
            eprintln!(
                "[agentos] failed to terminate rejected {context} runtime for PID {}: {error}",
                kernel_handle.pid()
            );
        }
    }
    kernel_handle.finish(127);
    if let Err(error) = kernel.waitpid(kernel_handle.pid()) {
        eprintln!(
            "[agentos] failed to reap rejected {context} kernel PID {}: {error}",
            kernel_handle.pid()
        );
    }
}

fn rollback_published_top_level_process_start(vm: &mut VmState, process_id: &str, context: &str) {
    let Some(mut process) = vm.active_processes.remove(process_id) else {
        eprintln!("[agentos] failed to find rejected {context} process {process_id} for rollback");
        return;
    };
    let kernel_handle = process.kernel_handle.clone();
    rollback_failed_top_level_process_start(
        &mut vm.kernel,
        &kernel_handle,
        Some(&mut process.execution),
        context,
    );
}

enum StartedTopLevelAdapterContext {
    Javascript(String),
    Python(String),
    WebAssembly(String),
}

// Network request types moved to crate::protocol

// VmDnsConfig, DnsResolutionSource moved to crate::state

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) async fn execute(
        &mut self,
        request: &RequestFrame,
        payload: ExecuteRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let execute_total_start = Instant::now();
        let process_event_capacity = self.config.runtime.protocol.max_process_events;
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| missing_vm_error(&vm_id))?;
        if vm.active_processes.contains_key(&payload.process_id) {
            return Err(SidecarError::InvalidState(format!(
                "VM {vm_id} already has an active process with id {}",
                payload.process_id
            )));
        }
        // ConfigureVm normally closes the trusted bootstrap window after
        // projecting package command stubs. Legacy/create-only callers can
        // execute without ConfigureVm, so seal here as a final boundary before
        // any untrusted guest code can observe a writable read-only root.
        vm.kernel
            .finish_root_filesystem_bootstrap()
            .map_err(kernel_error)?;
        let vm_pending_stdin_bytes_budget = Arc::clone(&vm.pending_stdin_bytes_budget);
        let vm_pending_event_bytes_budget = Arc::clone(&vm.pending_event_bytes_budget);
        let standalone_wasm_backend = match payload.wasm_backend {
            Some(StandaloneWasmBackend::V8) => ExecutionStandaloneWasmBackend::V8,
            Some(StandaloneWasmBackend::Wasmtime) => ExecutionStandaloneWasmBackend::Wasmtime,
            Some(StandaloneWasmBackend::WasmtimeThreads) => {
                ExecutionStandaloneWasmBackend::WasmtimeThreads
            }
            None => vm.standalone_wasm_backend,
        };

        if let Some(command) = payload.command.as_deref() {
            if let Some(binding_resolution) =
                resolve_binding_command(vm, command, &payload.args, payload.cwd.as_deref())?
            {
                let guest_cwd = payload
                    .cwd
                    .as_deref()
                    .map(normalize_path)
                    .unwrap_or_else(|| vm.guest_cwd.clone());
                let kernel_handle = vm
                    .kernel
                    .create_virtual_process(
                        EXECUTION_DRIVER_NAME,
                        BINDING_DRIVER_NAME,
                        command,
                        std::iter::once(command.to_owned())
                            .chain(payload.args.iter().cloned())
                            .collect(),
                        VirtualProcessOptions {
                            env: vm.guest_env.clone(),
                            cwd: Some(guest_cwd.clone()),
                            ..VirtualProcessOptions::default()
                        },
                    )
                    .map_err(kernel_error)?;
                let kernel_pid = kernel_handle.pid();
                let runtime_control = match ActiveProcess::attach_runtime_control_before_start(
                    &kernel_handle,
                    Arc::clone(&self.process_event_notify),
                ) {
                    Ok(runtime_control) => runtime_control,
                    Err(error) => {
                        rollback_failed_top_level_process_start(
                            &mut vm.kernel,
                            &kernel_handle,
                            None,
                            "top-level binding runtime-control attachment",
                        );
                        return Err(error);
                    }
                };
                let binding_execution = BindingExecution::with_event_notify(
                    Arc::clone(&self.process_event_notify),
                    process_event_capacity,
                )
                .with_vm_pending_event_bytes_budget(Arc::clone(&vm_pending_event_bytes_budget));
                let cancelled = binding_execution.cancelled.clone();
                let paused = Arc::clone(&binding_execution.paused);
                let pause_notify = Arc::clone(&binding_execution.pause_notify);
                let pending_events = binding_execution.pending_events.clone();
                let event_overflow_reason = binding_execution.event_overflow_reason.clone();
                let pending_event_bytes = binding_execution.pending_event_bytes.clone();
                let pending_event_count_limit = binding_execution.pending_event_count_limit.clone();
                let pending_event_bytes_limit = binding_execution.pending_event_bytes_limit.clone();
                let binding_vm_pending_event_bytes_budget =
                    binding_execution.vm_pending_event_bytes_budget.clone();
                let event_notify = binding_execution.event_notify.clone();
                let host_cwd = runtime_launch_path_for_guest(vm, &guest_cwd);
                let mut process = ActiveProcess::new_with_attached_runtime_control(
                    kernel_pid,
                    kernel_handle,
                    vm.runtime_context.clone(),
                    vm.limits.clone(),
                    process_event_capacity,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Binding(binding_execution),
                    runtime_control,
                    Arc::clone(&self.process_event_notify),
                )
                .with_adapter_policy(ExecutionAdapterPolicy::BINDING)
                .with_standalone_wasm_backend(standalone_wasm_backend)
                .with_vm_pending_byte_budgets(
                    Arc::clone(&vm_pending_stdin_bytes_budget),
                    Arc::clone(&vm_pending_event_bytes_budget),
                )
                .with_guest_cwd(guest_cwd.clone())
                .with_host_cwd(host_cwd);
                if let Err(error) = process.apply_runtime_controls() {
                    let rollback_handle = process.kernel_handle.clone();
                    rollback_failed_top_level_process_start(
                        &mut vm.kernel,
                        &rollback_handle,
                        Some(&mut process.execution),
                        "top-level binding pending runtime control",
                    );
                    return Err(error);
                }
                vm.active_processes
                    .insert(payload.process_id.clone(), process);
                // Registration is the publication boundary for an execution
                // that may already have queued work. Never rely solely on a
                // pre-publication executor wake.
                self.process_event_notify.notify_one();
                if let Err(error) = self.bridge.emit_lifecycle(&vm_id, LifecycleState::Busy) {
                    rollback_published_top_level_process_start(
                        vm,
                        &payload.process_id,
                        "top-level binding lifecycle publication",
                    );
                    return Err(error);
                }
                spawn_binding_process_events(BindingProcessEventRequest {
                    runtime_context: vm.runtime_context.clone(),
                    sidecar_requests: self.sidecar_requests.clone(),
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    vm_id: vm_id.clone(),
                    binding_resolution,
                    cancelled,
                    paused,
                    pause_notify,
                    pending_events,
                    event_overflow_reason,
                    pending_event_bytes,
                    pending_event_count_limit,
                    pending_event_bytes_limit,
                    vm_pending_event_bytes_budget: binding_vm_pending_event_bytes_budget,
                    event_notify,
                });
                return Ok(DispatchResult {
                    response: process_started_response(
                        request,
                        payload.process_id,
                        Some(kernel_pid),
                    ),
                    events: Vec::new(),
                });
            }
        }

        let requested_tty = payload
            .env
            .get(EXECUTION_REQUEST_TTY_ENV)
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
        let phase_start = Instant::now();
        let mut resolved = resolve_execute_request(vm, &payload)?;
        stage_agentos_package_command(vm, &mut resolved, WasmLaunchAuthority::TrustedInitialImage)?;
        admit_trusted_initial_wasm_source_if_missing(vm, &resolved).await?;
        stage_kernel_wasm_launch_asset(
            vm,
            &mut resolved,
            WasmLaunchAuthority::TrustedInitialImage,
        )?;
        let resolved = resolved;
        record_execute_phase("resolve_execute_request", phase_start.elapsed());
        let phase_start = Instant::now();
        let mut env = resolved.env.clone();
        env.remove(EXECUTION_REQUEST_TTY_ENV);
        env.insert(
            String::from(EXECUTION_SANDBOX_ROOT_ENV),
            normalize_host_path(&vm.runtime_scratch_root)
                .to_string_lossy()
                .into_owned(),
        );
        if resolved.adapter_policy.forwards_kernel_stdin_rpc {
            env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
            // Managed V8 reads fd 0 through the sidecar's kernel bridge. The
            // execution crate keeps its local bridge only for standalone use.
            env.insert(
                String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                String::from("1"),
            );
        } else if resolved.adapter_policy.encodes_inherited_fd_bootstrap {
            env.insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
        }
        if resolved.adapter_policy.supports_prepared_in_place_exec {
            env.insert(String::from(WASM_EXEC_COMMIT_RPC_ENV), String::from("1"));
        }
        let provisional_launch_entrypoint = if resolved
            .adapter_policy
            .uses_javascript_entrypoint_projection
        {
            env.get("AGENTOS_GUEST_ENTRYPOINT")
                .filter(|path| path.starts_with('/'))
                .map(|path| normalize_path(path))
                .unwrap_or_else(|| resolved.entrypoint.clone())
        } else {
            resolved.entrypoint.clone()
        };
        let argv = std::iter::once(provisional_launch_entrypoint)
            .chain(resolved.execution_args.iter().cloned())
            .collect::<Vec<_>>();
        let requested_permission_tier = resolved
            .wasm_permission_tier
            .map(kernel_process_permission_tier)
            .unwrap_or(ProcessPermissionTier::Full);
        record_execute_phase("env_argv_setup", phase_start.elapsed());
        let phase_start = Instant::now();
        let kernel_handle = vm
            .kernel
            .spawn_process(
                &resolved.command,
                argv,
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    cwd: Some(resolved.guest_cwd.clone()),
                    permission_tier: Some(requested_permission_tier),
                    ..SpawnOptions::default()
                },
            )
            .map_err(kernel_error)?;
        let kernel_pid = kernel_handle.pid();
        record_execute_phase("kernel_spawn_process", phase_start.elapsed());

        macro_rules! top_level_start_step {
            ($result:expr, $context:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        rollback_failed_top_level_process_start(
                            &mut vm.kernel,
                            &kernel_handle,
                            None,
                            $context,
                        );
                        return Err(error);
                    }
                }
            };
        }

        macro_rules! dispose_started_context {
            ($context:expr) => {
                match $context {
                    StartedTopLevelAdapterContext::Javascript(context_id) => {
                        self.javascript_engine.dispose_context(context_id);
                    }
                    StartedTopLevelAdapterContext::Python(context_id) => {
                        self.python_engine.dispose_context(context_id);
                    }
                    StartedTopLevelAdapterContext::WebAssembly(context_id) => {
                        self.wasm_engine.dispose_context(context_id);
                    }
                }
            };
        }

        let launch_entrypoint = if resolved
            .adapter_policy
            .uses_javascript_entrypoint_projection
        {
            top_level_start_step!(
                resolve_agentos_package_javascript_launch_entrypoint(vm, kernel_pid, &mut env,),
                "top-level JavaScript package entrypoint resolution"
            )
            .unwrap_or_else(|| resolved.entrypoint.clone())
        } else {
            resolved.entrypoint.clone()
        };

        // Attach before PTY setup, asset preparation, or engine start. Kernel
        // signals arriving during any of those steps remain durable in this
        // receiver; every failure below funnels through process reaping.
        let runtime_control = top_level_start_step!(
            ActiveProcess::attach_runtime_control_before_start(
                &kernel_handle,
                Arc::clone(&self.process_event_notify),
            ),
            "top-level runtime-control attachment"
        );
        if resolved.runtime == GuestRuntimeKind::WebAssembly {
            top_level_start_step!(
                vm.kernel
                    .initialize_canonical_wasi_preopens(EXECUTION_DRIVER_NAME, kernel_pid)
                    .map_err(kernel_error),
                "top-level WASI capability-root initialization"
            );
        }
        let tty_master_fd = if requested_tty {
            let (master_fd, slave_fd, _) = top_level_start_step!(
                vm.kernel
                    .open_pty(EXECUTION_DRIVER_NAME, kernel_pid)
                    .map_err(kernel_error),
                "top-level PTY allocation"
            );
            top_level_start_step!(
                vm.kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd, 0)
                    .map_err(kernel_error),
                "top-level PTY stdin installation"
            );
            top_level_start_step!(
                vm.kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd, 1)
                    .map_err(kernel_error),
                "top-level PTY stdout installation"
            );
            top_level_start_step!(
                vm.kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd, 2)
                    .map_err(kernel_error),
                "top-level PTY stderr installation"
            );
            top_level_start_step!(
                vm.kernel
                    .pty_set_foreground_pgid(
                        EXECUTION_DRIVER_NAME,
                        kernel_pid,
                        master_fd,
                        kernel_pid,
                    )
                    .map_err(kernel_error),
                "top-level PTY foreground-group setup"
            );
            if let Some((cols, rows)) = requested_pty_window_size(&env) {
                top_level_start_step!(
                    vm.kernel
                        .pty_resize(EXECUTION_DRIVER_NAME, kernel_pid, master_fd, cols, rows)
                        .map_err(kernel_error),
                    "top-level PTY resize"
                );
            }
            Some(master_fd)
        } else {
            None
        };
        let kernel_stdin_writer_fd = if let Some(master_fd) = tty_master_fd {
            master_fd
        } else {
            top_level_start_step!(
                install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid),
                "top-level stdin pipe installation"
            )
        };

        let (execution, process_env, started_context) = match resolved.runtime {
            GuestRuntimeKind::JavaScript => {
                let phase_start = Instant::now();
                top_level_start_step!(
                    prepare_javascript_launch_assets(
                        vm,
                        &resolved,
                        &env,
                        WasmLaunchAuthority::TrustedInitialImage,
                        None,
                    ),
                    "top-level JavaScript asset preparation"
                );
                record_execute_phase("js_prepare_launch_assets", phase_start.elapsed());
                let phase_start = Instant::now();
                // A trusted initial request may name a host source that has not
                // been admitted to the kernel VFS yet. Asset preparation above
                // performs that one bounded admission. Load the executable
                // source only after admission so the kernel remains the source
                // of truth and the V8 import cache never falls back to the
                // caller's ambient host pathname.
                let inline_code = top_level_start_step!(
                    load_javascript_entrypoint_source(
                        vm,
                        kernel_pid,
                        &resolved.guest_cwd,
                        &launch_entrypoint,
                        &env,
                    ),
                    "top-level JavaScript entrypoint load"
                );
                record_execute_phase("js_load_entrypoint_source", phase_start.elapsed());

                let phase_start = Instant::now();
                let context =
                    self.javascript_engine
                        .create_context(CreateJavascriptContextRequest {
                            vm_id: vm_id.clone(),
                            bootstrap_module: None,
                            compile_cache_root: Some(self.cache_root.join("node-compile-cache")),
                        });
                record_execute_phase("js_create_context", phase_start.elapsed());
                let phase_start = Instant::now();
                let context_id = context.context_id;
                let execution = match self
                    .javascript_engine
                    .start_execution_with_module_reader_and_runtime(
                        StartJavascriptExecutionRequest {
                            guest_runtime: guest_runtime_identity(vm, None, None),
                            vm_id: vm_id.clone(),
                            context_id: context_id.clone(),
                            argv: std::iter::once(launch_entrypoint.clone())
                                .chain(resolved.execution_args.iter().cloned())
                                .collect(),
                            argv0: None,
                            env: env.clone(),
                            cwd: resolved.host_cwd.clone(),
                            limits: javascript_execution_limits(vm),
                            inline_code,
                            wasm_module_bytes: None,
                        },
                        None,
                        None,
                        vm.runtime_context.clone(),
                    )
                    .map_err(javascript_error)
                {
                    Ok(execution) => execution,
                    Err(error) => {
                        self.javascript_engine.dispose_context(&context_id);
                        rollback_failed_top_level_process_start(
                            &mut vm.kernel,
                            &kernel_handle,
                            None,
                            "top-level JavaScript engine start",
                        );
                        return Err(error);
                    }
                };
                record_execute_phase("js_start_execution", phase_start.elapsed());
                (
                    ActiveExecution::Javascript(execution),
                    env.clone(),
                    StartedTopLevelAdapterContext::Javascript(context_id),
                )
            }
            GuestRuntimeKind::Python => {
                // The `python` command path (marked by AGENTOS_PYTHON_ARGV) is
                // explicit about file mode via AGENTOS_PYTHON_FILE, so a `-c` code
                // string that happens to end in `.py` is never mistaken for a path.
                // The low-level execute API keeps the `.py`-suffix heuristic.
                let python_file_path = if resolved.env.contains_key("AGENTOS_PYTHON_ARGV") {
                    resolved.env.get("AGENTOS_PYTHON_FILE").map(PathBuf::from)
                } else {
                    python_file_entrypoint(&resolved.entrypoint)
                };
                let pyodide_dist_path = top_level_start_step!(
                    self.python_engine
                        .bundled_pyodide_dist_path_for_vm_async(&vm_id, &vm.runtime_context)
                        .await
                        .map_err(python_error),
                    "top-level Python asset preparation"
                );
                let pyodide_cache_path = pyodide_dist_path
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(pyodide_dist_path.as_path())
                    .join("pyodide-package-cache");
                add_runtime_guest_path_mapping(
                    &mut env,
                    PYTHON_PYODIDE_GUEST_ROOT,
                    &pyodide_dist_path,
                );
                add_runtime_guest_path_mapping(
                    &mut env,
                    PYTHON_PYODIDE_CACHE_GUEST_ROOT,
                    &pyodide_cache_path,
                );
                add_runtime_host_access_path(
                    &mut env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &pyodide_dist_path,
                    true,
                );
                add_runtime_host_access_path(
                    &mut env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &pyodide_cache_path,
                    true,
                );
                add_runtime_host_access_path(
                    &mut env,
                    "AGENTOS_EXTRA_FS_WRITE_PATHS",
                    &pyodide_cache_path,
                    false,
                );
                let context = self
                    .python_engine
                    .create_context(CreatePythonContextRequest {
                        vm_id: vm_id.clone(),
                        pyodide_dist_path,
                    });
                let context_id = context.context_id;
                let execution = match self
                    .python_engine
                    .start_execution_with_runtime_async(
                        StartPythonExecutionRequest {
                            vm_id: vm_id.clone(),
                            context_id: context_id.clone(),
                            code: resolved.entrypoint.clone(),
                            file_path: python_file_path,
                            env: env.clone(),
                            cwd: resolved.host_cwd.clone(),
                            limits: python_execution_limits_with_env(vm, &env),
                            guest_runtime: guest_runtime_identity(vm, None, None),
                        },
                        vm.runtime_context.clone(),
                    )
                    .await
                    .map_err(python_error)
                {
                    Ok(execution) => execution,
                    Err(error) => {
                        self.python_engine.dispose_context(&context_id);
                        rollback_failed_top_level_process_start(
                            &mut vm.kernel,
                            &kernel_handle,
                            None,
                            "top-level Python engine start",
                        );
                        return Err(error);
                    }
                };
                (
                    ActiveExecution::Python(execution),
                    env.clone(),
                    StartedTopLevelAdapterContext::Python(context_id),
                )
            }
            GuestRuntimeKind::WebAssembly => {
                let wasm_limits = wasm_execution_limits(vm);
                let wasm_guest_runtime =
                    guest_runtime_identity(vm, Some(u64::from(kernel_pid)), Some(0));
                let wasm_permission_tier = top_level_start_step!(
                    vm.kernel
                        .process_permission_tier(EXECUTION_DRIVER_NAME, kernel_pid)
                        .map_err(kernel_error),
                    "top-level compatibility-WASM permission lookup"
                );
                let module_path = match payload.wasm_backend {
                    _ if matches!(
                        standalone_wasm_backend,
                        ExecutionStandaloneWasmBackend::Wasmtime
                            | ExecutionStandaloneWasmBackend::WasmtimeThreads
                    ) =>
                    {
                        env.get("AGENTOS_GUEST_ENTRYPOINT")
                            .map(|path| format!("{TRUSTED_INITIAL_MODULE_PREFIX}{path}"))
                            .unwrap_or_else(|| resolved.entrypoint.clone())
                    }
                    _ => resolved.entrypoint.clone(),
                };
                let context = self.wasm_engine.create_context(CreateWasmContextRequest {
                    vm_id: vm_id.clone(),
                    module_path: Some(module_path),
                });
                let context_id = context.context_id;
                let execution = match self
                    .wasm_engine
                    .start_execution_with_runtime_async_for_backend(
                        StartWasmExecutionRequest {
                            vm_id: vm_id.clone(),
                            context_id: context_id.clone(),
                            managed_kernel_host: true,
                            argv: resolved.process_args.clone(),
                            env: env.clone(),
                            cwd: resolved.host_cwd.clone(),
                            permission_tier: execution_wasm_permission_tier(wasm_permission_tier),
                            limits: wasm_limits,
                            guest_runtime: wasm_guest_runtime,
                        },
                        vm.runtime_context.clone(),
                        standalone_wasm_backend,
                    )
                    .await
                    .map_err(wasm_error)
                {
                    Ok(execution) => execution,
                    Err(error) => {
                        self.wasm_engine.dispose_context(&context_id);
                        rollback_failed_top_level_process_start(
                            &mut vm.kernel,
                            &kernel_handle,
                            None,
                            "top-level compatibility-WASM engine start",
                        );
                        return Err(error);
                    }
                };
                (
                    ActiveExecution::Wasm(Box::new(execution)),
                    env,
                    StartedTopLevelAdapterContext::WebAssembly(context_id),
                )
            }
        };
        let reported_process_id = execution.native_process_id().unwrap_or(kernel_pid);
        let phase_start = Instant::now();
        let mut process = ActiveProcess::new_with_attached_runtime_control(
            kernel_pid,
            kernel_handle,
            vm.runtime_context.clone(),
            vm.limits.clone(),
            process_event_capacity,
            resolved.runtime,
            execution,
            runtime_control,
            Arc::clone(&self.process_event_notify),
        )
        .with_adapter_policy(resolved.adapter_policy)
        .with_standalone_wasm_backend(standalone_wasm_backend)
        .with_vm_pending_byte_budgets(vm_pending_stdin_bytes_budget, vm_pending_event_bytes_budget)
        .with_kernel_stdin_writer_fd(kernel_stdin_writer_fd)
        .with_tty_master_fd(tty_master_fd)
        .with_guest_cwd(resolved.guest_cwd.clone())
        .with_env(process_env)
        .with_host_cwd(resolved.host_cwd.clone());
        if let Err(error) = process.apply_runtime_controls() {
            let rollback_handle = process.kernel_handle.clone();
            rollback_failed_top_level_process_start(
                &mut vm.kernel,
                &rollback_handle,
                Some(&mut process.execution),
                "top-level pending runtime control",
            );
            dispose_started_context!(&started_context);
            return Err(error);
        }
        vm.active_processes
            .insert(payload.process_id.clone(), process);
        // A fast executor can publish its first event before this process is
        // visible to the pump. Rearm after the authoritative registration
        // commit so that event cannot remain stranded.
        self.process_event_notify.notify_one();
        if let Err(error) = self.bridge.emit_lifecycle(&vm_id, LifecycleState::Busy) {
            rollback_published_top_level_process_start(
                vm,
                &payload.process_id,
                "top-level engine lifecycle publication",
            );
            dispose_started_context!(&started_context);
            return Err(error);
        }
        mark_execute_response_ready(&vm_id, &payload.process_id);
        record_execute_phase("process_register_and_lifecycle", phase_start.elapsed());
        record_execute_phase("execute_total", execute_total_start.elapsed());

        Ok(DispatchResult {
            response: process_started_response(
                request,
                payload.process_id,
                Some(reported_process_id),
            ),
            events: Vec::new(),
        })
    }
}
