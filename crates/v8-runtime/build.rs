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

fn find_v8_icu_data(v8_version: &str) -> PathBuf {
    let registry_src = cargo_home().join("registry").join("src");
    let candidates = [
        Path::new("third_party/icu/common/icudtl.dat"),
        Path::new("third_party/icu/flutter_desktop/icudtl.dat"),
        Path::new("third_party/icu/chromecast_video/icudtl.dat"),
    ];

    let entries = fs::read_dir(&registry_src).unwrap_or_else(|error| {
        panic!("failed to read cargo registry src {}: {}", registry_src.display(), error)
    });

    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("failed to inspect cargo registry entry: {}", error));
        let crate_root = entry.path().join(format!("v8-{}", v8_version));
        for relative in candidates {
            let candidate = crate_root.join(relative);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    panic!(
        "failed to locate ICU data for v8-{} under {}",
        v8_version,
        registry_src.display(),
    );
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"));
    let lock_path = manifest_dir.join("Cargo.lock");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));

    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let v8_version = read_v8_version(&lock_path);
    let icu_data = find_v8_icu_data(&v8_version);
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
