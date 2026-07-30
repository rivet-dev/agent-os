use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("protocol");
    println!("cargo:rerun-if-changed={}", schema_dir.display());
    vbare_compiler::process_schemas_with_config(&schema_dir, &Default::default())?;
    Ok(())
}
