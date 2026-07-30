#[cfg(feature = "runtime")]
use base64::{engine::general_purpose::STANDARD, Engine as _};
#[cfg(feature = "runtime")]
use std::{env, fmt::Write as _, fs, path::PathBuf};
#[cfg(feature = "runtime")]
use webpki_root_certs::TLS_SERVER_ROOT_CERTS;

// Stage the base filesystem fixture into OUT_DIR. In-tree builds use the
// canonical AgentOS core fixture from the current workspace; the
// published crate falls back to the vendored `assets/base-filesystem.json` copy.
fn main() {
    #[cfg(not(feature = "runtime"))]
    return;

    #[cfg(feature = "runtime")]
    stage_runtime_assets();
}

#[cfg(feature = "runtime")]
fn stage_runtime_assets() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));

    println!("cargo:rerun-if-changed=build.rs");

    let workspace_fixtures = [
        manifest_dir.join("../../packages/core/fixtures/base-filesystem.json"),
        manifest_dir.join("../../packages/core/fixtures/base-filesystem.json"),
    ];
    let vendored = manifest_dir.join("assets/base-filesystem.json");
    let src = workspace_fixtures
        .into_iter()
        .find(|fixture| fixture.exists())
        .unwrap_or(vendored);

    println!("cargo:rerun-if-changed={}", src.display());

    let dest = out_dir.join("base-filesystem.json");
    fs::copy(&src, &dest).unwrap_or_else(|error| {
        panic!(
            "failed to stage base-filesystem.json from {} to {}: {}",
            src.display(),
            dest.display(),
            error
        )
    });

    let destination = out_dir.join("ca-certificates.crt");
    let mut pem = String::new();
    for certificate in TLS_SERVER_ROOT_CERTS {
        pem.push_str("-----BEGIN CERTIFICATE-----\n");
        let encoded = STANDARD.encode(certificate.as_ref());
        for line in encoded.as_bytes().chunks(64) {
            writeln!(
                pem,
                "{}",
                std::str::from_utf8(line).expect("base64 must be UTF-8")
            )
            .expect("writing to a String must succeed");
        }
        pem.push_str("-----END CERTIFICATE-----\n");
    }
    assert!(!pem.is_empty(), "Mozilla CA root set must not be empty");
    fs::write(&destination, pem).unwrap_or_else(|error| {
        panic!(
            "failed to write generated CA bundle to {}: {error}",
            destination.display()
        )
    });
}
