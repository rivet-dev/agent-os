use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn cargo_home() -> PathBuf {
    if let Some(home) = env::var_os("CARGO_HOME") {
        return PathBuf::from(home);
    }

    let home = env::var_os("HOME").expect("HOME must be set when CARGO_HOME is unset");
    PathBuf::from(home).join(".cargo")
}

fn read_v8_version(lock_path: &Path) -> String {
    let lock = fs::read_to_string(lock_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {}", lock_path.display(), error));

    let mut in_v8_package = false;
    for line in lock.lines() {
        match line.trim() {
            "[[package]]" => in_v8_package = false,
            "name = \"v8\"" => in_v8_package = true,
            _ if in_v8_package && line.trim_start().starts_with("version = \"") => {
                let version = line
                    .trim()
                    .trim_start_matches("version = \"")
                    .trim_end_matches('"');
                return version.to_owned();
            }
            _ => {}
        }
    }

    panic!("failed to locate v8 version in {}", lock_path.display());
}

fn find_v8_crate_root(v8_version: &str) -> PathBuf {
    let registry_src = cargo_home().join("registry").join("src");
    let entries = fs::read_dir(&registry_src).unwrap_or_else(|error| {
        panic!(
            "failed to read cargo registry src {}: {}",
            registry_src.display(),
            error
        )
    });

    for entry in entries {
        let entry = entry
            .unwrap_or_else(|error| panic!("failed to inspect cargo registry entry: {}", error));
        let crate_root = entry.path().join(format!("v8-{}", v8_version));
        if crate_root.is_dir() {
            return crate_root;
        }
    }

    panic!(
        "failed to locate v8-{} under {}",
        v8_version,
        registry_src.display(),
    );
}

fn find_v8_icu_data(crate_root: &Path) -> PathBuf {
    for relative in [
        Path::new("third_party/icu/common/icudtl.dat"),
        Path::new("third_party/icu/flutter_desktop/icudtl.dat"),
        Path::new("third_party/icu/chromecast_video/icudtl.dat"),
    ] {
        let candidate = crate_root.join(relative);
        if candidate.exists() {
            return candidate;
        }
    }

    panic!("failed to locate ICU data under {}", crate_root.display(),);
}

fn prepare_v8_include_overlay(crate_root: &Path, out_dir: &Path) -> PathBuf {
    const OLD_ITERATOR_ALIAS: &str = "using iterator_concept = Iterator::iterator_concept;";
    const FIXED_ITERATOR_ALIAS: &str =
        "using iterator_concept = typename Iterator::iterator_concept;";

    let source_include = crate_root.join("v8/include");
    let overlay_include = out_dir.join("v8-include");
    fs::create_dir_all(&overlay_include).unwrap_or_else(|error| {
        panic!(
            "failed to create V8 include overlay {}: {}",
            overlay_include.display(),
            error,
        )
    });

    let sandbox_header = source_include.join("v8-sandbox.h");
    fs::copy(&sandbox_header, overlay_include.join("v8-sandbox.h")).unwrap_or_else(|error| {
        panic!(
            "failed to copy V8 sandbox header {}: {}",
            sandbox_header.display(),
            error,
        )
    });

    let internal_header = source_include.join("v8-internal.h");
    let internal = fs::read_to_string(&internal_header).unwrap_or_else(|error| {
        panic!(
            "failed to read V8 internal header {}: {}",
            internal_header.display(),
            error,
        )
    });
    let patched = if internal.contains(FIXED_ITERATOR_ALIAS) {
        internal
    } else if internal.matches(OLD_ITERATOR_ALIAS).count() == 1 {
        internal.replacen(OLD_ITERATOR_ALIAS, FIXED_ITERATOR_ALIAS, 1)
    } else {
        panic!(
            "V8 iterator alias in {} did not match the expected declaration",
            internal_header.display(),
        );
    };
    fs::write(overlay_include.join("v8-internal.h"), patched).unwrap_or_else(|error| {
        panic!(
            "failed to write patched V8 internal header overlay: {}",
            error,
        )
    });

    println!("cargo:rerun-if-changed={}", sandbox_header.display());
    println!("cargo:rerun-if-changed={}", internal_header.display());
    overlay_include
}

fn build_v8_thread_support(manifest_dir: &Path, crate_root: &Path, out_dir: &Path) {
    let source = manifest_dir.join("src/v8_thread_support.cc");
    let include = crate_root.join("v8/include");
    let overlay_include = prepare_v8_include_overlay(crate_root, out_dir);
    println!("cargo:rerun-if-changed={}", source.display());
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .warnings(false)
        .include(overlay_include)
        .include(include)
        .file(source)
        .compile("agentos_v8_thread_support");
}

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"));
    let lock_path = manifest_dir.join("Cargo.lock");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));

    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    agentos_build_support::build_v8_bridge(&manifest_dir, &out_dir);

    let v8_version = read_v8_version(&lock_path);
    let v8_crate_root = find_v8_crate_root(&v8_version);
    build_v8_thread_support(&manifest_dir, &v8_crate_root, &out_dir);
    let icu_data = find_v8_icu_data(&v8_crate_root);
    let dest_path = out_dir.join("icudtl.dat");

    fs::copy(&icu_data, &dest_path).unwrap_or_else(|error| {
        panic!(
            "failed to copy ICU data from {} to {}: {}",
            icu_data.display(),
            dest_path.display(),
            error,
        )
    });
}
