//! Regression test for issue #1:
//! "WASI fdOpen permission check typo (read blocked as write)".
//!
//! The original bug computed write-intent from the READ rights bit
//! (`rightsBase & 2n`, where `2n` == WASI_RIGHT_FD_READ == `1n << 1n`).
//! That meant a pure read open (which sets RIGHT_FD_READ) was treated as a
//! write and rejected with EACCES/EROFS on read-only or isolated tiers.
//!
//! The fix is that write-intent is derived from RIGHT_FD_WRITE
//! (`1n << 6n` == 64n) instead. The WASI module JS that performs `path_open`
//! lives in `crates/execution/assets/runners/wasi-module.js`, and the delegated
//! runner path lives in `crates/execution/assets/runners/wasm-runner.mjs`.
//!
//! `build_wasm_runner_bootstrap` is a private function, so rather than execute
//! it we pin the source-level invariant: write-intent MUST be checked against
//! the WRITE bit and MUST NOT be checked against the READ bit. This locks out
//! reintroducing the typo. If the buggy `& 2n` / READ-bit write check ever
//! returns, this test fails.

use std::fs;
use std::path::PathBuf;

fn read_source(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

/// Returns the body of the `_hasWriteRights` JS method in the WASI module asset,
/// or `None` if the method cannot be located.
fn extract_has_write_rights_body(source: &str) -> Option<&str> {
    let start = source.find("_hasWriteRights(rights)")?;
    let rest = &source[start..];
    // Stop at the next sibling method so unrelated read-right checks cannot
    // become false positives when methods are inserted or renamed below it.
    let end = rest.find("_hasReadRights(rights)")?;
    Some(&rest[..end])
}

#[test]
fn wasm_runner_write_intent_uses_write_bit_not_read_bit() {
    let wasm_src = read_source("assets/runners/wasi-module.js");

    // The WRITE right constant must be defined as bit 6 (64n), not the read bit.
    assert!(
        wasm_src.contains("const __agentOSWasiRightFdWrite = 1n << 6n;"),
        "expected RIGHT_FD_WRITE to be defined as `1n << 6n` (64n) in wasi-module.js; \
         the write-intent constant is the foundation of the read-vs-write distinction"
    );

    let body = extract_has_write_rights_body(&wasm_src)
        .expect("expected to find `_hasWriteRights(rights)` method in wasi-module.js");

    // Write-intent must be masked against the WRITE bit.
    assert!(
        body.contains("(BigInt(rights) & __agentOSWasiRightFdWrite) !== 0n"),
        "_hasWriteRights must check write-intent against RIGHT_FD_WRITE \
         (__agentOSWasiRightFdWrite); found body: {body}"
    );

    // Guard against reintroducing the original typo: write-intent must NOT be
    // derived from the READ bit. `2n` and `1n << 1n` are RIGHT_FD_READ.
    assert!(
        !body.contains("& 2n"),
        "_hasWriteRights must NOT mask against `2n` (RIGHT_FD_READ) — that was \
         the original typo that blocked reads as writes; found body: {body}"
    );
    assert!(
        !body.contains("1n << 1n"),
        "_hasWriteRights must NOT mask against the read bit `1n << 1n`; \
         found body: {body}"
    );
    assert!(
        !body.contains("RightFdRead"),
        "_hasWriteRights must NOT reference the READ rights constant for \
         write-intent; found body: {body}"
    );
}

#[test]
fn wasm_runner_path_open_gates_write_access_on_write_rights() {
    let wasm_src = read_source("assets/runners/wasi-module.js");

    // path_open derives requestedWriteAccess from create/truncate flags OR the
    // WRITE rights bit (via _hasWriteRights), never from the read bit. A pure
    // read (RIGHT_FD_READ only, no CREAT/TRUNC) therefore yields
    // requestedWriteAccess === false and is not denied on read-only tiers.
    assert!(
        wasm_src.contains("createOrTruncate || this._hasWriteRights(requestedRightsBase)"),
        "path_open must compute write-intent from create/truncate flags or \
         _hasWriteRights(requestedRightsBase), so pure reads are not flagged as writes"
    );

    // EROFS / EACCES must only fire when write access is actually requested.
    assert!(
        wasm_src.contains("if (requestedWriteAccess && resolved.readOnly) {"),
        "read-only EROFS must be gated behind requestedWriteAccess so reads succeed"
    );
}
#[test]
fn wasm_runner_fd_close_releases_directory_backing_fd() {
    let wasm_src = read_source("assets/runners/wasi-module.js");
    let close_start = wasm_src
        .find("_fdClose(fd)")
        .expect("expected WASI fd_close implementation");
    let close_end = wasm_src[close_start..]
        .find("_fdSync(fd)")
        .map(|offset| close_start + offset)
        .expect("expected fd_sync after fd_close");
    let close_body = &wasm_src[close_start..close_end];

    assert!(
        close_body.contains("entry.kind === \"file\" || entry.kind === \"directory\""),
        "fd_close must release both file and directory backing descriptors; body: {close_body}"
    );
    assert!(
        close_body.contains("__agentOSFs().closeSync(entry.realFd);"),
        "fd_close must close the kernel-backed descriptor before deleting its WASI entry"
    );
}

#[test]
fn writable_wasm_preopens_include_create_file_right() {
    let runner = read_source("assets/runners/wasm-runner.mjs");

    assert!(
        runner.contains("const WASI_RIGHT_PATH_CREATE_FILE = 1n << 12n;"),
        "WASI PATH_CREATE_FILE must use preview1 bit 12"
    );
    let rights_start = runner
        .find("const READ_WRITE_PREOPEN_RIGHTS_BASE =")
        .expect("read-write preopen rights declaration");
    let rights_end = runner[rights_start..]
        .find("const READ_WRITE_PREOPEN_RIGHTS_INHERITING =")
        .map(|offset| rights_start + offset)
        .expect("read-write inheriting rights declaration");
    assert!(
        runner[rights_start..rights_end].contains("WASI_RIGHT_PATH_CREATE_FILE"),
        "writable WASI preopens must grant PATH_CREATE_FILE or std::fs::write fails with EACCES"
    );
    assert!(
        runner.contains(
            "const READ_WRITE_PREOPEN_RIGHTS_INHERITING = READ_WRITE_PREOPEN_RIGHTS_BASE;"
        ),
        "Rust std requests directory path rights in path_open's inheriting set; writable preopens must allow their tier's path rights"
    );
}
