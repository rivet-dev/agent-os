//! Built-in implementations of commands that don't have
//! practical uutils replacements for wasm32-wasip1.
//!
//! - sleep: uses host_process.sleep_ms (Atomics.wait on host side)
//! - test/[: conditional expressions (uu_test has 17 unix errors)
//! - whoami: reads USER/LOGNAME env vars (uu_whoami needs unix)
//! - spawn-test: internal subprocess lifecycle test
#![cfg_attr(target_os = "wasi", feature(wasi_ext))]

use std::ffi::OsString;
use std::fs::Metadata;
use std::io::{self, Write};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(target_os = "wasi")]
mod host_fs {
    #[link(wasm_import_module = "host_fs")]
    unsafe extern "C" {
        pub fn path_mode(path_ptr: *const u8, path_len: u32, follow_symlinks: u32) -> u32;
    }
}

/// Sleep: pause for N seconds via host_process.sleep_ms callback.
/// Uses Atomics.wait on the host side — no busy-waiting.
pub fn sleep(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    if str_args.is_empty() {
        eprintln!("sleep: missing operand");
        return 1;
    }

    let secs: f64 = match str_args[0].parse() {
        Ok(s) if s >= 0.0 => s,
        _ => {
            eprintln!("sleep: invalid time interval '{}'", str_args[0]);
            return 1;
        }
    };

    let millis = (secs * 1000.0) as u32;
    if let Err(_) = wasi_ext::host_sleep_ms(millis) {
        // Fallback to busy-wait if host doesn't support sleep_ms
        let start = std::time::Instant::now();
        let duration = std::time::Duration::from_secs_f64(secs);
        while start.elapsed() < duration {
            std::thread::yield_now();
        }
    }

    0
}

/// spawn-test: spawns a child process via std::process::Command and
/// prints its stdout. Used to verify subprocess lifecycle integration.
///
/// Usage: spawn-test <command> [args...]
/// Default: spawn-test echo hello
pub fn spawn_test(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let program = str_args.first().map(|s| s.as_str()).unwrap_or("echo");
    let child_args: Vec<&str> = str_args.iter().skip(1).map(|s| s.as_str()).collect();

    let output = match std::process::Command::new(program)
        .args(&child_args)
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("spawn-test: failed to spawn '{}': {}", program, e);
            return 1;
        }
    };

    // Print child's stdout/stderr to our stdout/stderr
    let _ = io::stdout().write_all(&output.stdout);
    let _ = io::stderr().write_all(&output.stderr);

    output.status.code().unwrap_or(1)
}

/// Minimal test / [ command: evaluate conditional expressions.
/// Dispatches on argv[0] basename for standalone binary usage.
pub fn test_cmd(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    // Remove trailing ']' if invoked as '['
    let args: Vec<&str> =
        if !str_args.is_empty() && str_args.last().map(|s| s.as_str()) == Some("]") {
            str_args[..str_args.len() - 1]
                .iter()
                .map(|s| s.as_str())
                .collect()
        } else {
            str_args.iter().map(|s| s.as_str()).collect()
        };

    if args.is_empty() {
        return 1; // empty test is false
    }

    let result = eval_test(&args);
    if result {
        0
    } else {
        1
    }
}

fn eval_test(args: &[&str]) -> bool {
    let mut parser = TestParser::new(args);
    let result = parser.parse_or_expression();
    result && parser.is_complete()
}

struct TestParser<'a> {
    args: &'a [&'a str],
    position: usize,
    invalid: bool,
}

impl<'a> TestParser<'a> {
    fn new(args: &'a [&'a str]) -> Self {
        Self {
            args,
            position: 0,
            invalid: false,
        }
    }

    fn parse_or_expression(&mut self) -> bool {
        let mut result = self.parse_and_expression();
        while self.consume("-o") {
            let rhs = self.parse_and_expression();
            result = result || rhs;
        }
        result
    }

    fn parse_and_expression(&mut self) -> bool {
        let mut result = self.parse_not_expression();
        while self.consume("-a") {
            let rhs = self.parse_not_expression();
            result = result && rhs;
        }
        result
    }

    fn parse_not_expression(&mut self) -> bool {
        if self.consume("!") {
            return !self.parse_not_expression();
        }
        self.parse_primary_expression()
    }

    fn parse_primary_expression(&mut self) -> bool {
        if self.consume("(") {
            let result = self.parse_or_expression();
            if !self.consume(")") {
                self.invalid = true;
                return false;
            }
            return result;
        }

        self.parse_simple_expression()
    }

    fn parse_simple_expression(&mut self) -> bool {
        let Some(first) = self.peek() else {
            self.invalid = true;
            return false;
        };

        if is_unary_operator(first) {
            let Some(second) = self.peek_nth(1) else {
                self.invalid = true;
                return false;
            };
            self.position += 2;
            return eval_simple(&[first, second]);
        }

        if let (Some(operator), Some(third)) = (self.peek_nth(1), self.peek_nth(2)) {
            if is_binary_operator(operator) {
                self.position += 3;
                return eval_simple(&[first, operator, third]);
            }
        }

        self.position += 1;
        eval_simple(&[first])
    }

    fn consume(&mut self, expected: &str) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<&'a str> {
        self.peek_nth(0)
    }

    fn peek_nth(&self, offset: usize) -> Option<&'a str> {
        self.args.get(self.position + offset).copied()
    }

    fn is_complete(&self) -> bool {
        !self.invalid && self.position == self.args.len()
    }
}

fn eval_simple(args: &[&str]) -> bool {
    match args.len() {
        0 => false,
        1 => !args[0].is_empty(),
        2 => match args[0] {
            "-n" => !args[1].is_empty(),
            "-z" => args[1].is_empty(),
            "-f" => std::fs::metadata(args[1])
                .map(|m| m.is_file())
                .unwrap_or(false),
            "-d" => std::fs::metadata(args[1])
                .map(|m| m.is_dir())
                .unwrap_or(false),
            "-e" => std::fs::metadata(args[1]).is_ok(),
            "-s" => std::fs::metadata(args[1])
                .map(|m| m.len() > 0)
                .unwrap_or(false),
            "-r" | "-w" | "-x" => has_access_mode(args[1], args[0]),
            _ => false,
        },
        3 => match args[1] {
            "=" | "==" => args[0] == args[2],
            "!=" => args[0] != args[2],
            "-eq" => args[0].parse::<i64>().ok() == args[2].parse::<i64>().ok(),
            "-ne" => args[0].parse::<i64>().ok() != args[2].parse::<i64>().ok(),
            "-lt" => args[0].parse::<i64>().unwrap_or(0) < args[2].parse::<i64>().unwrap_or(0),
            "-le" => args[0].parse::<i64>().unwrap_or(0) <= args[2].parse::<i64>().unwrap_or(0),
            "-gt" => args[0].parse::<i64>().unwrap_or(0) > args[2].parse::<i64>().unwrap_or(0),
            "-ge" => args[0].parse::<i64>().unwrap_or(0) >= args[2].parse::<i64>().unwrap_or(0),
            "-nt" => compare_mtime(args[0], args[2], |lhs, rhs| lhs > rhs),
            "-ot" => compare_mtime(args[0], args[2], |lhs, rhs| lhs < rhs),
            _ => false,
        },
        _ => false,
    }
}

fn compare_mtime(
    left: &str,
    right: &str,
    predicate: impl FnOnce(SystemTime, SystemTime) -> bool,
) -> bool {
    let Some(left_mtime) = file_mtime(left) else {
        return false;
    };
    let Some(right_mtime) = file_mtime(right) else {
        return false;
    };

    predicate(left_mtime, right_mtime)
}

fn file_mtime(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn is_unary_operator(token: &str) -> bool {
    matches!(token, "-n" | "-z" | "-f" | "-d" | "-e" | "-s" | "-r" | "-w" | "-x")
}

fn is_binary_operator(token: &str) -> bool {
    matches!(
        token,
        "=" | "==" | "!=" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" | "-nt" | "-ot"
    )
}

fn has_access_mode(path: &str, flag: &str) -> bool {
    match requested_mode_bit(flag) {
        Some(bit) => access_mode_allowed(path, bit),
        None => false,
    }
}

fn requested_mode_bit(flag: &str) -> Option<u32> {
    match flag {
        "-r" => Some(0o4),
        "-w" => Some(0o2),
        "-x" => Some(0o1),
        _ => None,
    }
}

#[cfg(any(unix, target_os = "wasi"))]
fn access_mode_allowed(path: &str, requested_bit: u32) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };

    permission_allows(path, &metadata, requested_bit)
}

#[cfg(not(any(unix, target_os = "wasi")))]
fn access_mode_allowed(path: &str, requested_bit: u32) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };

    permission_allows(path, &metadata, requested_bit)
}

#[cfg(unix)]
fn permission_allows(_path: &str, metadata: &Metadata, requested_bit: u32) -> bool {
    let mode = metadata.mode();
    let identity = current_identity();

    if identity.uid == 0 {
        if requested_bit == 0o1 {
            return metadata.is_dir() || (mode & 0o111) != 0;
        }
        return true;
    }

    let permission_bits = if identity.uid == metadata.uid() {
        (mode >> 6) & 0o7
    } else if identity.gids.iter().any(|gid| *gid == metadata.gid()) {
        (mode >> 3) & 0o7
    } else {
        mode & 0o7
    };

    (permission_bits & requested_bit) != 0
}

#[cfg(target_os = "wasi")]
fn permission_allows(_path: &str, metadata: &Metadata, requested_bit: u32) -> bool {
    let mode = wasi_path_mode(_path).unwrap_or_default();
    let identity = current_identity();

    if identity.uid == 0 {
        if requested_bit == 0o1 {
            return metadata.is_dir() || (mode & 0o111) != 0;
        }
        return true;
    }

    let owner = (mode >> 6) & 0o7;
    let group = (mode >> 3) & 0o7;
    let other = mode & 0o7;

    (owner & requested_bit) != 0 || (group & requested_bit) != 0 || (other & requested_bit) != 0
}

#[cfg(not(any(unix, target_os = "wasi")))]
fn permission_allows(_path: &str, metadata: &Metadata, requested_bit: u32) -> bool {
    requested_bit != 0o2 || !metadata.permissions().readonly()
}

#[cfg(target_os = "wasi")]
fn wasi_path_mode(path: &str) -> Option<u32> {
    let bytes = path.as_bytes();
    let mode = unsafe { host_fs::path_mode(bytes.as_ptr(), bytes.len() as u32, 1) };
    if mode == 0 { None } else { Some(mode) }
}

struct ProcessIdentity {
    uid: u32,
    #[cfg(unix)]
    gids: Vec<u32>,
}

#[cfg(target_os = "wasi")]
fn current_identity() -> ProcessIdentity {
    let uid = wasi_ext::get_euid()
        .or_else(|_| wasi_ext::get_uid())
        .unwrap_or(0);

    ProcessIdentity { uid }
}

#[cfg(unix)]
fn current_identity() -> ProcessIdentity {
    let uid = unsafe { libc::geteuid() } as u32;
    let gid = unsafe { libc::getegid() } as u32;

    let supplementary_count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut gids = vec![gid];

    if supplementary_count > 0 {
        let mut extra_gids = vec![0; supplementary_count as usize];
        let written = unsafe { libc::getgroups(supplementary_count, extra_gids.as_mut_ptr()) };
        if written > 0 {
            gids.extend(
                extra_gids
                    .into_iter()
                    .take(written as usize)
                    .map(|group| group as u32),
            );
        }
    }

    ProcessIdentity { uid, gids }
}

#[cfg(not(any(unix, target_os = "wasi")))]
fn current_identity() -> ProcessIdentity {
    ProcessIdentity { uid: 0 }
}

/// Minimal whoami: print the current user name.
pub fn whoami(_args: Vec<OsString>) -> i32 {
    // Try USER env var first, fall back to LOGNAME, then "user"
    let name = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_string());
    println!("{}", name);
    0
}

#[cfg(test)]
mod tests {
    use super::test_cmd;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[cfg(any(unix, target_os = "wasi"))]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_access_checks_follow_mode_bits() {
        let fixture = TempFixture::new();

        fixture.write_file("read-write.txt", 0o644);
        fixture.write_file("read-only.txt", 0o444);
        fixture.write_file("executable.sh", 0o755);
        fixture.write_file("blocked.bin", 0o000);

        let read_write = fixture.path("read-write.txt");
        assert_eq!(run_test("-r", &read_write), 0);
        assert_eq!(run_test("-w", &read_write), 0);
        assert_eq!(run_test("-x", &read_write), 1);

        let read_only = fixture.path("read-only.txt");
        assert_eq!(run_test("-r", &read_only), 0);
        assert_eq!(run_test("-w", &read_only), 1);
        assert_eq!(run_test("-x", &read_only), 1);

        let executable = fixture.path("executable.sh");
        assert_eq!(run_test("-r", &executable), 0);
        assert_eq!(run_test("-w", &executable), 0);
        assert_eq!(run_test("-x", &executable), 0);

        let blocked = fixture.path("blocked.bin");
        assert_eq!(run_test("-r", &blocked), 1);
        assert_eq!(run_test("-w", &blocked), 1);
        assert_eq!(run_test("-x", &blocked), 1);
    }

    #[test]
    fn test_compound_expressions_follow_posix_precedence_and_grouping() {
        let cases = [
            (vec!["a"], true),
            (vec!["-n", "value"], true),
            (vec!["-z", ""], true),
            (vec!["1", "-eq", "1"], true),
            (vec!["1", "-eq", "2"], false),
            (vec!["1", "-eq", "1", "-a", "2", "-eq", "2"], true),
            (vec!["1", "-eq", "1", "-o", "2", "-eq", "3"], true),
            (vec!["1", "-eq", "1", "-o", "2", "-eq", "3", "-a", "4", "-eq", "5"], true),
            (vec!["1", "-eq", "2", "-o", "2", "-eq", "2", "-a", "3", "-eq", "3"], true),
            (
                vec![
                    "(",
                    "1",
                    "-eq",
                    "2",
                    "-o",
                    "2",
                    "-eq",
                    "2",
                    ")",
                    "-a",
                    "3",
                    "-eq",
                    "3",
                ],
                true,
            ),
            (
                vec![
                    "(",
                    "1",
                    "-eq",
                    "2",
                    "-o",
                    "2",
                    "-eq",
                    "3",
                    ")",
                    "-a",
                    "3",
                    "-eq",
                    "3",
                ],
                false,
            ),
            (
                vec!["!", "(", "1", "-eq", "2", "-o", "2", "-eq", "3", ")", "-a", "4", "-eq", "4"],
                true,
            ),
            (
                vec!["!", "(", "1", "-eq", "1", "-a", "2", "-eq", "2", ")"],
                false,
            ),
            (
                vec!["!", "1", "-eq", "2", "-o", "3", "-eq", "4"],
                true,
            ),
            (
                vec![
                    "(",
                    "1",
                    "-eq",
                    "1",
                    "-o",
                    "2",
                    "-eq",
                    "3",
                    ")",
                    "-a",
                    "!",
                    "(",
                    "4",
                    "-eq",
                    "5",
                    ")",
                ],
                true,
            ),
        ];

        for (expr, expected) in cases {
            assert_result("test", &expr, expected);
            assert_result("[", &expr, expected);
        }
    }

    #[test]
    fn test_compound_expressions_reject_unbalanced_grouping() {
        assert_result("[", &["(", "1", "-eq", "1"], false);
        assert_result("test", &["1", "-eq", "1", ")"], false);
    }

    #[test]
    fn test_file_time_comparisons_use_mtime_and_ignore_missing_paths() {
        let fixture = TempFixture::new();

        fixture.write_file("old.txt", 0o644);
        fixture.write_file("mid.txt", 0o644);
        fixture.write_file("new.txt", 0o644);

        let old = fixture.path("old.txt");
        let mid = fixture.path("mid.txt");
        let new = fixture.path("new.txt");
        let missing = fixture.path("missing.txt");

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_mtime(&old, base);
        set_mtime(&mid, base + Duration::from_secs(10));
        set_mtime(&new, base + Duration::from_secs(20));

        assert_result("test", &[new.as_str(), "-nt", mid.as_str()], true);
        assert_result("test", &[mid.as_str(), "-nt", new.as_str()], false);
        assert_result("test", &[old.as_str(), "-ot", mid.as_str()], true);
        assert_result("test", &[mid.as_str(), "-ot", old.as_str()], false);
        assert_result("test", &[mid.as_str(), "-nt", mid.as_str()], false);
        assert_result("test", &[mid.as_str(), "-ot", mid.as_str()], false);
        assert_result("test", &[mid.as_str(), "-nt", missing.as_str()], false);
        assert_result("test", &[missing.as_str(), "-ot", mid.as_str()], false);
        assert_result("[", &[new.as_str(), "-nt", old.as_str()], true);
        assert_result("[", &[old.as_str(), "-ot", new.as_str()], true);
    }

    fn run_test(flag: &str, path: &str) -> i32 {
        let mut argv = vec![OsString::from("test")];
        argv.push(OsString::from(flag));
        argv.push(OsString::from(path));
        test_cmd(argv)
    }

    fn assert_result(program: &str, expr: &[&str], expected: bool) {
        let status = run_expression(program, expr);
        assert_eq!(
            status == 0,
            expected,
            "program={program} expr={expr:?} expected={expected} got_status={status}"
        );
    }

    fn run_expression(program: &str, expr: &[&str]) -> i32 {
        let mut argv = vec![OsString::from(program)];
        argv.extend(expr.iter().map(OsString::from));
        if program == "[" {
            argv.push(OsString::from("]"));
        }
        test_cmd(argv)
    }

    struct TempFixture {
        root: PathBuf,
    }

    impl TempFixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "secureexec-builtins-test-{}-{}",
                std::process::id(),
                unique
            ));
            fs::create_dir(&root).expect("create temp fixture");
            Self { root }
        }

        fn path(&self, name: &str) -> String {
            self.root
                .join(name)
                .to_str()
                .expect("fixture path utf-8")
                .to_string()
        }

        fn write_file(&self, name: &str, mode: u32) {
            let path = self.root.join(name);
            fs::write(&path, b"fixture").expect("write fixture file");
            let permissions = fs::Permissions::from_mode(mode);
            fs::set_permissions(&path, permissions).expect("set fixture permissions");
        }
    }

    #[cfg(unix)]
    fn set_mtime(path: &str, time: SystemTime) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let duration = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("mtime after unix epoch");
        let path = CString::new(std::path::Path::new(path).as_os_str().as_bytes())
            .expect("c path");
        let times = [
            libc::timespec {
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos() as libc::c_long,
            },
            libc::timespec {
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos() as libc::c_long,
            },
        ];

        let result = unsafe {
            libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0)
        };
        assert_eq!(result, 0, "set mtime for {path:?}");
    }

    #[cfg(not(unix))]
    fn set_mtime(_path: &str, _time: SystemTime) {
        panic!("mtime test fixture requires unix host support");
    }

    impl Drop for TempFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
