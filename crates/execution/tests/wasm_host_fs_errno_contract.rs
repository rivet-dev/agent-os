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
