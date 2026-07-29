//! Reproducible micro-benchmark for the agentOS package **load** step: the
//! work the sidecar does per packed `.aospkg` at VM configure time — decode
//! the chunk1 vbare manifest, build the granular leaf mounts (tar-backed
//! version mount + `current`/`bin/*`/man-page symlinks), and open the tar
//! mount from its precomputed index. The tar is never extracted.
//!
//! ## What is timed
//! Per sample, four spans are timed:
//!   1. `read_package_manifest_from_path(<pkg>.aospkg)`
//!      → read the 16-byte header, decode the chunk1 `PackageManifest`
//!      (commands included)
//!   2. `build_package_leaf_mounts(&[descriptor], "/opt/agentos")`
//!      → cross-package command-collision check + leaf-mount construction
//!   3. `TarFileSystem::open` on the `.aospkg`
//!      → decode the precomputed chunk2 mount index + mmap the mount tar
//!      (cold each sample: the identity-keyed archive cache holds only
//!      weak refs, so dropping the fs between samples re-loads the index)
//!   4. read every regular file in the mounted tar back out through the VFS
//!      → recursive `read_dir_with_types` walk + `read_file` on each file,
//!      summing bytes. This proves the mount serves real content and
//!      bounds the total cost of "install + read everything".
//!
//! `FULL load` is steps 1–3 (the honest "install" span — content reads happen
//! later, on demand). `load + read ALL bytes` is steps 1–4 and is the
//! conservative upper bound to quote against extraction-based installs.
//!
//! ## Extraction baseline
//! Each target also runs an in-process `tar::Archive::unpack` of the same
//! source `package.tar` into a fresh directory per sample (removed outside
//! the timed span). This is what an extract-style installer (apt/dpkg, plain
//! `tar -x`) must do at minimum, steel-manned in agentOS's *disfavor*: no
//! fork/exec, no decompression, no dpkg bookkeeping, no fsync.
//!
//! Pack time (scanning a source `package.tar` and encoding the `.aospkg`
//! header/manifest/index — the "compile" step) is explicitly NOT counted: it
//! happens once at package build time, not at VM load. It is printed once per
//! target as an informational `pack (excluded)` line.
//! Nothing else (no ConfigureVm / no VM boot) is included.
//!
//! ## Page cache
//! By default samples run warm (a warmup loop touches everything first).
//! Set `PROJ_BENCH_COLD=1` to `posix_fadvise(DONTNEED)` the `.aospkg` (and
//! the baseline's source tar) before every sample, evicting its page cache
//! so each sample pays real disk reads. Warm-vs-cold is the difference
//! between "VM N+1 loads a package another VM already touched" and "first
//! load after boot".
//!
//! ## Reproducibility
//! Fixed warmup + sample count, deterministic on-disk inputs. Each sample
//! re-runs the *full* projection from scratch. Re-running the same command on
//! the same inputs yields comparable numbers.
//!
//! ## Inputs
//! Always synthesizes deterministic package tars in a tempdir first, so the
//! benchmark prints useful rows in a fresh checkout with no built registry:
//!   - synthetic-tiny:   a few `bin/` commands and a small manifest
//!   - synthetic-medium: the same shape plus a deterministic ~5 MiB payload
//!
//! Then runs the repo's built registry tars (skipped with a note if a tar is
//! absent, e.g. in a clean checkout that has not built `dist/`):
//!   - coreutils: `software/coreutils/dist/package.tar` (large, many commands)
//!   - tar:       `software/tar/dist/package.tar`       (single wasm binary)
//!   - git:       `software/git/dist/package.tar`       (the package
//!     the marketing install comparison talks about)
//!
//! Override the source tars via env:
//!   PROJ_BENCH_COREUTILS_TAR=/abs/package.tar  PROJ_BENCH_TAR_TAR=/abs/package.tar
//!   PROJ_BENCH_GIT_TAR=/abs/package.tar
//! Tune the run via env: PROJ_BENCH_SAMPLES (default 30), PROJ_BENCH_WARMUP
//! (default 2), PROJ_BENCH_COLD (default 0).
//!
//! ## Run
//! ```text
//! cargo test -p agentos-native-sidecar --release --test projection_bench -- --ignored --nocapture
//! # or, pointing at built tars elsewhere:
//! PROJ_BENCH_COREUTILS_TAR=/abs/software/coreutils/dist/package.tar \
//! PROJ_BENCH_TAR_TAR=/abs/software/tar/dist/package.tar \
//! PROJ_BENCH_GIT_TAR=/abs/software/git/dist/package.tar \
//!   cargo test -p agentos-native-sidecar --release --test projection_bench -- --ignored --nocapture
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use agentos_native_sidecar::package_projection::{
    build_package_leaf_mounts, read_package_manifest_from_path, DEFAULT_PACKAGE_TAR_NAME,
};
use vfs::package_format::pack::pack_aospkg_from_tar;
use vfs::posix::{TarFileSystem, VirtualFileSystem};

const SOURCE_PACKAGE_TAR_NAME: &str = "package.tar";

fn repo_root() -> PathBuf {
    // crates/sidecar -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn source_tar_path(env_key: &str, default_rel: &str) -> PathBuf {
    match std::env::var(env_key) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => repo_root().join(default_rel),
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Nearest-rank percentile (p in [0,100]) over an already-sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.iter().sum::<f64>() / v.len() as f64
}

struct Sample {
    total: f64,
    manifest: f64,
    mounts: f64,
    tar_open: f64,
    read_all: f64,
}

struct SyntheticTargets {
    root: PathBuf,
    tiny: PathBuf,
    medium: PathBuf,
    tiny_pack: Duration,
    medium_pack: Duration,
}

struct RealTarget {
    label: &'static str,
    dir: PathBuf,
    source_tar: PathBuf,
    pack: Option<Duration>,
}

struct RealTargets {
    root: PathBuf,
    targets: Vec<RealTarget>,
}

impl Drop for RealTargets {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Drop for SyntheticTargets {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_repeated_file(path: &Path, len: usize) {
    let mut file = fs::File::create(path)
        .unwrap_or_else(|e| panic!("create synthetic payload {} failed: {e}", path.display()));
    let chunk = b"agentos projection bench payload\n";
    let mut remaining = len;
    while remaining > 0 {
        let n = remaining.min(chunk.len());
        std::io::Write::write_all(&mut file, &chunk[..n])
            .unwrap_or_else(|e| panic!("write synthetic payload {} failed: {e}", path.display()));
        remaining -= n;
    }
}

fn create_package_tar(label: &str, dest: &Path, commands: &[&str], payload_bytes: usize) {
    let source = dest.join("package");
    fs::create_dir_all(source.join("bin"))
        .unwrap_or_else(|e| panic!("create synthetic package {} failed: {e}", source.display()));
    fs::write(
        source.join("agentos-package.json"),
        format!("{{\"name\":\"synthetic-{label}\",\"version\":\"1.0.0\"}}\n"),
    )
    .unwrap_or_else(|e| panic!("write synthetic manifest for {label} failed: {e}"));

    for command in commands {
        fs::write(
            source.join("bin").join(command),
            format!("#!/bin/sh\nprintf '{command}\\n'\n"),
        )
        .unwrap_or_else(|e| panic!("write synthetic bin/{command} failed: {e}"));
    }

    let mut members = vec!["agentos-package.json", "bin"];
    if payload_bytes > 0 {
        write_repeated_file(&source.join("payload.dat"), payload_bytes);
        members.push("payload.dat");
    }

    let tar_path = dest.join(SOURCE_PACKAGE_TAR_NAME);
    let status = Command::new("tar")
        .args([
            "--sort=name",
            "--mtime=@0",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "-cf",
        ])
        .arg(&tar_path)
        .arg("-C")
        .arg(&source)
        .args(members)
        .status()
        .unwrap_or_else(|e| panic!("run tar for synthetic {label} failed: {e}"));
    assert!(
        status.success(),
        "tar failed for synthetic {label} with status {status}"
    );
}

fn create_synthetic_targets() -> SyntheticTargets {
    let unique = format!(
        "agentos-projection-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(unique);
    let tiny = root.join("tiny");
    let medium = root.join("medium");
    fs::create_dir_all(&tiny)
        .unwrap_or_else(|e| panic!("create synthetic tiny dir {} failed: {e}", tiny.display()));
    fs::create_dir_all(&medium).unwrap_or_else(|e| {
        panic!(
            "create synthetic medium dir {} failed: {e}",
            medium.display()
        )
    });

    create_package_tar("tiny", &tiny, &["alpha", "beta", "gamma"], 0);
    create_package_tar(
        "medium",
        &medium,
        &["alpha", "beta", "gamma"],
        5 * 1024 * 1024,
    );

    let tiny_pack = repack_package_tar_to_aospkg(
        &tiny.join(SOURCE_PACKAGE_TAR_NAME),
        &tiny.join(DEFAULT_PACKAGE_TAR_NAME),
    );
    let medium_pack = repack_package_tar_to_aospkg(
        &medium.join(SOURCE_PACKAGE_TAR_NAME),
        &medium.join(DEFAULT_PACKAGE_TAR_NAME),
    );

    SyntheticTargets {
        root,
        tiny,
        medium,
        tiny_pack,
        medium_pack,
    }
}

fn create_repacked_real_targets(sources: &[(&'static str, &Path)]) -> RealTargets {
    let unique = format!(
        "agentos-projection-real-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(unique);
    let targets = sources
        .iter()
        .map(|(label, source_tar)| {
            let dir = root.join(label);
            fs::create_dir_all(&dir)
                .unwrap_or_else(|e| panic!("create repacked {label} dir failed: {e}"));
            let pack = source_tar.is_file().then(|| {
                repack_package_tar_to_aospkg(source_tar, &dir.join(DEFAULT_PACKAGE_TAR_NAME))
            });
            RealTarget {
                label,
                dir,
                source_tar: source_tar.to_path_buf(),
                pack,
            }
        })
        .collect();
    RealTargets { root, targets }
}

/// Pack a source `package.tar` into a `.aospkg` via the canonical packer in
/// `vfs::package_format::pack` (agentos-package.json is consumed at pack time
/// and stripped from the mount tar). This is the "compile" step; it runs at
/// package build time and is never part of the timed load span. Returns the
/// pack duration so callers can print it as an excluded stat.
fn repack_package_tar_to_aospkg(source_tar: &Path, dest_aospkg: &Path) -> Duration {
    let started = Instant::now();
    pack_aospkg_from_tar(source_tar, dest_aospkg)
        .unwrap_or_else(|e| panic!("pack {} failed: {e}", source_tar.display()));
    started.elapsed()
}

/// Evict a file's pages from the OS page cache so the next access pays real
/// disk I/O. Flushes first (a dirty page cannot be dropped), then advises
/// DONTNEED over the whole file.
fn evict_page_cache(path: &Path) {
    use std::os::fd::AsRawFd;
    let file = fs::File::open(path)
        .unwrap_or_else(|e| panic!("open {} for cache eviction failed: {e}", path.display()));
    file.sync_all()
        .unwrap_or_else(|e| panic!("fsync {} before eviction failed: {e}", path.display()));
    nix::fcntl::posix_fadvise(
        file.as_raw_fd(),
        0,
        0,
        nix::fcntl::PosixFadviseAdvice::POSIX_FADV_DONTNEED,
    )
    .unwrap_or_else(|e| panic!("posix_fadvise(DONTNEED) on {} failed: {e}", path.display()));
}

/// Walk the mounted tar filesystem and read every regular file's full content
/// through the VFS read path. Returns (bytes read, files read).
fn read_all_files(fs: &mut TarFileSystem) -> (u64, usize) {
    let mut stack = vec![String::from("/")];
    let mut bytes = 0u64;
    let mut files = 0usize;
    while let Some(dir) = stack.pop() {
        let entries = fs
            .read_dir_with_types(&dir)
            .unwrap_or_else(|e| panic!("readdir {dir} failed: {e:?}"));
        for entry in entries {
            let path = if dir == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", dir, entry.name)
            };
            if entry.is_directory {
                stack.push(path);
            } else if !entry.is_symbolic_link {
                let content = fs
                    .read_file(&path)
                    .unwrap_or_else(|e| panic!("read {path} failed: {e:?}"));
                bytes += content.len() as u64;
                files += 1;
            }
        }
    }
    (bytes, files)
}

struct ProjectOutcome {
    sample: Sample,
    command_count: usize,
    mount_count: usize,
    bytes_read: u64,
    files_read: usize,
}

fn project_once(dir: &str) -> ProjectOutcome {
    // Step 1: read the .aospkg header + chunk1 manifest (commands included).
    // The wire carries the packed file path, so the bench does too.
    let aospkg = Path::new(dir).join(DEFAULT_PACKAGE_TAR_NAME);
    let aospkg = aospkg.to_str().expect("utf8 aospkg path");
    let t0 = Instant::now();
    let descriptor = read_package_manifest_from_path(aospkg)
        .unwrap_or_else(|e| panic!("read_package_manifest_from_path({aospkg}) failed: {e:?}"));
    let manifest_ms = ms(t0.elapsed());
    let command_count = descriptor.commands.len();
    let tar_path = descriptor
        .tar_path
        .clone()
        .unwrap_or_else(|| panic!("bench package in {dir} must have a .aospkg tar"));

    // Step 2: build the granular leaf mounts (+ collision check).
    let t1 = Instant::now();
    let mounts = build_package_leaf_mounts(&[descriptor], "/opt/agentos")
        .unwrap_or_else(|e| panic!("build_package_leaf_mounts({dir}) failed: {e:?}"));
    let mounts_ms = ms(t1.elapsed());

    // Step 3: open the tar mount — decode the precomputed chunk2 index and
    // mmap the mount tar. The archive cache only holds weak refs, so dropping
    // the fs at the end of this sample makes the next sample a cold load.
    let t2 = Instant::now();
    let mut fs = TarFileSystem::open(&tar_path)
        .unwrap_or_else(|e| panic!("TarFileSystem::open({tar_path}) failed: {e:?}"));
    let tar_open_ms = ms(t2.elapsed());
    let total_ms = ms(t0.elapsed());

    // Step 4 (reported separately from FULL load): prove the mount serves
    // real content by reading every regular file back out through the VFS.
    let t3 = Instant::now();
    let (bytes_read, files_read) = read_all_files(&mut fs);
    let read_all_ms = ms(t3.elapsed());
    drop(fs);

    ProjectOutcome {
        sample: Sample {
            total: total_ms,
            manifest: manifest_ms,
            mounts: mounts_ms,
            tar_open: tar_open_ms,
            read_all: read_all_ms,
        },
        command_count,
        mount_count: mounts.len(),
        bytes_read,
        files_read,
    }
}

/// In-process extraction of the source `package.tar` into a fresh directory:
/// the minimum work an extract-style installer must do, with every advantage
/// granted (no fork/exec, no decompression, no scripts/db, no fsync).
/// Directory creation and removal happen outside the timed span.
fn extract_once(source_tar: &Path, dest: &Path) -> f64 {
    fs::create_dir_all(dest)
        .unwrap_or_else(|e| panic!("create extraction dir {} failed: {e}", dest.display()));
    let t0 = Instant::now();
    let file = fs::File::open(source_tar)
        .unwrap_or_else(|e| panic!("open {} failed: {e}", source_tar.display()));
    let mut archive = tar::Archive::new(std::io::BufReader::new(file));
    archive
        .unpack(dest)
        .unwrap_or_else(|e| panic!("unpack {} failed: {e}", source_tar.display()));
    let elapsed = ms(t0.elapsed());
    fs::remove_dir_all(dest)
        .unwrap_or_else(|e| panic!("remove extraction dir {} failed: {e}", dest.display()));
    elapsed
}

fn run_target(
    label: &str,
    dir: &Path,
    source_tar: Option<&Path>,
    warmup: usize,
    samples: usize,
    pack: Option<Duration>,
    cold: bool,
) {
    let tar = dir.join(DEFAULT_PACKAGE_TAR_NAME);
    if !tar.is_file() {
        println!(
            "[skip] {label}: no {DEFAULT_PACKAGE_TAR_NAME} at {} (build its dist/ first)",
            dir.display()
        );
        return;
    }
    let tar_bytes = std::fs::metadata(&tar).map(|m| m.len()).unwrap_or(0);
    let dir_str = dir.to_str().expect("utf8 dir");

    // Warmup (warms page cache in warm mode; results discarded).
    let mut cmd_count = 0usize;
    let mut mount_count = 0usize;
    let mut bytes_read = 0u64;
    let mut files_read = 0usize;
    for _ in 0..warmup {
        let outcome = project_once(dir_str);
        cmd_count = outcome.command_count;
        mount_count = outcome.mount_count;
        bytes_read = outcome.bytes_read;
        files_read = outcome.files_read;
    }

    let mut rows: Vec<Sample> = Vec::with_capacity(samples);
    for _ in 0..samples {
        if cold {
            evict_page_cache(&tar);
        }
        let outcome = project_once(dir_str);
        cmd_count = outcome.command_count;
        mount_count = outcome.mount_count;
        bytes_read = outcome.bytes_read;
        files_read = outcome.files_read;
        rows.push(outcome.sample);
    }

    // Extraction baseline over the same source tar, same warmup/sample/cold
    // treatment as the load samples.
    let baseline = source_tar.filter(|p| p.is_file()).map(|source| {
        let dest = dir.join("extract-baseline");
        for _ in 0..warmup {
            extract_once(source, &dest);
        }
        let mut totals: Vec<f64> = (0..samples)
            .map(|_| {
                if cold {
                    evict_page_cache(source);
                }
                extract_once(source, &dest)
            })
            .collect();
        totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        totals
    });

    let mut totals: Vec<f64> = rows.iter().map(|r| r.total).collect();
    let mut manifests: Vec<f64> = rows.iter().map(|r| r.manifest).collect();
    let mut mounts_v: Vec<f64> = rows.iter().map(|r| r.mounts).collect();
    let mut tar_opens: Vec<f64> = rows.iter().map(|r| r.tar_open).collect();
    let mut read_alls: Vec<f64> = rows.iter().map(|r| r.read_all).collect();
    let mut full_with_read: Vec<f64> = rows.iter().map(|r| r.total + r.read_all).collect();
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    manifests.sort_by(|a, b| a.partial_cmp(b).unwrap());
    mounts_v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    tar_opens.sort_by(|a, b| a.partial_cmp(b).unwrap());
    read_alls.sort_by(|a, b| a.partial_cmp(b).unwrap());
    full_with_read.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!(
        "\n=== {label}  ({:.1} MiB tar, {cmd_count} commands, {mount_count} leaf mounts, N={samples}, warmup={warmup}, {} cache) ===",
        tar_bytes as f64 / (1024.0 * 1024.0),
        if cold { "COLD page" } else { "warm page" }
    );
    if let Some(pack) = pack {
        println!(
            "  pack .tar -> .aospkg (excluded from load): {:.1} ms, once at package build time",
            ms(pack)
        );
    }
    println!(
        "  step4 reads back {files_read} files, {:.1} MiB, through the mounted VFS each sample",
        bytes_read as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  {:<28} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "span (ms)", "min", "median", "mean", "p95", "max"
    );
    // mean() is order-invariant, so the sorted slices serve every stat.
    let print_stat = |name: &str, sorted: &[f64]| {
        println!(
            "  {:<28} {:>9.3} {:>9.3} {:>9.3} {:>9.3} {:>9.3}",
            name,
            sorted.first().copied().unwrap_or(f64::NAN),
            median(sorted),
            mean(sorted),
            percentile(sorted, 95.0),
            sorted.last().copied().unwrap_or(f64::NAN),
        );
    };
    print_stat("FULL load (1+2+3)", &totals);
    print_stat("  manifest+cmds (step1)", &manifests);
    print_stat("  leaf mounts   (step2)", &mounts_v);
    print_stat("  tar mount open (step3)", &tar_opens);
    print_stat("read ALL bytes (step4)", &read_alls);
    print_stat("load + read ALL (1..4)", &full_with_read);
    match &baseline {
        Some(extract) => {
            print_stat("tar extract baseline", extract);
            let load_median = median(&totals);
            let full_median = median(&full_with_read);
            let extract_median = median(extract);
            println!(
                "  extract/load: {:.0}x   extract/(load+read ALL): {:.1}x",
                extract_median / load_median,
                extract_median / full_median
            );
        }
        None => println!("  tar extract baseline: [skip] no source package.tar"),
    }
}

#[test]
#[ignore = "bench: prints a projection-timing table; run with --ignored --nocapture"]
fn projection_bench() {
    let warmup = env_usize("PROJ_BENCH_WARMUP", 2);
    let samples = env_usize("PROJ_BENCH_SAMPLES", 30);
    let cold = env_flag("PROJ_BENCH_COLD");

    let coreutils_tar = source_tar_path(
        "PROJ_BENCH_COREUTILS_TAR",
        "software/coreutils/dist/package.tar",
    );
    let tar_tar = source_tar_path("PROJ_BENCH_TAR_TAR", "software/tar/dist/package.tar");
    let git_tar = source_tar_path("PROJ_BENCH_GIT_TAR", "software/git/dist/package.tar");

    println!("\n# agentOS package load benchmark (.aospkg)");
    println!(
        "# FULL load = manifest chunk read + leaf mounts + tar mount open (index decode + mmap)"
    );
    println!("# step4 additionally reads every regular file back through the mounted VFS");
    println!("# baseline = in-process tar extraction of the same source package.tar");
    println!("# pack (.tar -> .aospkg) runs once in setup and is excluded from all load stats");
    println!(
        "# page cache: {} (set PROJ_BENCH_COLD=1 for per-sample eviction)",
        if cold {
            "COLD (evicted per sample)"
        } else {
            "warm"
        }
    );
    println!("# repo root = {}", repo_root().display());

    let synthetic = create_synthetic_targets();
    run_target(
        "synthetic-tiny",
        &synthetic.tiny,
        Some(&synthetic.tiny.join(SOURCE_PACKAGE_TAR_NAME)),
        warmup,
        samples,
        Some(synthetic.tiny_pack),
        cold,
    );
    run_target(
        "synthetic-medium",
        &synthetic.medium,
        Some(&synthetic.medium.join(SOURCE_PACKAGE_TAR_NAME)),
        warmup,
        samples,
        Some(synthetic.medium_pack),
        cold,
    );
    let real = create_repacked_real_targets(&[
        ("coreutils", &coreutils_tar),
        ("tar (wasm binary)", &tar_tar),
        ("git", &git_tar),
    ]);
    for target in &real.targets {
        run_target(
            target.label,
            &target.dir,
            Some(&target.source_tar),
            warmup,
            samples,
            target.pack,
            cold,
        );
    }
    println!();
}

/// Default-suite load budget: the coreutils FULL load (manifest, leaf mounts,
/// tar index open) must stay under 20 ms median even in a debug build — it
/// measures ~0.15 ms in release and ~1 ms in debug, so 20 ms means something
/// structurally wrong (e.g. a whole-archive read snuck back in). Not hidden
/// behind `#[ignore]` so the budget cannot silently regress; skips cleanly
/// when the registry package is not built.
#[test]
fn coreutils_load_budget() {
    let coreutils_tar = source_tar_path(
        "PROJ_BENCH_COREUTILS_TAR",
        "software/coreutils/dist/package.tar",
    );
    if !coreutils_tar.is_file() {
        eprintln!(
            "skipping coreutils_load_budget: {} not built",
            coreutils_tar.display()
        );
        return;
    }
    let real = create_repacked_real_targets(&[("coreutils", &coreutils_tar)]);
    let dir = real.targets[0].dir.to_str().expect("utf8 dir");
    for _ in 0..2 {
        let _ = project_once(dir);
    }
    let mut totals: Vec<f64> = (0..10).map(|_| project_once(dir).sample.total).collect();
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(
        median(&totals) < 20.0,
        "coreutils FULL-load median (incl. tar index decode + mmap) must be < 20 ms, got {:.3} ms",
        median(&totals)
    );
}
