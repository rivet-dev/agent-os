use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Large Pyodide runtime assets are excluded from the published crate (see the
/// `exclude` list in Cargo.toml) to keep it under the registry size limit.
/// During in-tree (workspace) builds they are copied from `assets/pyodide/`.
///
/// When building the published crate (the unpacked tarball, where these assets
/// are absent) Python support is built WITHOUT the externalized Pyodide assets:
/// each missing asset is staged as an empty placeholder so the `include_bytes!`
/// of the OUT_DIR copy still compiles, and the `agentos_pyodide_unavailable`
/// cfg is set so the runtime reports Python as unavailable instead of trying to
/// boot an incomplete Pyodide. This keeps `cargo publish` verification free of
/// any CDN/network dependency. Python support remains fully functional in the
/// workspace build where the in-tree assets exist.
const EXTERNALIZED_PYODIDE_ASSETS: &[&str] = &[
    "pyodide.asm.wasm",
    "pyodide.asm.js",
    "python_stdlib.zip",
    "numpy-2.2.5-cp313-cp313-pyodide_2025_0_wasm32.whl",
    "pandas-2.3.3-cp313-cp313-pyodide_2025_0_wasm32.whl",
];

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));

    println!("cargo:rerun-if-changed=build.rs");
    // Declare the cfg used to gate Python availability so `cargo` does not warn
    // about an unexpected cfg name.
    println!("cargo:rustc-check-cfg=cfg(agentos_pyodide_unavailable)");
    println!("cargo:rustc-check-cfg=cfg(agentos_typescript_unavailable)");
    agentos_build_support::build_v8_bridge(&manifest_dir, &out_dir);
    stage_pyodide_assets(&manifest_dir, &out_dir);
    stage_typescript_assets(&manifest_dir, &out_dir);
}

fn stage_typescript_assets(manifest_dir: &Path, out_dir: &Path) {
    let source_dir = manifest_dir.join("../../node_modules/typescript/lib");
    let staged_dir = out_dir.join("typescript");
    let generated_path = out_dir.join("typescript_assets.rs");
    println!("cargo:rerun-if-changed={}", source_dir.display());
    fs::create_dir_all(&staged_dir).unwrap_or_else(|error| {
        panic!(
            "failed to create TypeScript staging dir {}: {error}",
            staged_dir.display()
        )
    });

    let mut assets = Vec::new();
    if let Ok(entries) = fs::read_dir(&source_dir) {
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| {
                panic!("failed to read TypeScript compiler asset entry: {error}")
            });
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if file_name != "typescript.js"
                && !(file_name.starts_with("lib.") && file_name.ends_with(".d.ts"))
            {
                continue;
            }
            let destination = staged_dir.join(file_name);
            fs::copy(&path, &destination).unwrap_or_else(|error| {
                panic!(
                    "failed to stage TypeScript compiler asset {}: {error}",
                    path.display()
                )
            });
            assets.push(file_name.to_owned());
        }
    }
    assets.sort();

    if !assets.iter().any(|asset| asset == "typescript.js") {
        println!("cargo:rustc-cfg=agentos_typescript_unavailable");
        println!(
            "cargo:warning=agentos-execution: building without the bundled TypeScript compiler; guest TypeScript checking will be unavailable in this build."
        );
    }

    let mut generated = String::from("&[\n");
    for asset in assets {
        writeln!(
            generated,
            "    ({asset:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/typescript/{asset}\")) as &'static [u8]),"
        )
        .expect("writing generated TypeScript asset table cannot fail");
    }
    generated.push_str("]\n");
    fs::write(&generated_path, generated).unwrap_or_else(|error| {
        panic!(
            "failed to write TypeScript asset table {}: {error}",
            generated_path.display()
        )
    });
}

fn stage_pyodide_assets(manifest_dir: &Path, out_dir: &Path) {
    let pyodide_out = out_dir.join("pyodide");
    fs::create_dir_all(&pyodide_out).unwrap_or_else(|error| {
        panic!(
            "failed to create pyodide staging dir {}: {}",
            pyodide_out.display(),
            error
        )
    });

    let mut pyodide_unavailable = false;

    for asset in EXTERNALIZED_PYODIDE_ASSETS {
        let in_tree = manifest_dir.join("assets/pyodide").join(asset);
        let dest = pyodide_out.join(asset);
        println!("cargo:rerun-if-changed={}", in_tree.display());

        if dest.exists() && !is_placeholder(&dest) {
            continue;
        }

        if in_tree.exists() {
            fs::copy(&in_tree, &dest).unwrap_or_else(|error| {
                panic!(
                    "failed to copy pyodide asset {} to {}: {}",
                    in_tree.display(),
                    dest.display(),
                    error
                )
            });
        } else {
            // Published-crate build: the externalized asset is absent and there
            // is no CDN dependency. Stage an empty placeholder so `include_bytes!`
            // compiles, and mark Python as unavailable for this build.
            pyodide_unavailable = true;
            fs::write(&dest, b"").unwrap_or_else(|error| {
                panic!(
                    "failed to write pyodide placeholder {}: {}",
                    dest.display(),
                    error
                )
            });
        }
    }

    if pyodide_unavailable {
        println!("cargo:rustc-cfg=agentos_pyodide_unavailable");
        println!(
            "cargo:warning=agentos-execution: building without bundled Pyodide assets; \
             guest Python execution will be unavailable in this build."
        );
    }
}

/// A zero-byte staged asset is a placeholder written by a prior published-crate
/// build; treat it as missing so a later workspace build can replace it with the
/// real in-tree asset.
fn is_placeholder(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.len() == 0)
        .unwrap_or(false)
}
