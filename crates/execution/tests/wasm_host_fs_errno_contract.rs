//! Contract checks for Linux filesystem errno propagation in the WASM runner.

use std::{fs, path::PathBuf};

fn runner_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/runners/wasm-runner.mjs");
    fs::read_to_string(path).expect("read wasm runner")
}

#[test]
fn existing_path_errors_reach_libc_as_eexist() {
    let source = runner_source();
    let start = source
        .find("function mapHostProcessError(")
        .expect("host error mapper");
    let end = source[start..]
        .find("\n}\n\nfunction seekGuestFileHandle")
        .map(|offset| start + offset)
        .expect("end of host error mapper");
    let error_map = &source[start..end];

    assert!(
        source.contains("const WASI_ERRNO_EXIST = 20;")
            && error_map.contains("case 'EEXIST':\n      return WASI_ERRNO_EXIST;"),
        "host EEXIST must retain the preview1/WASI errno value instead of becoming EFAULT"
    );
}

#[test]
fn resource_limit_errors_reach_libc_as_enomem() {
    let source = runner_source();
    let start = source
        .find("function mapHostProcessError(")
        .expect("host error mapper");
    let end = source[start..]
        .find("\n}\n\nfunction seekGuestFileHandle")
        .map(|offset| start + offset)
        .expect("end of host error mapper");
    let error_map = &source[start..end];

    assert!(
        source.contains("const WASI_ERRNO_NOMEM = 48;")
            && error_map.contains("case 'ENOMEM':\n      return WASI_ERRNO_NOMEM;"),
        "host ENOMEM must retain the preview1/WASI errno value instead of becoming EFAULT"
    );
}

#[test]
fn filesystem_capacity_errors_reach_libc_as_enospc() {
    let source = runner_source();
    let start = source
        .find("function mapHostProcessError(")
        .expect("host error mapper");
    let end = source[start..]
        .find("\n}\n\nfunction seekGuestFileHandle")
        .map(|offset| start + offset)
        .expect("end of host error mapper");
    let error_map = &source[start..end];

    assert!(
        source.contains("const WASI_ERRNO_NOSPC = 51;")
            && error_map.contains("case 'ENOSPC':\n      return WASI_ERRNO_NOSPC;"),
        "host ENOSPC must retain the preview1/WASI errno value instead of becoming EFAULT"
    );
}

#[test]
fn oversized_xattr_names_are_einval_in_both_engine_adapters() {
    let v8 = runner_source();
    assert_eq!(v8.matches("'wasm.abi.maxXattrNameBytes'").count(), 6);
    assert_eq!(
        v8.matches("XATTR_NAME_MAX,\n        WASI_ERRNO_INVAL,")
            .count(),
        6,
        "all V8 xattr name prechecks must return Linux EINVAL"
    );

    let wasmtime_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/wasm/wasmtime/linker/filesystem.rs");
    let wasmtime = fs::read_to_string(wasmtime_path).expect("read Wasmtime filesystem linker");
    assert!(wasmtime.contains(
        "\"wasm.abi.maxXattrNameBytes\",\n                length as usize,\n                XATTR_NAME_MAX,\n                ERRNO_INVAL,"
    ));
    let bounded_name_checks = wasmtime
        .split("let Ok(name) = bounded_name")
        .skip(1)
        .collect::<Vec<_>>();
    assert_eq!(bounded_name_checks.len(), 6);
    for check in bounded_name_checks {
        let precheck = check
            .split_once("};")
            .map(|(precheck, _)| precheck)
            .expect("bounded xattr name precheck terminator");
        assert!(
            precheck.contains("return ERRNO_INVAL;"),
            "Wasmtime bounded xattr name precheck must return Linux EINVAL"
        );
    }
}

#[test]
fn xattr_empty_paths_are_enoent_in_both_engine_adapters() {
    let v8 = runner_source();
    assert_eq!(
        v8.matches("if ((Number(pathLen) >>> 0) === 0) return WASI_ERRNO_NOENT;")
            .count(),
        4,
        "every V8 pathname xattr operation must reject the empty pathname"
    );

    let wasmtime_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/wasm/wasmtime/linker/filesystem.rs");
    let wasmtime = fs::read_to_string(wasmtime_path).expect("read Wasmtime filesystem linker");
    assert_eq!(
        wasmtime.matches("if path.is_empty() {").count(),
        4,
        "every Wasmtime pathname xattr operation must reject the empty pathname"
    );
    assert!(wasmtime.contains("return ERRNO_NOENT;"));
}

#[test]
fn path_open_preserves_directory_and_nofollow_before_kernel_mutation() {
    let source = runner_source();

    assert!(
        source.contains("const KERNEL_O_DIRECTORY = 0o200000;")
            && source.contains("const KERNEL_O_NOFOLLOW = 0o400000;"),
        "the runner must expose the kernel's Linux-compatible open flag bits"
    );
    assert!(
        source.contains(
            "if ((normalizedOflags & WASI_OFLAGS_DIRECTORY) !== 0) flags |= KERNEL_O_DIRECTORY;"
        ) && source.contains("flags |= KERNEL_O_NOFOLLOW;"),
        "path_open must pass O_DIRECTORY and a missing SYMLINK_FOLLOW lookup flag to the kernel"
    );
    assert!(
        source.contains(
            "kernelOpenFlagsFromWasi(oflags, rightsBase, fdflags, dirflags, requestedDirect)"
        ),
        "path_open must include its WASI lookup flags in the kernel conversion"
    );
    assert!(
        source.contains(
            "function openProcSelfFdAlias(guestPath, oflags, rightsBase, lookupflags, openedFdPtr)"
        ) && source.contains("return WASI_ERRNO_LOOP;")
            && source.contains(
                "const procFdResult = SIDECAR_MANAGED_PROCESS\n      ? null\n      : openProcSelfFdAlias("
            ),
        "standalone aliases must honor O_NOFOLLOW and managed aliases must use kernel path_open"
    );
}
