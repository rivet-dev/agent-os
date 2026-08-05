use std::{
    env, fs,
    path::{Path, PathBuf},
};

// Stage the base filesystem fixture into OUT_DIR. In-tree builds use the
// canonical AgentOS core fixture from the current workspace; the
// published crate falls back to the vendored `assets/base-filesystem.json` copy.
fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));

    #[cfg(feature = "package-filesystem")]
    stage_package_format_schema(&manifest_dir, &out_dir);

    let workspace_fixtures = [
        manifest_dir.join("../../packages/core/fixtures/base-filesystem.json"),
        manifest_dir.join("../../packages/core/fixtures/base-filesystem.json"),
    ];
    let vendored = manifest_dir.join("assets/base-filesystem.json");
    let src = workspace_fixtures
        .into_iter()
        .find(|fixture| fixture.exists())
        .unwrap_or(vendored);

    copy_if_changed(&src, &out_dir.join("base-filesystem.json"));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", src.display());
    #[cfg(feature = "package-filesystem")]
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("package-format")
            .join("v1.bare")
            .display()
    );
}

#[cfg(feature = "package-filesystem")]
fn stage_package_format_schema(manifest_dir: &Path, out_dir: &Path) {
    let source_schema = manifest_dir.join("package-format").join("v1.bare");
    let schema_dir = out_dir.join("package-format-schema");
    fs::create_dir_all(&schema_dir).expect("failed to create generated package schema dir");
    let schema_changed = copy_if_changed(&source_schema, &schema_dir.join("v1.bare"));
    let generated_missing =
        !out_dir.join("combined_imports.rs").exists() || !out_dir.join("v1_generated.rs").exists();
    if !schema_changed && !generated_missing {
        return;
    }

    let cfg = vbare_compiler::Config::default();
    vbare_compiler::process_schemas_with_config(&schema_dir, &cfg)
        .expect("failed to generate package format BARE schema");
}

fn copy_if_changed(source: &Path, destination: &Path) -> bool {
    let contents = fs::read(source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()));
    if fs::read(destination).is_ok_and(|existing| existing == contents) {
        return false;
    }
    fs::write(destination, contents)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", destination.display()));
    true
}
