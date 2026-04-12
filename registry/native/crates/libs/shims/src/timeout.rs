//! Shim implementation of the `timeout` command.
//!
//! Spawns a child process and kills it if it exceeds the timeout duration.
//! Uses std::process::Command (which delegates to wasi-ext proc_spawn)
//! with try_wait() for non-blocking wait and kill() for termination.
//! On WASI, poll sleeps must go through `wasi_ext::host_sleep_ms()` because
//! `std::thread::sleep()` returns immediately and turns the timeout loop into
//! a hot busy-wait.
//!
//! Usage:
//!   timeout DURATION COMMAND [ARG]...
//!
//! DURATION is in seconds (fractional allowed, e.g. 0.5).
//! Exit codes:
//!   124 - command timed out
//!   125 - timeout itself failed
//!   126 - command found but not executable
//!   127 - command not found

use std::ffi::OsString;
use std::time::Duration;

const INITIAL_POLL_SLEEP_MS: u32 = 1;
const MAX_POLL_SLEEP_MS: u32 = 128;

pub fn timeout(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0] ("timeout")
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    if str_args.is_empty() {
        eprintln!("timeout: missing operand");
        return 125;
    }

    if str_args.len() < 2 {
        eprintln!("timeout: missing operand after '{}'", str_args[0]);
        return 125;
    }

    let duration_secs: f64 = match str_args[0].parse() {
        Ok(d) if d >= 0.0 => d,
        _ => {
            eprintln!("timeout: invalid time interval '{}'", str_args[0]);
            return 125;
        }
    };

    let program = &str_args[1];
    let child_args = &str_args[2..];

    let mut child = match std::process::Command::new(program).args(child_args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("timeout: failed to run command '{}': {}", program, e);
            return 127;
        }
    };

    let start = std::time::Instant::now();
    let timeout_duration = Duration::from_secs_f64(duration_secs);
    let mut poll_sleep_ms = INITIAL_POLL_SLEEP_MS;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Child exited on its own
                return status.code().unwrap_or(1);
            }
            Ok(None) => {
                // Still running — check timeout
                if start.elapsed() >= timeout_duration {
                    // Timeout exceeded — kill the child
                    let _ = child.kill();
                    let _ = child.wait(); // reap
                    return 124;
                }
                let remaining = timeout_duration.saturating_sub(start.elapsed());
                let sleep_ms = next_poll_sleep_ms(poll_sleep_ms, remaining);
                if let Err(error) = sleep_for_poll(Duration::from_millis(u64::from(sleep_ms))) {
                    eprintln!("timeout: failed to sleep while waiting for command: {error}");
                    return 125;
                }
                poll_sleep_ms = poll_sleep_ms.saturating_mul(2).min(MAX_POLL_SLEEP_MS);
            }
            Err(e) => {
                eprintln!("timeout: error waiting for command: {}", e);
                return 125;
            }
        }
    }
}

fn next_poll_sleep_ms(requested_ms: u32, remaining: Duration) -> u32 {
    let remaining_ms = ceil_duration_to_millis(remaining);
    requested_ms.max(1).min(remaining_ms.max(1))
}

fn ceil_duration_to_millis(duration: Duration) -> u32 {
    let millis = duration.as_millis();
    if millis == 0 && !duration.is_zero() {
        return 1;
    }

    millis.try_into().unwrap_or(u32::MAX)
}

fn sleep_for_poll(duration: Duration) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let millis = ceil_duration_to_millis(duration);
        wasi_ext::host_sleep_ms(millis).map_err(|errno| format!("wasi errno {errno}"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::sleep(duration);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ceil_duration_to_millis, next_poll_sleep_ms, MAX_POLL_SLEEP_MS};
    use std::time::Duration;

    #[test]
    fn poll_sleep_is_capped_by_remaining_time() {
        assert_eq!(
            next_poll_sleep_ms(MAX_POLL_SLEEP_MS, Duration::from_millis(40)),
            40
        );
    }

    #[test]
    fn poll_sleep_uses_one_millisecond_floor_for_submillisecond_remaining_time() {
        assert_eq!(next_poll_sleep_ms(8, Duration::from_micros(250)), 1);
        assert_eq!(ceil_duration_to_millis(Duration::from_micros(250)), 1);
    }

    #[test]
    fn poll_sleep_preserves_requested_delay_when_deadline_allows_it() {
        assert_eq!(next_poll_sleep_ms(32, Duration::from_secs(2)), 32);
    }
}
