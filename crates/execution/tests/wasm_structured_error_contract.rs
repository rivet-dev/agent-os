use std::{fs, path::PathBuf};

#[test]
fn pipe_sync_rpc_decoder_preserves_structured_error_details() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/runners/wasm-runner.mjs");
    let source = fs::read_to_string(path).expect("read wasm runner");
    let start = source.find("function callSyncRpc(").expect("callSyncRpc");
    let section = &source[start..];
    let code = section
        .find("error.code = response.error.code")
        .expect("structured error code");
    let details = section
        .find("error.details = decodeSyncRpcValue(response.error.details)")
        .expect("structured error details");
    let throw = details
        + section[details..]
            .find("throw error;")
            .expect("structured error throw");

    assert!(code < details && details < throw);
}
