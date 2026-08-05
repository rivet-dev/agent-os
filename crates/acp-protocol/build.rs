use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));
    let source_schema = manifest_dir.join("protocol").join("agent_os_acp_v1.bare");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", source_schema.display());

    let schema_dir = out_dir.join("protocol-schema");
    fs::create_dir_all(&schema_dir).expect("failed to create generated protocol schema dir");
    fs::copy(&source_schema, schema_dir.join("v1.bare")).unwrap_or_else(|error| {
        panic!(
            "failed to stage protocol schema from {}: {}",
            source_schema.display(),
            error
        )
    });

    let cfg = vbare_compiler::Config::default();
    vbare_compiler::process_schemas_with_config(&schema_dir, &cfg)
        .expect("failed to generate agent-os protocol from BARE schema");
}
