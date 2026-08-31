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
    vm: &VmState,
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
    })
}

fn resolve_command_execution(
    vm: &VmState,
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
                            resolve_vm_guest_path_to_host(vm, guest_entrypoint)
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
                        resolve_vm_guest_path_to_host(vm, guest_entrypoint)
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

    let host_entrypoint = resolve_vm_guest_path_to_host(vm, &guest_entrypoint);
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
    })
}

const MAX_JAVASCRIPT_COMMAND_REDIRECT_DEPTH: usize = 4;

pub(super) fn resolve_javascript_command_entrypoint(
    vm: &VmState,
    guest_entrypoint: &str,
    host_entrypoint: &Path,
) -> Option<(String, PathBuf)> {
    // agentOS package content is served guest-native (tar + single-symlink
    // mounts) and is never materialized on the host, so the shebang-reading
    // fallback below (which reads the host path) cannot classify these
    // entrypoints. Within the package mount the only runtimes are WebAssembly
    // (`*.wasm`) and JavaScript, and `bin/<cmd>` launchers are frequently
    // extensionless — so classify by extension here: `.wasm` is WASM (fall
    // through), everything else in the mount is JavaScript.
    if guest_path_is_within_agentos_package_mount(vm, guest_entrypoint) {
        let extension = Path::new(guest_entrypoint)
            .extension()
            .and_then(|extension| extension.to_str());
        if extension != Some("wasm") {
            return Some((guest_entrypoint.to_owned(), host_entrypoint.to_path_buf()));
        }
        return None;
    }

    resolve_javascript_command_entrypoint_inner(
        vm,
        guest_entrypoint,
        host_entrypoint,
        MAX_JAVASCRIPT_COMMAND_REDIRECT_DEPTH,
    )
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
    let resolved_host_entrypoint = resolve_vm_guest_path_to_host(vm, &resolved_guest_entrypoint);
    (resolved_guest_entrypoint, resolved_host_entrypoint)
}

fn resolve_javascript_command_entrypoint_inner(
    vm: &VmState,
    guest_entrypoint: &str,
    host_entrypoint: &Path,
    redirects_remaining: usize,
) -> Option<(String, PathBuf)> {
    if redirects_remaining > 0 {
        let symlink_target = fs::symlink_metadata(host_entrypoint)
            .ok()
            .filter(|metadata| metadata.file_type().is_symlink())
            .and_then(|_| fs::read_link(host_entrypoint).ok());
        if let Some(symlink_target) = symlink_target {
            let guest_parent = Path::new(guest_entrypoint)
                .parent()
                .and_then(|path| path.to_str())
                .unwrap_or("/");
            let symlink_guest_entrypoint = if symlink_target.is_absolute() {
                normalize_path(&symlink_target.to_string_lossy())
            } else {
                normalize_path(&format!(
                    "{guest_parent}/{}",
                    symlink_target.to_string_lossy().replace('\\', "/")
                ))
            };
            let symlink_host_entrypoint =
                resolve_vm_guest_path_to_host(vm, &symlink_guest_entrypoint);
            return resolve_javascript_command_entrypoint_inner(
                vm,
                &symlink_guest_entrypoint,
                &symlink_host_entrypoint,
                redirects_remaining - 1,
            );
        }
    }

    let script = load_executable_script_preview(host_entrypoint)?;
    let interpreter = parse_script_interpreter_name(&script);

    if interpreter.is_none() && is_probable_javascript_entrypoint(host_entrypoint, &script) {
        return Some((guest_entrypoint.to_owned(), host_entrypoint.to_path_buf()));
    }

    let interpreter = interpreter?;
    if interpreter == "node" {
        return Some((guest_entrypoint.to_owned(), host_entrypoint.to_path_buf()));
    }

    if redirects_remaining == 0 || !matches!(interpreter.as_str(), "sh" | "bash" | "dash") {
        return None;
    }

    let shim_target = parse_node_shell_shim_target(&script)?;
    let guest_parent = Path::new(guest_entrypoint)
        .parent()
        .and_then(|path| path.to_str())
        .unwrap_or("/");
    let shim_guest_entrypoint = normalize_path(&format!("{guest_parent}/{shim_target}"));
    let shim_host_entrypoint = resolve_vm_guest_path_to_host(vm, &shim_guest_entrypoint);
    resolve_javascript_command_entrypoint_inner(
        vm,
        &shim_guest_entrypoint,
        &shim_host_entrypoint,
        redirects_remaining - 1,
    )
}

fn load_executable_script_preview(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let preview_len = bytes.len().min(16 * 1024);
    Some(String::from_utf8_lossy(&bytes[..preview_len]).into_owned())
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
        resolve_vm_guest_path_to_host(vm, &guest_cwd)
    };
    (guest_cwd, host_cwd, value.is_none())
}

pub(super) fn resolve_vm_guest_path_to_host(vm: &VmState, guest_path: &str) -> PathBuf {
    host_mount_path_for_guest_path(vm, guest_path)
        .unwrap_or_else(|| shadow_path_for_guest(vm, guest_path))
}

pub(super) fn shadow_path_for_guest(vm: &VmState, guest_path: &str) -> PathBuf {
    let normalized = normalize_path(guest_path);
    let relative = normalized.trim_start_matches('/');
    if relative.is_empty() {
        return vm.cwd.clone();
    }
    vm.cwd.join(relative)
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
    // example `bash -c -l 'echo ok'`). The compact shell shipped in AgentOS
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

pub(crate) fn sync_active_process_host_writes_to_kernel(
    vm: &mut VmState,
) -> Result<(), SidecarError> {
    if vm.root_filesystem_mode != RootFilesystemMode::ReadOnly {
        sync_vm_shadow_root_to_kernel(vm)?;
    }

    let normalized_vm_root = normalize_host_path(&vm.cwd);
    let extra_roots = collect_active_process_host_sync_roots(vm, &normalized_vm_root);
    for (host_cwd, guest_cwd) in extra_roots {
        sync_host_directory_tree_to_kernel(vm, &host_cwd, &guest_cwd)?;
    }

    Ok(())
}

fn sync_vm_shadow_root_to_kernel(vm: &mut VmState) -> Result<(), SidecarError> {
    let shadow_root = normalize_host_path(&vm.cwd);
    if host_sync_root_is_filesystem_root(&shadow_root) {
        tracing::warn!("skipping host shadow sync rooted at the host filesystem root");
        return Ok(());
    }
    let mut snapshot = collect_shadow_sync_snapshot(&shadow_root)?;
    snapshot
        .entries
        .retain(|path, _| !should_skip_shadow_sync_path(vm, path));

    // An unreadable subtree is not an empty subtree. Preserve the previous
    // inventory below it so a transient EACCES cannot be misinterpreted as a
    // request to delete all of its kernel children.
    for (path, entry) in &vm.shadow_sync_inventory {
        if snapshot
            .incomplete_subtrees
            .iter()
            .any(|prefix| guest_path_is_at_or_below(path, prefix))
        {
            snapshot.entries.entry(path.clone()).or_insert(*entry);
        }
    }

    let failed_replacements = propagate_shadow_deletions_to_kernel(vm, &mut snapshot.entries);
    let mut synced_file_times = BTreeMap::new();
    sync_host_directory_tree_to_kernel_inner(
        vm,
        &shadow_root,
        &shadow_root,
        "/",
        &mut synced_file_times,
        Some(&snapshot.entries),
        Some(&failed_replacements),
    )?;
    vm.shadow_sync_inventory = snapshot.entries;
    Ok(())
}

#[derive(Default)]
struct ShadowSyncSnapshot {
    entries: BTreeMap<String, ShadowSyncInventoryEntry>,
    incomplete_subtrees: BTreeSet<String>,
}

/// Capture the shadow root before any additive kernel writes. The separate
/// inventory pass lets deletion and type-replacement reconciliation happen
/// first, which is essential when a stale symlink or directory occupies the
/// pathname that is about to become a regular file.
pub(crate) fn initial_shadow_sync_inventory(
    shadow_root: &Path,
) -> Result<BTreeMap<String, ShadowSyncInventoryEntry>, SidecarError> {
    Ok(collect_shadow_sync_snapshot(shadow_root)?.entries)
}

fn collect_shadow_sync_snapshot(shadow_root: &Path) -> Result<ShadowSyncSnapshot, SidecarError> {
    let mut snapshot = ShadowSyncSnapshot::default();
    collect_shadow_sync_snapshot_inner(shadow_root, shadow_root, "/", &mut snapshot)?;
    Ok(snapshot)
}

fn collect_shadow_sync_snapshot_inner(
    shadow_root: &Path,
    current_host_dir: &Path,
    current_guest_dir: &str,
    snapshot: &mut ShadowSyncSnapshot,
) -> Result<(), SidecarError> {
    let entries = match fs::read_dir(current_host_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(libc::EPERM) =>
        {
            let guest_path = normalize_path(current_guest_dir);
            snapshot.incomplete_subtrees.insert(guest_path.clone());
            tracing::warn!(
                path = %current_host_dir.display(),
                guest_path = %guest_path,
                "shadow inventory is incomplete because a host directory is unreadable"
            );
            return Ok(());
        }
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inventory host shadow directory {}: {error}",
                current_host_dir.display()
            )));
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SidecarError::Io(format!(
                    "failed to inventory host shadow entry in {}: {error}",
                    current_host_dir.display()
                )));
            }
        };
        let host_path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SidecarError::Io(format!(
                    "failed to inventory host shadow entry {}: {error}",
                    host_path.display()
                )));
            }
        };
        let relative_path = host_path.strip_prefix(shadow_root).map_err(|error| {
            SidecarError::InvalidState(format!(
                "failed to relativize host shadow path {} against {}: {error}",
                host_path.display(),
                shadow_root.display()
            ))
        })?;
        let guest_path = normalize_path(&format!(
            "/{}",
            relative_path.to_string_lossy().replace('\\', "/")
        ));
        let node_type = if file_type.is_dir() {
            ShadowNodeType::Directory
        } else if file_type.is_file() {
            ShadowNodeType::File
        } else if file_type.is_symlink() {
            ShadowNodeType::Symlink
        } else {
            continue;
        };
        snapshot.entries.insert(
            guest_path.clone(),
            ShadowSyncInventoryEntry::present(node_type),
        );
        if node_type == ShadowNodeType::Directory {
            collect_shadow_sync_snapshot_inner(shadow_root, &host_path, &guest_path, snapshot)?;
        }
    }
    Ok(())
}

/// Removes kernel paths whose shadow copy disappeared since the last walk.
///
/// Best-effort by design: a failure to reconcile one stale path must not
/// poison the guest filesystem operation that triggered the sync, so failures
/// are surfaced as host-visible warnings instead of errors. Children sort
/// after their parents in the `BTreeSet`, so the reverse iteration removes
/// leaves before the directories that contain them.
fn propagate_shadow_deletions_to_kernel(
    vm: &mut VmState,
    current: &mut BTreeMap<String, ShadowSyncInventoryEntry>,
) -> BTreeSet<String> {
    let stale = vm
        .shadow_sync_inventory
        .iter()
        .rev()
        .filter_map(|(path, previous)| {
            let replacement = current.get(path);
            (previous.deletion_pending
                || replacement.is_none()
                || replacement.is_some_and(|entry| entry.node_type != previous.node_type))
            .then(|| (path.clone(), *previous))
        })
        .collect::<Vec<_>>();
    let mut failed = BTreeSet::new();
    for (path, previous) in stale {
        if path == "/" || should_skip_shadow_sync_path(vm, &path) || is_shadow_bootstrap_dir(&path)
        {
            continue;
        }
        let stat = match vm.kernel.lstat(&path) {
            Ok(stat) => stat,
            Err(error) if error.code() == "ENOENT" => continue,
            Err(error) => {
                tracing::warn!(
                    path = %path,
                    error = %error,
                    "failed to inspect a stale shadow path; deletion will be retried"
                );
                retain_shadow_deletion_tombstone(current, &path, previous);
                failed.insert(path);
                continue;
            }
        };
        let result = if stat.is_directory && !stat.is_symbolic_link {
            vm.kernel.remove_dir(&path)
        } else {
            vm.kernel.remove_file(&path)
        };
        if let Err(error) = result {
            if error.code() != "ENOENT" {
                tracing::warn!(
                    path = %path,
                    error = %error,
                    "failed to propagate guest shadow deletion into the kernel VFS; deletion will be retried"
                );
                retain_shadow_deletion_tombstone(current, &path, previous);
                failed.insert(path);
            }
        }
    }
    failed
}

fn retain_shadow_deletion_tombstone(
    current: &mut BTreeMap<String, ShadowSyncInventoryEntry>,
    path: &str,
    previous: ShadowSyncInventoryEntry,
) {
    current
        .entry(path.to_owned())
        .and_modify(|entry| entry.deletion_pending = true)
        .or_insert(ShadowSyncInventoryEntry {
            node_type: previous.node_type,
            deletion_pending: true,
        });
}

fn collect_active_process_host_sync_roots(
    vm: &VmState,
    normalized_vm_root: &Path,
) -> Vec<(PathBuf, String)> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();

    for process in vm.active_processes.values() {
        collect_process_host_sync_roots(process, normalized_vm_root, &mut seen, &mut roots);
    }

    roots
}

fn collect_process_host_sync_roots(
    process: &ActiveProcess,
    normalized_vm_root: &Path,
    seen: &mut BTreeSet<(PathBuf, String)>,
    roots: &mut Vec<(PathBuf, String)>,
) {
    // Kernel-backed runtimes make clean writes observable immediately and keep
    // their host trees only as mirrors. Importing every such mirror lets an
    // inherited child's older snapshot overwrite a shared kernel file. Only a
    // dirty or otherwise non-observable process root is authoritative input.
    if process.host_write_dirty || !process.clean_host_writes_are_observable() {
        let normalized_host_cwd = normalize_host_path(&process.host_cwd);
        if !path_is_within_root(&normalized_host_cwd, normalized_vm_root) {
            let guest_cwd = normalize_path(&process.guest_cwd);
            if seen.insert((normalized_host_cwd.clone(), guest_cwd.clone())) {
                roots.push((normalized_host_cwd, guest_cwd));
            }
        }
    }

    for child in process.child_processes.values() {
        collect_process_host_sync_roots(child, normalized_vm_root, seen, roots);
    }
}

pub(crate) fn sync_process_host_writes_to_kernel(
    vm: &mut VmState,
    process: &ActiveProcess,
) -> Result<(), SidecarError> {
    sync_process_host_roots_to_kernel(
        vm,
        &process.host_cwd,
        &process.guest_cwd,
        process.runtime != GuestRuntimeKind::JavaScript,
    )
}

pub(super) fn sync_process_host_roots_to_kernel(
    vm: &mut VmState,
    process_host_cwd: &Path,
    process_guest_cwd: &str,
    sync_root_shadow: bool,
) -> Result<(), SidecarError> {
    if sync_root_shadow && vm.root_filesystem_mode != RootFilesystemMode::ReadOnly {
        sync_vm_shadow_root_to_kernel(vm)?;
    }

    if !path_is_within_root(
        &normalize_host_path(process_host_cwd),
        &normalize_host_path(&vm.cwd),
    ) {
        sync_host_directory_tree_to_kernel(vm, process_host_cwd, process_guest_cwd)?;
    }

    Ok(())
}

fn host_sync_root_is_filesystem_root(host_root: &Path) -> bool {
    normalize_host_path(host_root) == Path::new("/")
}

fn sync_host_directory_tree_to_kernel(
    vm: &mut VmState,
    host_root: &Path,
    guest_root: &str,
) -> Result<(), SidecarError> {
    let normalized_host_root = normalize_host_path(host_root);
    let normalized_guest_root = normalize_path(guest_root);
    if guest_sync_root_targets_live_mount(vm, &normalized_guest_root) {
        // A host_dir/module_access mount is the kernel's live backing store.
        // Importing shadow output through a guest alias into that mount is
        // destructive: symlink reconciliation can unlink or rewrite entries
        // in the host workspace while a process is exiting.
        return Ok(());
    }
    if host_sync_root_is_filesystem_root(host_root) {
        // A process tracked with host cwd "/" would pull the entire host
        // filesystem into the kernel VFS (until the size/inode caps fire).
        // No sanctioned flow shadows the host root wholesale; host access is
        // scoped through mounts.
        tracing::warn!("skipping host shadow sync rooted at the host filesystem root");
        return Ok(());
    }
    let mut synced_file_times = BTreeMap::new();
    sync_host_directory_tree_to_kernel_inner(
        vm,
        &normalized_host_root,
        &normalized_host_root,
        &normalized_guest_root,
        &mut synced_file_times,
        None,
        None,
    )
}

fn guest_sync_root_targets_live_mount(vm: &mut VmState, guest_root: &str) -> bool {
    host_mount_path_for_guest_path(vm, guest_root).is_some()
        || vm
            .kernel
            .realpath(guest_root)
            .ok()
            .is_some_and(|resolved| host_mount_path_for_guest_path(vm, &resolved).is_some())
}

fn sync_host_directory_tree_to_kernel_inner(
    vm: &mut VmState,
    host_root: &Path,
    current_host_dir: &Path,
    guest_root: &str,
    synced_file_times: &mut BTreeMap<(u64, u64), (u64, u64)>,
    expected_inventory: Option<&BTreeMap<String, ShadowSyncInventoryEntry>>,
    failed_replacements: Option<&BTreeSet<String>>,
) -> Result<(), SidecarError> {
    let entries = match fs::read_dir(current_host_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            // Host dirs the sidecar user cannot read (e.g. root-owned
            // /lost+found under a host-root mount) are skipped rather than
            // failing the whole shadow sync; the guest just won't see them.
            tracing::warn!(
                path = %current_host_dir.display(),
                "skipping unreadable host shadow directory"
            );
            return Ok(());
        }
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to read host shadow directory {}: {error}",
                current_host_dir.display()
            )));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|error| {
            SidecarError::Io(format!(
                "failed to read host shadow entry in {}: {error}",
                current_host_dir.display()
            ))
        })?;
        let host_path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            SidecarError::Io(format!(
                "failed to stat host shadow entry {}: {error}",
                host_path.display()
            ))
        })?;
        let relative_path = host_path
            .strip_prefix(host_root)
            .map_err(|error| {
                SidecarError::InvalidState(format!(
                    "failed to relativize host shadow path {} against {}: {error}",
                    host_path.display(),
                    host_root.display()
                ))
            })?
            .to_string_lossy()
            .replace('\\', "/");
        let guest_path = if guest_root == "/" {
            normalize_path(&format!("/{relative_path}"))
        } else {
            normalize_path(&format!(
                "{}/{}",
                guest_root.trim_end_matches('/'),
                relative_path
            ))
        };

        if should_skip_shadow_sync_path(vm, &guest_path) {
            continue;
        }
        if expected_inventory.is_some_and(|inventory| !inventory.contains_key(&guest_path)) {
            // The entry appeared after the inventory pass. Pick it up on the
            // next sync rather than mixing two different shadow snapshots.
            continue;
        }
        if failed_replacements.is_some_and(|failed| failed.contains(&guest_path)) {
            // Never write through a stale object whose removal failed. The
            // retained tombstone will retry before a future additive write.
            continue;
        }

        if file_type.is_dir() {
            ensure_kernel_shadow_node_type(vm, &guest_path, ShadowNodeType::Directory)?;
            let metadata = entry.metadata().map_err(|error| {
                SidecarError::Io(format!(
                    "failed to read host shadow metadata {}: {error}",
                    host_path.display()
                ))
            })?;
            if !is_shadow_bootstrap_dir(&guest_path) {
                if !vm.kernel.exists(&guest_path).unwrap_or(false) {
                    vm.kernel.mkdir(&guest_path, true).map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "failed to sync host shadow directory {} to guest {}: {}",
                            host_path.display(),
                            guest_path,
                            kernel_error(error)
                        ))
                    })?;
                }
                vm.kernel
                    .chmod(&guest_path, host_shadow_mode(&metadata))
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "failed to sync host shadow directory mode {} to guest {}: {}",
                            host_path.display(),
                            guest_path,
                            kernel_error(error)
                        ))
                    })?;
            }
            sync_host_directory_tree_to_kernel_inner(
                vm,
                host_root,
                &host_path,
                guest_root,
                synced_file_times,
                expected_inventory,
                failed_replacements,
            )?;
            continue;
        }

        if file_type.is_file() {
            ensure_kernel_shadow_node_type(vm, &guest_path, ShadowNodeType::File)?;
            let metadata = entry.metadata().map_err(|error| {
                SidecarError::Io(format!(
                    "failed to read host shadow metadata {}: {error}",
                    host_path.display()
                ))
            })?;
            let timestamp_key = (metadata.dev(), metadata.ino());
            let (atime_ms, mtime_ms) =
                *synced_file_times.entry(timestamp_key).or_insert_with(|| {
                    (
                        metadata_time_ms(metadata.atime(), metadata.atime_nsec()),
                        metadata_time_ms(metadata.mtime(), metadata.mtime_nsec()),
                    )
                });
            let desired_mode = host_shadow_mode(&metadata);
            // Fast path: skip the expensive re-read + re-write when the kernel already
            // holds a copy of this shadow file that matches on size, mode, and mtime.
            //
            // Every read-side fs op (exists/stat/readFile/...) triggers a full
            // shadow-tree reconciliation walk. Without this skip the walk re-reads every
            // file's bytes from the host and re-writes them into the kernel VFS on every
            // op -- O(whole tree) per op, and super-linear as the VM's shadow grows,
            // which is a dominant source of session-creation/runtime latency on
            // populated VMs.
            //
            // This is a (size, mode, mtime) quick-check, the same heuristic rsync uses
            // by default. It needs no separate cache to invalidate -- it compares against
            // the kernel's own stat, so a kernel reset (e.g. a layer swap) or any host
            // change that moves size/mode/mtime forces a resync. Limitation: mtime is
            // compared at the millisecond granularity the kernel stores (utimes truncates
            // to ms), so a host-side rewrite that preserves byte length AND mode AND lands
            // in the same wall-clock millisecond can be skipped and leave stale bytes.
            // That window is sub-millisecond same-length edits; if it ever matters here,
            // upgrade this to a content digest (or full-precision mtime) for files whose
            // mtime is within the last few ms of `now`.
            if let Ok(existing) = vm.kernel.lstat(&guest_path) {
                if !existing.is_directory
                    && !existing.is_symbolic_link
                    && existing.size == metadata.len()
                    && (existing.mode & 0o7777) == (desired_mode & 0o7777)
                    && existing.mtime_ms == mtime_ms
                {
                    continue;
                }
            }
            let bytes = match read_host_shadow_file(&host_path, desired_mode) {
                Ok(bytes) => bytes,
                // The host entry vanished between the walk and the read
                // (short-lived files churn constantly — editor swap files,
                // temp files). Skipping matches native semantics; failing
                // here would poison EVERY subsequent fs op on the VM.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                // Same tolerance for entries the sidecar user cannot read
                // (root-owned files under a host-root mount): skip them
                // rather than poisoning the whole sync.
                Err(error)
                    if error.kind() == std::io::ErrorKind::PermissionDenied
                        || error.raw_os_error() == Some(libc::EPERM) =>
                {
                    tracing::warn!(
                        path = %host_path.display(),
                        "skipping unreadable host shadow file"
                    );
                    continue;
                }
                Err(error) => {
                    return Err(SidecarError::Io(format!(
                        "failed to read host shadow file {}: {error}",
                        host_path.display()
                    )));
                }
            };
            match vm.kernel.write_file(&guest_path, bytes) {
                Ok(()) => {}
                // ENOENT here means the guest-side path cannot currently
                // receive the write (e.g. it is a symlink whose target was
                // just unlinked by the guest — vim's swap-file dance). The
                // entry is mid-churn; skip it rather than failing the VM.
                Err(error) if error.code() == "ENOENT" => continue,
                Err(error) => {
                    return Err(SidecarError::InvalidState(format!(
                        "failed to sync host shadow file {} to guest {}: {}",
                        host_path.display(),
                        guest_path,
                        kernel_error(error)
                    )));
                }
            }
            vm.kernel
                .chmod(&guest_path, desired_mode)
                .map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "failed to sync host shadow file mode {} to guest {}: {}",
                        host_path.display(),
                        guest_path,
                        kernel_error(error)
                    ))
                })?;
            vm.kernel
                .utimes(&guest_path, atime_ms, mtime_ms)
                .map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "failed to sync host shadow file times {} to guest {}: {}",
                        host_path.display(),
                        guest_path,
                        kernel_error(error)
                    ))
                })?;
            continue;
        }

        if file_type.is_symlink() {
            ensure_kernel_shadow_node_type(vm, &guest_path, ShadowNodeType::Symlink)?;
            let target = match fs::read_link(&host_path) {
                Ok(target) => target,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(SidecarError::Io(format!(
                        "failed to read host shadow symlink {}: {error}",
                        host_path.display()
                    )));
                }
            };
            replace_kernel_symlink(vm, &guest_path, &target.to_string_lossy())?;
        }
    }

    Ok(())
}

fn ensure_kernel_shadow_node_type(
    vm: &mut VmState,
    guest_path: &str,
    desired: ShadowNodeType,
) -> Result<(), SidecarError> {
    let existing = match vm.kernel.lstat(guest_path) {
        Ok(existing) => existing,
        Err(error) if error.code() == "ENOENT" => return Ok(()),
        Err(error) => return Err(kernel_error(error)),
    };
    let existing_type = if existing.is_symbolic_link {
        ShadowNodeType::Symlink
    } else if existing.is_directory {
        ShadowNodeType::Directory
    } else {
        ShadowNodeType::File
    };
    if existing_type == desired {
        return Ok(());
    }

    let result = if existing_type == ShadowNodeType::Directory {
        vm.kernel.remove_dir(guest_path)
    } else {
        // `remove_file` unlinks the directory entry itself. It must run before
        // `write_file`, otherwise that write would follow a stale symlink.
        vm.kernel.remove_file(guest_path)
    };
    result.map_err(|error| {
        SidecarError::InvalidState(format!(
            "failed to replace shadow path {guest_path} from {existing_type:?} to {desired:?}: {}",
            kernel_error(error)
        ))
    })
}

fn replace_kernel_symlink(
    vm: &mut VmState,
    guest_path: &str,
    target: &str,
) -> Result<(), SidecarError> {
    if vm.kernel.symlink(target, guest_path).is_ok() {
        return Ok(());
    }

    if let Ok(existing_target) = vm.kernel.read_link(guest_path) {
        if existing_target == target {
            return Ok(());
        }
    }

    let _ = vm.kernel.remove_file(guest_path);
    let _ = vm.kernel.remove_dir(guest_path);
    vm.kernel
        .symlink(target, guest_path)
        .map_err(kernel_error)?;
    Ok(())
}

fn host_shadow_mode(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode() & 0o7777
}

/// Reads a shadow-root file back into the kernel even when guest-visible mode
/// bits make it unreadable for the host user. The sidecar is the kernel for
/// this tree, so guest permission bits (for example a 0o200 write-only file
/// produced by `chmod` plus a shell append redirect) must not break the
/// exit-time shadow sync. The original mode is restored after the read.
fn read_host_shadow_file(host_path: &Path, mode: u32) -> std::io::Result<Vec<u8>> {
    match fs::read(host_path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            fs::set_permissions(host_path, fs::Permissions::from_mode(mode | 0o400))?;
            let result = fs::read(host_path);
            fs::set_permissions(host_path, fs::Permissions::from_mode(mode))?;
            result
        }
        Err(error) => Err(error),
    }
}

fn metadata_time_ms(seconds: i64, nanos: i64) -> u64 {
    let seconds = seconds.max(0) as u64;
    let nanos = nanos.max(0) as u64;
    seconds
        .saturating_mul(1_000)
        .saturating_add(nanos / 1_000_000)
}

fn is_shadow_bootstrap_dir(path: &str) -> bool {
    matches!(
        path,
        "/dev"
            | "/proc"
            | "/tmp"
            | "/bin"
            | "/lib"
            | "/sbin"
            | "/boot"
            | "/etc"
            | "/root"
            | "/run"
            | "/srv"
            | "/sys"
            | "/opt"
            | "/mnt"
            | "/media"
            | "/home"
            | "/home/agentos"
            | "/usr"
            | "/usr/bin"
            | "/usr/games"
            | "/usr/include"
            | "/usr/lib"
            | "/usr/libexec"
            | "/usr/man"
            | "/usr/local"
            | "/usr/local/bin"
            | "/usr/sbin"
            | "/usr/share"
            | "/usr/share/man"
            | "/var"
            | "/var/cache"
            | "/var/empty"
            | "/var/lib"
            | "/var/lock"
            | "/var/log"
            | "/var/run"
            | "/var/spool"
            | "/var/tmp"
            | "/etc/agentos"
            | "/workspace"
    )
}

#[cfg(test)]
mod shadow_sync_tests {
    use super::{is_protected_agentos_shadow_sync_path, is_shadow_bootstrap_dir};

    #[test]
    fn shadow_bootstrap_sync_skips_virtual_home_tree() {
        assert!(is_shadow_bootstrap_dir("/home"));
        assert!(is_shadow_bootstrap_dir("/home/agentos"));
    }

    #[test]
    fn protected_agentos_paths_are_not_shadow_synced() {
        assert!(is_protected_agentos_shadow_sync_path("/etc/agentos"));
        assert!(is_protected_agentos_shadow_sync_path(
            "/etc/agentos/instructions.md"
        ));
        assert!(!is_protected_agentos_shadow_sync_path("/etc/agentos-copy"));
        assert!(!is_protected_agentos_shadow_sync_path("/etc/agentos.md"));
    }
}

fn is_kernel_owned_shadow_sync_path(path: &str) -> bool {
    matches!(path, "/dev" | "/proc" | "/sys")
        || path.starts_with("/dev/")
        || path.starts_with("/proc/")
        || path.starts_with("/sys/")
}

pub(crate) fn is_protected_agentos_shadow_sync_path(path: &str) -> bool {
    path == "/etc/agentos" || path.starts_with("/etc/agentos/")
}

fn should_skip_shadow_sync_path(vm: &VmState, guest_path: &str) -> bool {
    is_kernel_owned_shadow_sync_path(guest_path)
        || is_protected_agentos_shadow_sync_path(guest_path)
        // Every configured mount is kernel-owned at and below its normalized
        // guest path. Shadow files are stale compatibility artifacts there;
        // syncing them would overwrite memory/plugin state (or fail on a
        // read-only mount) and deleting them must not unmount guest data.
        || vm.configuration.mounts.iter().any(|mount| {
            normalize_path(&mount.guest_path) != "/"
                && guest_path_is_at_or_below(guest_path, &mount.guest_path)
        })
}

fn guest_path_is_at_or_below(path: &str, prefix: &str) -> bool {
    let path = normalize_path(path);
    let prefix = normalize_path(prefix);
    prefix == "/" || path == prefix || path.starts_with(&format!("{prefix}/"))
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
    request: &mut JavascriptChildProcessSpawnRequest,
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
    let is_registered_command = vm
        .command_guest_paths
        .values()
        .any(|path| normalize_path(path) == script_path);
    if !is_registered_command {
        let stat = vm.kernel.stat(&script_path).map_err(kernel_error)?;
        if stat.is_directory || stat.mode & 0o111 == 0 {
            return Err(SidecarError::Execution(format!(
                "EACCES: permission denied, execute '{script_path}'"
            )));
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
            return Err(SidecarError::Execution(format!(
                "ENOEXEC: shebang line exceeds {MAX_SHEBANG_LINE_BYTES} bytes: {script_path}"
            )));
        }
        Some(index) => index,
        None if header.len() > MAX_SHEBANG_LINE_BYTES => {
            return Err(SidecarError::Execution(format!(
                "ENOEXEC: shebang line exceeds {MAX_SHEBANG_LINE_BYTES} bytes: {script_path}"
            )));
        }
        None => header.len(),
    };
    let line = header[2..line_end]
        .strip_suffix(b"\r")
        .unwrap_or(&header[2..line_end]);
    let text = std::str::from_utf8(line).map_err(|_| {
        SidecarError::Execution(format!("ENOEXEC: invalid shebang line: {script_path}"))
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
        return Err(SidecarError::Execution(format!(
            "ENOEXEC: invalid shebang line: {script_path}"
        )));
    }
    let (command, mut interpreter_args) = if matches!(interpreter, "/usr/bin/env" | "/bin/env") {
        let optional_arg = optional_arg.ok_or_else(|| {
            SidecarError::Execution(format!(
                "ENOENT: missing interpreter after {interpreter} in shebang: {script_path}"
            ))
        })?;
        if let Some(split_string) = optional_arg
            .strip_prefix("-S")
            .filter(|rest| rest.starts_with(|ch: char| ch.is_ascii_whitespace()))
        {
            let mut words = shlex::split(split_string.trim()).ok_or_else(|| {
                SidecarError::Execution(format!(
                    "ENOEXEC: invalid /usr/bin/env -S quoting in shebang: {script_path}"
                ))
            })?;
            if words.is_empty() {
                return Err(SidecarError::Execution(format!(
                    "ENOENT: missing interpreter after /usr/bin/env -S in shebang: {script_path}"
                )));
            }
            let command = words.remove(0);
            (command, words)
        } else {
            if optional_arg.starts_with('-')
                || optional_arg.chars().any(|ch| ch.is_ascii_whitespace())
            {
                return Err(SidecarError::Execution(format!(
                    "ENOEXEC: /usr/bin/env shebang arguments require -S: {script_path}"
                )));
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
    vm: &VmState,
    guest_cwd: &str,
    command: &str,
    path_env: Option<&str>,
) -> Option<String> {
    if !is_path_like_specifier(command) {
        if let Some(entrypoint) = vm.command_guest_paths.get(command) {
            return Some(entrypoint.clone());
        }

        for search_dir in guest_command_search_dirs(vm, guest_cwd, path_env) {
            let candidate = normalize_path(&format!("{search_dir}/{command}"));
            if let Some(entrypoint) = resolve_guest_command_path_candidate(vm, &candidate) {
                return Some(entrypoint);
            }
        }

        return None;
    }

    let normalized = resolve_path_like_guest_specifier(guest_cwd, command);
    resolve_guest_command_path_candidate(vm, &normalized).or_else(|| {
        // Some guest shells materialize PATH lookups into absolute candidate paths.
        // If that path points into a searched directory but does not exist, fall
        // back to the command basename so the sidecar can remap VM command packages.
        let parent_dir = Path::new(&normalized).parent()?.to_str()?;
        if !guest_command_search_dirs(vm, guest_cwd, path_env)
            .iter()
            .any(|search_dir| normalize_path(search_dir) == normalize_path(parent_dir))
        {
            return None;
        }

        let file_name = Path::new(&normalized).file_name()?.to_str()?;
        vm.command_guest_paths.get(file_name).cloned()
    })
}

pub(super) fn resolve_exact_guest_command_entrypoint(
    vm: &VmState,
    guest_cwd: &str,
    command: &str,
) -> Option<String> {
    if !is_path_like_specifier(command) {
        return None;
    }

    let normalized = resolve_path_like_guest_specifier(guest_cwd, command);
    if let Some(name) = registered_command_name_for_path(vm, &normalized) {
        return vm.command_guest_paths.get(&name).cloned();
    }
    if vm
        .kernel
        .exists(&normalized)
        .ok()
        .is_some_and(|exists| exists)
    {
        // execve follows the final symlink. Returning the real path also lets
        // projected package commands select their actual JS/WASM entrypoint.
        return vm
            .kernel
            .realpath(&normalized)
            .ok()
            .map(|path| normalize_path(&path))
            .or(Some(normalized));
    }

    resolve_vm_guest_path_to_host(vm, &normalized)
        .is_file()
        .then_some(normalized)
}

pub(super) fn registered_command_name_for_path(vm: &VmState, path: &str) -> Option<String> {
    let normalized = normalize_path(path);
    let name = ["/bin/", "/usr/bin/", "/usr/local/bin/", "/opt/agentos/bin/"]
        .into_iter()
        .find_map(|prefix| normalized.strip_prefix(prefix))
        .or_else(|| {
            normalized
                .strip_prefix("/__agentos/commands/")
                .and_then(|suffix| suffix.rsplit('/').next())
        })?;
    (!name.is_empty() && !name.contains('/') && vm.kernel.commands().contains_key(name))
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
        .ok_or_else(|| SidecarError::Kernel(format!("ENOEXEC: invalid shebang line: {path}")))?;
    let line = &line[..line_end];
    let interpreter_start = line
        .iter()
        .position(|byte| !matches!(*byte, b' ' | b'\t'))
        .ok_or_else(|| SidecarError::Kernel(format!("ENOEXEC: invalid shebang line: {path}")))?;
    let interpreter_tail = &line[interpreter_start..];
    let separator = interpreter_tail
        .iter()
        .position(|byte| matches!(*byte, b' ' | b'\t'));
    if newline.is_none() && header.len() >= LINUX_BINPRM_BUF_SIZE && separator.is_none() {
        return Err(SidecarError::Kernel(format!(
            "ENOEXEC: shebang interpreter path exceeds the Linux header limit: {path}"
        )));
    }

    let interpreter_end = separator.unwrap_or(interpreter_tail.len());
    let interpreter = std::str::from_utf8(&interpreter_tail[..interpreter_end])
        .map_err(|_| SidecarError::Kernel(format!("ENOEXEC: invalid shebang line: {path}")))?;
    if interpreter.is_empty() {
        return Err(SidecarError::Kernel(format!(
            "ENOEXEC: invalid shebang line: {path}"
        )));
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
                .map_err(|_| SidecarError::Kernel(format!("ENOEXEC: invalid shebang line: {path}")))
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

fn spawn_request_guest_cwd(
    parent_guest_cwd: &str,
    request: &JavascriptChildProcessSpawnRequest,
) -> String {
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
        return Err(SidecarError::Kernel(String::from(
            "ENOENT: posix_spawnp command is empty",
        )));
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
        Err(SidecarError::Kernel(format!(
            "ENOENT: posix_spawnp command not found in PATH: {command}"
        )))
    }
}

/// Finish pathname and shebang resolution after POSIX file actions have run.
/// `posix_spawnp` searches PATH exactly once in this staged child state, then
/// follows the same recursive shebang rules as literal `posix_spawn`.
pub(super) fn resolve_posix_spawn_program(
    vm: &mut VmState,
    parent_guest_cwd: &str,
    request: &mut JavascriptChildProcessSpawnRequest,
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
    request: &mut JavascriptChildProcessSpawnRequest,
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
        if registered_command_name_for_path(vm, &resolved_path).is_some() {
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
            return Err(SidecarError::Kernel(format!(
                "ENOEXEC: exec format error: {resolved_path}"
            )));
        };
        if interpreter_depth >= LINUX_MAX_INTERPRETER_DEPTH {
            return Err(SidecarError::Kernel(format!(
                "ELOOP: interpreter recursion for {resolved_path} exceeds the Linux limit"
            )));
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
        Err(SidecarError::InvalidState(format!(
            "ENOEXEC: exec format error: {path}"
        )))
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

fn resolve_guest_command_path_candidate(vm: &VmState, candidate: &str) -> Option<String> {
    if candidate.starts_with(&format!("{}/", crate::package_projection::OPT_AGENTOS_BIN)) {
        if let Ok(realpath) = vm.kernel.realpath(candidate) {
            return Some(normalize_path(&realpath));
        }
    }

    if candidate.starts_with("/bin/")
        || candidate.starts_with("/usr/bin/")
        || candidate.starts_with("/usr/local/bin/")
        || candidate.starts_with(&format!("{}/", crate::package_projection::OPT_AGENTOS_BIN))
        || candidate.starts_with("/__agentos/commands/")
    {
        if let Some(file_name) = Path::new(candidate)
            .file_name()
            .and_then(|name| name.to_str())
        {
            if let Some(guest_entrypoint) = vm.command_guest_paths.get(file_name) {
                return Some(guest_entrypoint.clone());
            }
        }
    }

    if vm
        .kernel
        .exists(candidate)
        .ok()
        .is_some_and(|exists| exists)
    {
        return Some(normalize_path(candidate));
    }

    resolve_vm_guest_path_to_host(vm, candidate)
        .is_file()
        .then(|| normalize_path(candidate))
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
    host_cwd: &Path,
    guest_entrypoint: Option<String>,
) -> Result<(), SidecarError> {
    let user = vm.kernel.user_profile();
    let path_mappings = runtime_guest_path_mappings(vm);
    let read_paths = expand_host_access_paths(
        std::iter::once(vm.cwd.clone())
            .chain(
                path_mappings
                    .iter()
                    .map(|mapping| PathBuf::from(&mapping.host_path)),
            )
            .chain(std::iter::once(host_cwd.to_path_buf()))
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let write_paths = dedupe_host_paths(
        std::iter::once(vm.cwd.clone())
            .chain(std::iter::once(host_cwd.to_path_buf()))
            .chain(runtime_guest_writable_host_paths(vm))
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let allowed_node_builtins = configured_allowed_node_builtins(vm);
    let loopback_exempt_ports = configured_loopback_exempt_ports(vm);

    env.insert(
        String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
        serde_json::to_string(&path_mappings).map_err(|error| {
            SidecarError::InvalidState(format!("failed to encode guest path mappings: {error}"))
        })?,
    );
    env.entry(String::from(EXECUTION_SANDBOX_ROOT_ENV))
        .or_insert_with(|| normalize_host_path(&vm.cwd).to_string_lossy().into_owned());
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
    let mut identity =
        shared_guest_runtime_identity(&user, resource_limits, virtual_pid, virtual_ppid);
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

/// Build the typed per-execution WebAssembly limits from the per-VM kernel
/// `ResourceLimits`. Replaces the old `apply_wasm_limit_env` env round-trip;
/// notably this is the path that finally enforces the stack cap that the
/// `AGENTOS_WASM_MAX_STACK_BYTES` env knob set but no reader consumed.
pub(super) fn wasm_execution_limits(vm: &VmState) -> WasmExecutionLimits {
    let resource_limits = vm.kernel.resource_limits();
    WasmExecutionLimits {
        max_fuel: resource_limits.max_wasm_fuel,
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
        runner_cpu_time_limit_ms: Some(vm.limits.wasm.runner_cpu_time_limit_ms),
        reactor_work_quantum: vm_reactor_work_quantum(&vm.limits),
        bridge_call_timeout_ms: Some(bridge_call_timeout_ms(&vm.limits)),
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
    let mut command_root_mappings = vm
        .command_guest_paths
        .values()
        .filter_map(|guest_path| {
            Path::new(guest_path)
                .parent()
                .and_then(|parent| parent.to_str())
                .map(normalize_path)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|guest_path| RuntimeGuestPathMapping {
            host_path: resolve_vm_guest_path_to_host(vm, &guest_path)
                .to_string_lossy()
                .into_owned(),
            guest_path,
            read_only: false,
        })
        .collect::<Vec<_>>();
    mappings.append(&mut command_root_mappings);
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
    mappings.push(RuntimeGuestPathMapping {
        guest_path: String::from("/"),
        host_path: vm.cwd.to_string_lossy().into_owned(),
        read_only: false,
    });
    mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.guest_path.len()));
    mappings.dedup_by(|left, right| {
        left.guest_path == right.guest_path && left.host_path == right.host_path
    });
    mappings
}

/// Build a `Send`-able, read-only VFS module reader over the VM's read-only
/// `host_dir`/`module_access` mounts (and the derived `/root/node_modules` root
/// for nested mounts). When present, the V8 bridge thread resolves modules
/// inline against this reader — concurrently with the service loop — so a large
/// cold-start module graph never serializes behind / starves an in-flight ACP
/// `session/new` bootstrap on the single service-loop thread. The reader reads
/// the same mounted tree the guest sees (anchored resolve-beneath, escaping-symlink
/// refusal), never the host-direct path translator. Returns `None` when the VM
/// has no usable read-only mount, so resolution falls back to the service-loop
/// kernel reader.
pub(super) fn build_module_reader(
    vm: &VmState,
    resolved: &ResolvedChildProcessExecution,
) -> Option<crate::plugins::host_dir::HostDirModuleReader> {
    let mut pairs: Vec<(String, PathBuf)> = vm
        .configuration
        .mounts
        .iter()
        .filter(|mount| mount.read_only)
        .filter(|mount| (mount.plugin.id == "host_dir") || (mount.plugin.id == "module_access"))
        .filter_map(|mount| {
            mount_config_host_path(&mount.plugin.config)
                .map(|host_path| (normalize_path(&mount.guest_path), PathBuf::from(host_path)))
        })
        .collect();

    // Packed package-version leaves: module resolution reads packed
    // `node_modules` content straight from the `.aospkg` mount index (shared
    // mmap cache; no kernel access), mirroring what the guest sees through the
    // kernel tar mount. `(guest_path, aospkg_path, tar_root)` triples.
    let mut package_tars: Vec<(String, String, String)> = vm
        .configuration
        .mounts
        .iter()
        .filter(|mount| mount.plugin.id == "agentos_packages")
        .filter_map(|mount| {
            let config = serde_json::from_str::<Value>(&mount.plugin.config).ok()?;
            if config.get("kind").and_then(Value::as_str) != Some("tar") {
                return None;
            }
            let tar_path = config.get("tarPath").and_then(Value::as_str)?.to_owned();
            let root = config
                .get("root")
                .and_then(Value::as_str)
                .unwrap_or("/")
                .to_owned();
            Some((normalize_path(&mount.guest_path), tar_path, root))
        })
        .collect();
    // `<pkg>/current -> <version>` symlink leaves: alias the current prefix to
    // the same tar so modules that self-locate through `current` (rather than
    // the realpathed version dir) still resolve.
    let current_aliases: Vec<(String, String, String)> = vm
        .configuration
        .mounts
        .iter()
        .filter(|mount| mount.plugin.id == "agentos_packages")
        .filter_map(|mount| {
            let config = serde_json::from_str::<Value>(&mount.plugin.config).ok()?;
            if config.get("kind").and_then(Value::as_str) != Some("singleSymlink") {
                return None;
            }
            let link_path = normalize_path(&mount.guest_path);
            let target = config.get("target").and_then(Value::as_str)?;
            let resolved_target = if target.starts_with('/') {
                normalize_path(target)
            } else {
                let parent = Path::new(&link_path).parent()?.to_str()?;
                normalize_path(&format!("{parent}/{target}"))
            };
            package_tars
                .iter()
                .find(|(guest, _, _)| *guest == resolved_target)
                .map(|(_, tar_path, root)| (link_path, tar_path.clone(), root.clone()))
        })
        .collect();
    package_tars.extend(current_aliases);

    let guest_entrypoint = resolved
        .env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .map(|path| normalize_path(path));
    if let Some(guest_entrypoint) = guest_entrypoint.as_deref() {
        // Package entrypoints may still carry their pre-realpath launch path
        // (`/opt/agentos/bin/<cmd>` or `<pkg>/current/...` symlink leaves), so
        // gate on EVERY agentos_packages mount prefix, not just the tar leaves.
        let package_mount_prefixes: Vec<String> = vm
            .configuration
            .mounts
            .iter()
            .filter(|mount| mount.plugin.id == "agentos_packages")
            .map(|mount| normalize_path(&mount.guest_path))
            .collect();
        let entrypoint_in_read_only_mount = pairs
            .iter()
            .map(|(guest_path, _)| guest_path)
            .chain(package_mount_prefixes.iter())
            .any(|guest_path| {
                guest_entrypoint == guest_path
                    || guest_entrypoint.starts_with(&format!("{guest_path}/"))
            });
        if !entrypoint_in_read_only_mount {
            return None;
        }
    }

    // Mirror runtime_guest_path_mappings: a mount nested under
    // `/root/node_modules/<pkg>` implies a `/root/node_modules` root the resolver
    // walks, so expose that root too (e.g. software-package mounts).
    let extra_roots: Vec<(String, PathBuf)> = pairs
        .iter()
        .filter(|(guest_path, _)| guest_path.starts_with("/root/node_modules/"))
        .filter_map(|(_, host_path)| {
            host_node_modules_root(host_path).map(|root| (String::from("/root/node_modules"), root))
        })
        .collect();
    pairs.extend(extra_roots);

    if std::env::var("AGENTOS_MODULE_READER_TRACE").is_ok() {
        eprintln!(
            "module-reader: entrypoint={:?} host_pairs={} package_tars={:?}",
            resolved.env.get("AGENTOS_GUEST_ENTRYPOINT"),
            pairs.len(),
            package_tars
                .iter()
                .map(|(guest, _, _)| guest.as_str())
                .collect::<Vec<_>>()
        );
    }
    crate::plugins::host_dir::HostDirModuleReader::from_mounts_and_package_tars(pairs, package_tars)
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
        parse_kernel_poll_args, parse_kernel_stdin_read_args,
        service_javascript_kernel_poll_sync_rpc, ActiveExecution, ActiveExecutionEvent,
        ActiveProcess, BindingExecution, JavascriptSyncRpcRequest, KernelPollFdResponse,
        SidecarKernel, EXECUTION_DRIVER_NAME, JAVASCRIPT_COMMAND,
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
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};
    use tokio::sync::Notify;

    fn test_runtime_context() -> agentos_runtime::RuntimeContext {
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .expect("create test runtime")
            .context()
    }

    #[test]
    fn explicit_null_kernel_wait_timeouts_mean_indefinite_readiness_waits() {
        let stdin_request = JavascriptSyncRpcRequest {
            id: 1,
            method: String::from("__kernel_stdin_read"),
            raw_bytes_args: HashMap::new(),
            args: vec![json!(4096), Value::Null],
        };
        assert_eq!(
            parse_kernel_stdin_read_args(&stdin_request).expect("parse stdin wait"),
            (4096, None)
        );

        let poll_request = JavascriptSyncRpcRequest {
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
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
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
            &JavascriptSyncRpcRequest {
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
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
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

/// Package content is tar-mounted guest-native and never materialized on the
/// host, so command resolution classifies package-mount entrypoints by
/// extension only (`resolve_javascript_command_entrypoint`) and the resolved
/// host path for a WebAssembly module may not exist. Correct both here, where
/// the kernel is available: sniff the real entrypoint's magic through the
/// kernel VFS, flip misclassified extensionless WebAssembly binaries from
/// JavaScript to WebAssembly, and stage the module bytes into the VM shadow
/// tree so the wasm engine (which loads modules from a host path) can read
/// them. Staging is per-VM and write-once per resolved version path — package
/// versions are immutable — and only commands that actually execute are
/// materialized; filesystem reads stay on the zero-extraction tar mount.
pub(super) fn stage_agentos_package_command(
    vm: &mut VmState,
    resolved: &mut ResolvedChildProcessExecution,
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
    let Ok(real_entrypoint) = vm.kernel.realpath(&guest_entrypoint) else {
        return Ok(());
    };
    let real_entrypoint = normalize_path(&real_entrypoint);
    let Ok(magic) = vm.kernel.pread_file(&real_entrypoint, 0, WASM_MAGIC.len()) else {
        return Ok(());
    };
    if magic != WASM_MAGIC {
        return Ok(());
    }
    let shadow_path = shadow_path_for_guest(vm, &real_entrypoint);
    if !shadow_path.is_file() {
        let bytes = vm
            .kernel
            .read_file(&real_entrypoint)
            .map_err(kernel_error)?;
        if let Some(parent) = shadow_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!("failed to create wasm shadow parent: {error}"))
            })?;
        }
        fs::write(&shadow_path, &bytes).map_err(|error| {
            SidecarError::Io(format!(
                "failed to stage wasm module {}: {error}",
                shadow_path.display()
            ))
        })?;
    }
    resolved.runtime = GuestRuntimeKind::WebAssembly;
    resolved.entrypoint = shadow_path.to_string_lossy().into_owned();
    Ok(())
}

pub(super) fn enforce_resolved_wasm_execute_dac(
    vm: &mut VmState,
    parent_kernel_pid: u32,
    resolved: &ResolvedChildProcessExecution,
) -> Result<(), SidecarError> {
    if resolved.binding_command || resolved.runtime != GuestRuntimeKind::WebAssembly {
        return Ok(());
    }
    let Some(guest_entrypoint) = resolved.env.get("AGENTOS_GUEST_ENTRYPOINT") else {
        return Ok(());
    };
    let guest_entrypoint = normalize_path(guest_entrypoint);
    if vm
        .command_guest_paths
        .values()
        .any(|path| normalize_path(path) == guest_entrypoint)
    {
        return Ok(());
    }
    vm.kernel
        .check_execute_for_process(EXECUTION_DRIVER_NAME, parent_kernel_pid, &guest_entrypoint)
        .map_err(kernel_error)
}

pub(super) fn prepare_javascript_shadow(
    vm: &mut VmState,
    resolved: &ResolvedChildProcessExecution,
    env: &BTreeMap<String, String>,
) -> Result<(), SidecarError> {
    let guest_entrypoint = env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .cloned()
        // An absolute `entrypoint` may be a host path that lives inside the VM's
        // host cwd (callers can pass a fully-qualified host path). The guest sees
        // it at its translated guest path (host_cwd -> guest_cwd), so the shadow
        // must be keyed by that guest path rather than the raw host path. Falling
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
    if host_mount_path_for_guest_path(vm, &guest_entrypoint).is_some() {
        return Ok(());
    }
    if vm.kernel.lstat(&guest_entrypoint).is_err() {
        let host_entrypoint = {
            let candidate = Path::new(&resolved.entrypoint);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                resolved.host_cwd.join(candidate)
            }
        };
        if host_entrypoint.exists() {
            materialize_host_path_to_shadow(vm, &guest_entrypoint, &host_entrypoint)?;
            // The shadow write only stages the file on the host side; the runtime
            // resolves modules against the kernel VFS, so the staged entrypoint
            // must be synced into the kernel before execution starts (otherwise
            // `require()` reports "Cannot find module").
            return sync_shadow_entrypoint_into_kernel(vm, &guest_entrypoint);
        }
    }
    materialize_guest_path_to_shadow(vm, &guest_entrypoint)
}

pub(super) fn resolve_agentos_package_javascript_launch_entrypoint(
    vm: &mut VmState,
    env: &mut BTreeMap<String, String>,
) -> Option<String> {
    let guest_entrypoint = env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .map(|path| normalize_path(path))?;
    if !guest_path_is_within_agentos_package_mount(vm, &guest_entrypoint) {
        return None;
    }

    let real_entrypoint = normalize_path(&vm.kernel.realpath(&guest_entrypoint).ok()?);
    if !guest_path_is_within_agentos_package_mount(vm, &real_entrypoint) {
        return None;
    }

    env.insert(
        String::from("AGENTOS_GUEST_ENTRYPOINT"),
        real_entrypoint.clone(),
    );
    if guest_javascript_entrypoint_uses_module_mode(vm, &real_entrypoint) {
        env.insert(
            String::from("AGENTOS_GUEST_ENTRYPOINT_MODULE_MODE"),
            String::from("1"),
        );
    } else {
        env.remove("AGENTOS_GUEST_ENTRYPOINT_MODULE_MODE");
    }
    Some(real_entrypoint)
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

fn guest_javascript_entrypoint_uses_module_mode(vm: &mut VmState, guest_path: &str) -> bool {
    match Path::new(guest_path)
        .extension()
        .and_then(|ext| ext.to_str())
    {
        Some("mjs" | "mts") => true,
        Some("js") => nearest_guest_package_json_type(vm, guest_path).as_deref() == Some("module"),
        _ => false,
    }
}

fn nearest_guest_package_json_type(vm: &mut VmState, guest_path: &str) -> Option<String> {
    let mut dir = dirname(guest_path);
    loop {
        let package_json_path = if dir == "/" {
            String::from("/package.json")
        } else {
            normalize_path(&format!("{dir}/package.json"))
        };
        if let Ok(bytes) = vm.kernel.read_file(&package_json_path) {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(package_type) = value.get("type").and_then(Value::as_str) {
                    return Some(package_type.to_owned());
                }
            }
        }
        if dir == "/" {
            return None;
        }
        dir = dirname(&dir);
    }
}

/// Sync a freshly-staged shadow entrypoint into the kernel VFS so the runtime's
/// kernel-backed module resolver can read it. Mirrors the host->kernel file sync
/// used by the broader shadow reconciliation, but scoped to the single
/// entrypoint we just materialized.
fn sync_shadow_entrypoint_into_kernel(
    vm: &mut VmState,
    guest_entrypoint: &str,
) -> Result<(), SidecarError> {
    if vm.kernel.exists(guest_entrypoint).unwrap_or(false) {
        return Ok(());
    }
    let shadow_path = shadow_path_for_guest(vm, guest_entrypoint);
    let bytes = match fs::read(&shadow_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to read staged shadow entrypoint {}: {error}",
                shadow_path.display()
            )));
        }
    };
    if let Some(parent) = guest_parent_path(guest_entrypoint) {
        if !vm.kernel.exists(&parent).unwrap_or(false) {
            vm.kernel.mkdir(&parent, true).map_err(kernel_error)?;
        }
    }
    vm.kernel
        .write_file(guest_entrypoint, bytes)
        .map_err(kernel_error)?;
    Ok(())
}

fn guest_parent_path(guest_path: &str) -> Option<String> {
    let parent = Path::new(guest_path).parent()?;
    let parent = parent.to_string_lossy();
    if parent.is_empty() || parent == "/" {
        None
    } else {
        Some(parent.into_owned())
    }
}

fn materialize_host_path_to_shadow(
    vm: &VmState,
    guest_path: &str,
    host_path: &Path,
) -> Result<(), SidecarError> {
    let shadow_path = shadow_path_for_guest(vm, guest_path);
    let metadata = fs::symlink_metadata(host_path)
        .map_err(|error| SidecarError::Io(format!("failed to stat host entrypoint: {error}")))?;

    if metadata.file_type().is_symlink() {
        if let Some(parent) = shadow_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!("failed to create shadow symlink parent: {error}"))
            })?;
        }
        let _ = fs::remove_file(&shadow_path);
        let _ = fs::remove_dir_all(&shadow_path);
        let target = fs::read_link(host_path)
            .map_err(|error| SidecarError::Io(format!("failed to read host symlink: {error}")))?;
        std::os::unix::fs::symlink(&target, &shadow_path)
            .map_err(|error| SidecarError::Io(format!("failed to mirror host symlink: {error}")))?;
        return Ok(());
    }

    if metadata.is_dir() {
        fs::create_dir_all(&shadow_path).map_err(|error| {
            SidecarError::Io(format!("failed to create shadow directory: {error}"))
        })?;
        fs::set_permissions(
            &shadow_path,
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
        )
        .map_err(|error| {
            SidecarError::Io(format!(
                "failed to set shadow directory mode on {}: {error}",
                shadow_path.display()
            ))
        })?;
        return Ok(());
    }

    if let Some(parent) = shadow_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!("failed to create shadow parent: {error}"))
        })?;
    }
    let bytes = fs::read(host_path)
        .map_err(|error| SidecarError::Io(format!("failed to read host entrypoint: {error}")))?;
    fs::write(&shadow_path, bytes).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror host file into shadow root: {error}"
        ))
    })?;
    fs::set_permissions(
        &shadow_path,
        fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
    )
    .map_err(|error| {
        SidecarError::Io(format!(
            "failed to set shadow file mode on {}: {error}",
            shadow_path.display()
        ))
    })?;
    Ok(())
}

fn materialize_guest_path_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
) -> Result<(), SidecarError> {
    let stat = vm.kernel.lstat(guest_path).map_err(kernel_error)?;
    let shadow_path = shadow_path_for_guest(vm, guest_path);

    if stat.is_symbolic_link {
        if let Some(parent) = shadow_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!("failed to create shadow symlink parent: {error}"))
            })?;
        }
        let _ = fs::remove_file(&shadow_path);
        let _ = fs::remove_dir_all(&shadow_path);
        let target = vm.kernel.read_link(guest_path).map_err(kernel_error)?;
        std::os::unix::fs::symlink(&target, &shadow_path)
            .map_err(|error| SidecarError::Io(format!("failed to mirror symlink: {error}")))?;
        return Ok(());
    }

    if stat.is_directory {
        fs::create_dir_all(&shadow_path).map_err(|error| {
            SidecarError::Io(format!("failed to create shadow directory: {error}"))
        })?;
        fs::set_permissions(&shadow_path, fs::Permissions::from_mode(stat.mode & 0o7777)).map_err(
            |error| {
                SidecarError::Io(format!(
                    "failed to set shadow directory mode on {}: {error}",
                    shadow_path.display()
                ))
            },
        )?;
        return Ok(());
    }

    if let Some(parent) = shadow_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!("failed to create shadow parent: {error}"))
        })?;
    }
    let bytes = vm.kernel.read_file(guest_path).map_err(kernel_error)?;
    fs::write(&shadow_path, bytes).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror guest file into shadow root: {error}"
        ))
    })?;
    fs::set_permissions(&shadow_path, fs::Permissions::from_mode(stat.mode & 0o7777)).map_err(
        |error| {
            SidecarError::Io(format!(
                "failed to set shadow file mode on {}: {error}",
                shadow_path.display()
            ))
        },
    )?;
    Ok(())
}

pub(super) fn load_javascript_entrypoint_source(
    vm: &mut VmState,
    host_cwd: &Path,
    entrypoint: &str,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    let mut read_guest_file = |path: &str| {
        vm.kernel
            .read_file(path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };

    if let Some(source) = env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .filter(|path| path.starts_with('/'))
        .and_then(|path| read_guest_file(path))
    {
        return Some(source);
    }

    if entrypoint.starts_with('/') {
        if let Some(source) = read_guest_file(entrypoint) {
            return Some(source);
        }
    }

    let host_entrypoint = if Path::new(entrypoint).is_absolute() {
        PathBuf::from(entrypoint)
    } else {
        host_cwd.join(entrypoint)
    };
    let normalized_entrypoint = normalize_host_path(&host_entrypoint);
    let sandbox_root = normalize_host_path(&vm.cwd);
    let host_cwd = normalize_host_path(&vm.host_cwd);
    if !path_is_within_root(&normalized_entrypoint, &sandbox_root)
        && !path_is_within_root(&normalized_entrypoint, &host_cwd)
    {
        return None;
    }

    fs::read_to_string(&normalized_entrypoint).ok()
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

// discover_command_guest_paths moved to crate::bootstrap

pub(super) fn is_path_like_specifier(specifier: &str) -> bool {
    specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with("file:")
}

pub(super) fn execution_wasm_permission_tier(
    tier: WasmPermissionTier,
) -> ExecutionWasmPermissionTier {
    match tier {
        WasmPermissionTier::Full => ExecutionWasmPermissionTier::Full,
        WasmPermissionTier::ReadWrite => ExecutionWasmPermissionTier::ReadWrite,
        WasmPermissionTier::ReadOnly => ExecutionWasmPermissionTier::ReadOnly,
        WasmPermissionTier::Isolated => ExecutionWasmPermissionTier::Isolated,
    }
}

fn resolve_wasm_permission_tier(
    vm: &VmState,
    command_name: Option<&str>,
    explicit_tier: Option<WasmPermissionTier>,
    entrypoint: &str,
) -> WasmPermissionTier {
    explicit_tier
        .or_else(|| command_name.and_then(|command| vm.command_permissions.get(command).copied()))
        .or_else(|| {
            Path::new(entrypoint)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|command| vm.command_permissions.get(command).copied())
        })
        .unwrap_or(WasmPermissionTier::Full)
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

pub(super) fn guest_runtime_path_for_host_path(
    runtime_env: &BTreeMap<String, String>,
    virtual_home: &str,
    cwd: &Path,
    host_path: &str,
) -> Option<String> {
    let resolved = if host_path.starts_with("file://") {
        PathBuf::from(host_path.trim_start_matches("file://"))
    } else if host_path.starts_with("file:") {
        PathBuf::from(host_path.trim_start_matches("file:"))
    } else {
        let candidate = PathBuf::from(host_path);
        if candidate.is_absolute() {
            candidate
        } else if host_path.starts_with("./") || host_path.starts_with("../") {
            cwd.join(candidate)
        } else {
            return None;
        }
    };
    let normalized = normalize_host_path(&resolved);

    if let Some(path) = guest_path_from_runtime_host_mappings(runtime_env, &normalized) {
        return Some(path);
    }

    let normalized_cwd = normalize_host_path(cwd);
    if !path_is_within_root(&normalized, &normalized_cwd) {
        return None;
    }

    let virtual_home = if virtual_home.starts_with('/') {
        virtual_home.to_string()
    } else {
        String::from("/root")
    };
    let suffix = normalized
        .strip_prefix(&normalized_cwd)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_owned();

    Some(if suffix.is_empty() {
        virtual_home
    } else {
        normalize_path(&format!("{virtual_home}/{suffix}"))
    })
}

fn guest_path_from_runtime_host_mappings(
    runtime_env: &BTreeMap<String, String>,
    host_path: &Path,
) -> Option<String> {
    let mappings = runtime_env
        .get("AGENTOS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<RuntimeGuestPathMapping>>(value).ok())?;
    let normalized = normalize_host_path(host_path);

    let mut sorted_mappings = mappings
        .into_iter()
        .filter_map(|mapping| {
            (!mapping.guest_path.is_empty() && !mapping.host_path.is_empty()).then_some((
                normalize_path(&mapping.guest_path),
                normalize_host_path(Path::new(&mapping.host_path)),
            ))
        })
        .collect::<Vec<_>>();
    sorted_mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.1.as_os_str().len()));

    for (guest_root, host_root) in sorted_mappings {
        if !path_is_within_root(&normalized, &host_root) {
            continue;
        }
        let suffix = normalized
            .strip_prefix(&host_root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_owned();

        return Some(if suffix.is_empty() {
            guest_root
        } else if guest_root == "/" {
            normalize_path(&format!("/{suffix}"))
        } else {
            normalize_path(&format!("{guest_root}/{suffix}"))
        });
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
    context: &JavascriptSocketPathContext,
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

// JavascriptChildProcessSpawnOptions, JavascriptChildProcessSpawnRequest moved to crate::protocol
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
        "AGENTOS_WASM_INITIAL_SIGNAL_MASK",
        "AGENTOS_WASM_INITIAL_SIGNAL_IGNORES",
        "AGENTOS_WASM_INITIAL_PENDING_SIGNALS",
    ];

    env.iter()
        .filter(|(key, _)| {
            ALLOWED_KEYS.contains(&key.as_str()) || key.starts_with("AGENTOS_VIRTUAL_OS_")
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

// Network request types moved to crate::protocol

// VmDnsConfig, DnsResolutionSource moved to crate::state

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) fn execute(
        &mut self,
        request: &RequestFrame,
        payload: ExecuteRequest,
    ) -> OwnedVmRouteFuture {
        let input = self.prepare_owned_vm_route(request);
        let bridge = self.bridge.clone();
        let sidecar_requests = self.sidecar_requests.clone();
        let process_event_notify = Arc::clone(&self.process_event_notify);
        let cache_root = self.cache_root.clone();
        let process_event_capacity = self.config.runtime.protocol.max_process_events;
        Box::pin(async move {
            execute_owned(
                input?,
                payload,
                bridge,
                sidecar_requests,
                process_event_notify,
                cache_root,
                process_event_capacity,
            )
            .await
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_owned<B>(
    input: OwnedVmRouteInput,
    payload: ExecuteRequest,
    bridge: SharedBridge<B>,
    sidecar_requests: SharedSidecarRequestClient,
    process_event_notify: Arc<tokio::sync::Notify>,
    cache_root: PathBuf,
    process_event_capacity: usize,
) -> Result<DispatchResult, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let execute_total_start = Instant::now();
    let request = &input.request;
    let (connection_id, session_id, vm_id) = match &request.ownership {
        OwnershipScope::VmOwnership(ownership) => (
            ownership.connection_id.clone(),
            ownership.session_id.clone(),
            input.vm_id.clone(),
        ),
        _ => {
            return Err(SidecarError::InvalidState(String::from(
                "execute requires VM ownership",
            )))
        }
    };
    let execution_engines = input.vm.try_read("clone VM execution services", |vm| {
        vm.execution_engines.clone()
    })?;
    let mut vm = input.vm.try_borrow_mut("prepare and start execution")?;
    if vm.active_processes.contains_key(&payload.process_id) {
        return Err(SidecarError::InvalidState(format!(
            "VM {vm_id} already has an active process with id {}",
            payload.process_id
        )));
    }
    let vm_pending_stdin_bytes_budget = Arc::clone(&vm.pending_stdin_bytes_budget);
    let vm_pending_event_bytes_budget = Arc::clone(&vm.pending_event_bytes_budget);

    if let Some(command) = payload.command.as_deref() {
        if let Some(binding_resolution) =
            resolve_binding_command(&mut vm, command, &payload.args, payload.cwd.as_deref())?
        {
            let guest_cwd = payload
                .cwd
                .as_deref()
                .map(normalize_path)
                .unwrap_or_else(|| vm.guest_cwd.clone());
            let guest_env = vm.guest_env.clone();
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
                        env: guest_env,
                        cwd: Some(guest_cwd.clone()),
                        ..VirtualProcessOptions::default()
                    },
                )
                .map_err(kernel_error)?;
            let kernel_pid = kernel_handle.pid();
            let binding_execution = BindingExecution::with_event_notify(
                Arc::clone(&process_event_notify),
                process_event_capacity,
            )
            .with_vm_pending_event_bytes_budget(Arc::clone(&vm_pending_event_bytes_budget));
            let cancelled = binding_execution.cancelled.clone();
            let pending_events = binding_execution.pending_events.clone();
            let event_overflow_reason = binding_execution.event_overflow_reason.clone();
            let pending_event_bytes = binding_execution.pending_event_bytes.clone();
            let pending_event_count_limit = binding_execution.pending_event_count_limit.clone();
            let pending_event_bytes_limit = binding_execution.pending_event_bytes_limit.clone();
            let binding_vm_pending_event_bytes_budget =
                binding_execution.vm_pending_event_bytes_budget.clone();
            let event_notify = binding_execution.event_notify.clone();
            let runtime_context = vm.runtime_context.clone();
            let limits = vm.limits.clone();
            let shadow_root = normalize_host_path(&vm.cwd);
            let host_cwd = resolve_vm_guest_path_to_host(&mut vm, &guest_cwd);
            vm.active_processes.insert(
                payload.process_id.clone(),
                ActiveProcess::new(
                    kernel_pid,
                    kernel_handle,
                    runtime_context.clone(),
                    limits,
                    process_event_capacity,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Binding(binding_execution),
                )
                .with_event_notify(Arc::clone(&process_event_notify))
                .with_vm_pending_byte_budgets(
                    Arc::clone(&vm_pending_stdin_bytes_budget),
                    Arc::clone(&vm_pending_event_bytes_budget),
                )
                .with_guest_cwd(guest_cwd.clone())
                .with_shadow_root(shadow_root)
                .with_host_cwd(host_cwd),
            );
            bridge.emit_lifecycle(&vm_id, LifecycleState::Busy)?;
            spawn_binding_process_events(BindingProcessEventRequest {
                runtime_context,
                sidecar_requests: sidecar_requests.clone(),
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                vm_id: vm_id.clone(),
                binding_resolution,
                cancelled,
                pending_events,
                event_overflow_reason,
                pending_event_bytes,
                pending_event_count_limit,
                pending_event_bytes_limit,
                vm_pending_event_bytes_budget: binding_vm_pending_event_bytes_budget,
                event_notify,
            });
            return Ok(DispatchResult {
                response: process_started_response(request, payload.process_id, Some(kernel_pid)),
                events: Vec::new(),
            });
        }
    }

    let requested_tty = payload
        .env
        .get(EXECUTION_REQUEST_TTY_ENV)
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let phase_start = Instant::now();
    let mut resolved = resolve_execute_request(&mut vm, &payload)?;
    stage_agentos_package_command(&mut vm, &mut resolved)?;
    let resolved = resolved;
    record_execute_phase("resolve_execute_request", phase_start.elapsed());
    let phase_start = Instant::now();
    let mut env = resolved.env.clone();
    env.remove(EXECUTION_REQUEST_TTY_ENV);
    let sandbox_root = normalize_host_path(&vm.cwd);
    env.insert(
        String::from(EXECUTION_SANDBOX_ROOT_ENV),
        sandbox_root.to_string_lossy().into_owned(),
    );
    if resolved.runtime == GuestRuntimeKind::JavaScript {
        env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
        // A TTY guest-node process reads stdin through the kernel PTY: host
        // input is written to the PTY master (write_kernel_process_stdin),
        // line discipline runs (echo / VERASE / ICRNL / VEOF), and the
        // sidecar drains the cooked bytes from the slave and forwards them
        // to the isolate's stream-stdin dispatch
        // (forward_tty_slave_input_to_javascript). The in-isolate
        // `_kernelStdinRead` bridge stays local; no RPC forwarding is
        // needed because the isolate never reads kernel fd 0 itself.
    } else if resolved.runtime == GuestRuntimeKind::WebAssembly {
        env.insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
    }
    let launch_entrypoint = if resolved.runtime == GuestRuntimeKind::JavaScript {
        resolve_agentos_package_javascript_launch_entrypoint(&mut vm, &mut env)
            .unwrap_or_else(|| resolved.entrypoint.clone())
    } else {
        resolved.entrypoint.clone()
    };
    let argv = std::iter::once(launch_entrypoint.clone())
        .chain(resolved.execution_args.iter().cloned())
        .collect::<Vec<_>>();
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
                ..SpawnOptions::default()
            },
        )
        .map_err(kernel_error)?;
    let kernel_pid = kernel_handle.pid();
    if let Err(error) = enforce_resolved_wasm_execute_dac(&mut vm, kernel_pid, &resolved) {
        kernel_handle.finish(126);
        return Err(error);
    }
    record_execute_phase("kernel_spawn_process", phase_start.elapsed());
    let tty_master_fd = if requested_tty {
        let (master_fd, slave_fd, _) = vm
            .kernel
            .open_pty(EXECUTION_DRIVER_NAME, kernel_pid)
            .map_err(kernel_error)?;
        vm.kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd, 0)
            .map_err(kernel_error)?;
        vm.kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd, 1)
            .map_err(kernel_error)?;
        vm.kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd, 2)
            .map_err(kernel_error)?;
        vm.kernel
            .pty_set_foreground_pgid(EXECUTION_DRIVER_NAME, kernel_pid, master_fd, kernel_pid)
            .map_err(kernel_error)?;
        if let Some((cols, rows)) = requested_pty_window_size(&env) {
            vm.kernel
                .pty_resize(EXECUTION_DRIVER_NAME, kernel_pid, master_fd, cols, rows)
                .map_err(kernel_error)?;
        }
        Some(master_fd)
    } else {
        None
    };

    let (execution, process_env) = match resolved.runtime {
        GuestRuntimeKind::JavaScript => {
            let phase_start = Instant::now();
            let inline_code = load_javascript_entrypoint_source(
                &mut vm,
                &resolved.host_cwd,
                &launch_entrypoint,
                &env,
            );
            record_execute_phase("js_load_entrypoint_source", phase_start.elapsed());
            let phase_start = Instant::now();
            prepare_javascript_shadow(&mut vm, &resolved, &env)?;
            record_execute_phase("js_prepare_shadow", phase_start.elapsed());

            let phase_start = Instant::now();
            let mut javascript_engine =
                execution_engines.javascript("start JavaScript execution")?;
            let context = javascript_engine.create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: Some(cache_root.join("node-compile-cache")),
            });
            record_execute_phase("js_create_context", phase_start.elapsed());
            let phase_start = Instant::now();
            let built_reader = build_module_reader(&mut vm, &resolved);
            let guest_reader = built_reader.clone().map(|reader| {
                Box::new(crate::plugins::host_dir::SessionModuleReader::new(reader))
                    as Box<dyn GuestModuleReader>
            });
            let module_reader =
                built_reader.map(|reader| Box::new(reader) as Box<dyn ModuleFsReader + Send>);
            record_execute_phase("js_build_module_reader", phase_start.elapsed());
            let phase_start = Instant::now();
            let execution = javascript_engine
                .start_execution_with_module_reader_and_runtime(
                    StartJavascriptExecutionRequest {
                        guest_runtime: guest_runtime_identity(&mut vm, None, None),
                        vm_id: vm_id.clone(),
                        context_id: context.context_id,
                        argv: std::iter::once(launch_entrypoint.clone())
                            .chain(resolved.execution_args.iter().cloned())
                            .collect(),
                        argv0: None,
                        env: env.clone(),
                        cwd: resolved.host_cwd.clone(),
                        limits: javascript_execution_limits(&mut vm),
                        inline_code,
                        wasm_module_bytes: None,
                    },
                    module_reader,
                    guest_reader,
                    vm.runtime_context.clone(),
                )
                .map_err(javascript_error)?;
            record_execute_phase("js_start_execution", phase_start.elapsed());
            (ActiveExecution::Javascript(execution), env.clone())
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
            let runtime_context = vm.runtime_context.clone();
            let python_limits = python_execution_limits_with_env(&mut vm, &env);
            let python_guest_runtime = guest_runtime_identity(&mut vm, None, None);
            // Pyodide discovery and runtime warmup can await bounded workers.
            // They own only the VM-local Python service, not the mutable VM
            // coordinator, so filesystem/kernel commands can still enter this
            // VM and operations in other VMs remain independent.
            drop(vm);
            let mut python_engine = execution_engines.python("start Python execution")?;
            let pyodide_dist_path = python_engine
                .bundled_pyodide_dist_path_for_vm_async(&vm_id, &runtime_context)
                .await
                .map_err(python_error)?;
            let pyodide_cache_path = pyodide_dist_path
                .parent()
                .and_then(Path::parent)
                .unwrap_or(pyodide_dist_path.as_path())
                .join("pyodide-package-cache");
            add_runtime_guest_path_mapping(&mut env, PYTHON_PYODIDE_GUEST_ROOT, &pyodide_dist_path);
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
            let context = python_engine.create_context(CreatePythonContextRequest {
                vm_id: vm_id.clone(),
                pyodide_dist_path,
            });
            let execution = python_engine
                .start_execution_with_runtime_async(
                    StartPythonExecutionRequest {
                        vm_id: vm_id.clone(),
                        context_id: context.context_id,
                        code: resolved.entrypoint.clone(),
                        file_path: python_file_path,
                        env: env.clone(),
                        cwd: resolved.host_cwd.clone(),
                        limits: python_limits,
                        guest_runtime: python_guest_runtime,
                    },
                    runtime_context,
                )
                .await
                .map_err(python_error)?;
            drop(python_engine);
            vm = input
                .vm
                .try_borrow_mut("register started Python execution")?;
            (ActiveExecution::Python(execution), env.clone())
        }
        GuestRuntimeKind::WebAssembly => {
            let wasm_limits = wasm_execution_limits(&mut vm);
            let wasm_guest_runtime =
                guest_runtime_identity(&mut vm, Some(u64::from(kernel_pid)), Some(0));
            let wasm_permission_tier = resolved.wasm_permission_tier.unwrap_or_else(|| {
                resolve_wasm_permission_tier(
                    &mut vm,
                    Some(&resolved.command),
                    None,
                    &resolved.entrypoint,
                )
            });
            let runtime_context = vm.runtime_context.clone();
            // WASM import-cache materialization and prewarm are external
            // waits. Release mutable VM state before entering them.
            drop(vm);
            let mut wasm_engine = execution_engines.wasm("start WebAssembly execution")?;
            let context = wasm_engine.create_context(CreateWasmContextRequest {
                vm_id: vm_id.clone(),
                module_path: Some(resolved.entrypoint.clone()),
            });
            let execution = wasm_engine
                .start_execution_with_runtime_async(
                    StartWasmExecutionRequest {
                        vm_id: vm_id.clone(),
                        context_id: context.context_id,
                        argv: resolved.process_args.clone(),
                        env: env.clone(),
                        cwd: resolved.host_cwd.clone(),
                        permission_tier: execution_wasm_permission_tier(wasm_permission_tier),
                        limits: wasm_limits,
                        guest_runtime: wasm_guest_runtime,
                    },
                    runtime_context,
                )
                .await
                .map_err(wasm_error)?;
            drop(wasm_engine);
            vm = input
                .vm
                .try_borrow_mut("register started WebAssembly execution")?;
            (ActiveExecution::Wasm(Box::new(execution)), env)
        }
    };
    let child_pid = execution.child_pid();
    let phase_start = Instant::now();
    let kernel_stdin_writer_fd = if let Some(master_fd) = tty_master_fd {
        master_fd
    } else {
        install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?
    };
    let runtime_context = vm.runtime_context.clone();
    let limits = vm.limits.clone();
    vm.active_processes.insert(
        payload.process_id.clone(),
        ActiveProcess::new(
            kernel_pid,
            kernel_handle,
            runtime_context,
            limits,
            process_event_capacity,
            resolved.runtime,
            execution,
        )
        .with_event_notify(Arc::clone(&process_event_notify))
        .with_vm_pending_byte_budgets(vm_pending_stdin_bytes_budget, vm_pending_event_bytes_budget)
        .with_kernel_stdin_writer_fd(kernel_stdin_writer_fd)
        .with_tty_master_fd(tty_master_fd)
        .with_guest_cwd(resolved.guest_cwd.clone())
        .with_env(process_env)
        .with_shadow_root(sandbox_root)
        .with_host_cwd(resolved.host_cwd.clone()),
    );
    bridge.emit_lifecycle(&vm_id, LifecycleState::Busy)?;
    mark_execute_response_ready(&vm_id, &payload.process_id);
    record_execute_phase("process_register_and_lifecycle", phase_start.elapsed());
    record_execute_phase("execute_total", execute_total_start.elapsed());

    Ok(DispatchResult {
        response: process_started_response(
            request,
            payload.process_id,
            Some(if child_pid == 0 {
                kernel_pid
            } else {
                child_pid
            }),
        ),
        events: Vec::new(),
    })
}
