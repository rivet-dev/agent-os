use agentos_bridge::ExecutionSignal;
use agentos_sidecar_protocol::protocol::{SignalDispositionAction, SignalHandlerRegistration};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn execution_signal_to_kernel(signal: ExecutionSignal) -> i32 {
    match signal {
        ExecutionSignal::Terminate => 15,
        ExecutionSignal::Interrupt => 2,
        ExecutionSignal::Kill => 9,
    }
}

pub fn execution_signal_from_number(signal: i32) -> Option<ExecutionSignal> {
    match signal {
        2 => Some(ExecutionSignal::Interrupt),
        9 => Some(ExecutionSignal::Kill),
        15 => Some(ExecutionSignal::Terminate),
        _ => None,
    }
}

pub fn default_signal_exit_code(signal: i32) -> Option<i32> {
    (signal > 0).then_some(128 + signal)
}

pub fn is_valid_posix_signal_number(signal: u32) -> bool {
    signal <= 64
}

pub fn parse_posix_signal(signal: &str) -> Option<i32> {
    let trimmed = signal.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = trimmed.parse::<i32>() {
        return (0..=64).contains(&value).then_some(value);
    }

    let upper = trimmed.to_ascii_uppercase();
    let normalized = upper.strip_prefix("SIG").unwrap_or(&upper);
    signal_number_from_name(normalized)
}

pub fn canonical_signal_name(signal: i32) -> Option<&'static str> {
    match signal {
        1 => Some("SIGHUP"),
        2 => Some("SIGINT"),
        3 => Some("SIGQUIT"),
        4 => Some("SIGILL"),
        5 => Some("SIGTRAP"),
        6 => Some("SIGABRT"),
        7 => Some("SIGBUS"),
        8 => Some("SIGFPE"),
        9 => Some("SIGKILL"),
        10 => Some("SIGUSR1"),
        11 => Some("SIGSEGV"),
        12 => Some("SIGUSR2"),
        13 => Some("SIGPIPE"),
        14 => Some("SIGALRM"),
        15 => Some("SIGTERM"),
        16 => Some("SIGSTKFLT"),
        17 => Some("SIGCHLD"),
        18 => Some("SIGCONT"),
        19 => Some("SIGSTOP"),
        20 => Some("SIGTSTP"),
        21 => Some("SIGTTIN"),
        22 => Some("SIGTTOU"),
        23 => Some("SIGURG"),
        24 => Some("SIGXCPU"),
        25 => Some("SIGXFSZ"),
        26 => Some("SIGVTALRM"),
        27 => Some("SIGPROF"),
        28 => Some("SIGWINCH"),
        29 => Some("SIGIO"),
        30 => Some("SIGPWR"),
        31 => Some("SIGSYS"),
        _ => None,
    }
}

pub fn signal_number_from_name(signal: &str) -> Option<i32> {
    match signal {
        "0" => Some(0),
        "HUP" => Some(1),
        "INT" => Some(2),
        "QUIT" => Some(3),
        "ILL" => Some(4),
        "TRAP" => Some(5),
        "ABRT" | "IOT" => Some(6),
        "BUS" => Some(7),
        "FPE" => Some(8),
        "KILL" => Some(9),
        "USR1" => Some(10),
        "SEGV" => Some(11),
        "USR2" => Some(12),
        "PIPE" => Some(13),
        "ALRM" => Some(14),
        "TERM" => Some(15),
        "STKFLT" => Some(16),
        "CHLD" => Some(17),
        "CONT" => Some(18),
        "STOP" => Some(19),
        "TSTP" => Some(20),
        "TTIN" => Some(21),
        "TTOU" => Some(22),
        "URG" => Some(23),
        "XCPU" => Some(24),
        "XFSZ" => Some(25),
        "VTALRM" => Some(26),
        "PROF" => Some(27),
        "WINCH" => Some(28),
        "IO" | "POLL" => Some(29),
        "PWR" => Some(30),
        "SYS" => Some(31),
        _ => None,
    }
}

pub fn parse_process_signal_state_request(
    args: &[Value],
) -> Result<(u32, SignalHandlerRegistration), crate::SidecarCoreError> {
    let signal = signal_state_u32_arg(args, 0, "process.signal_state signal")?;
    validate_process_signal_number(signal, "process.signal_state signal")?;
    let action = signal_state_str_arg(args, 1, "process.signal_state action")?;
    let mask_json = signal_state_str_arg(args, 2, "process.signal_state mask")?;
    let flags = signal_state_u32_arg(args, 3, "process.signal_state flags")?;
    let mask: Vec<u32> = serde_json::from_str(mask_json).map_err(|error| {
        crate::SidecarCoreError::typed(
            "EINVAL",
            format!("process.signal_state mask must be valid JSON: {error}"),
        )
    })?;
    for signal in &mask {
        validate_process_signal_number(*signal, "process.signal_state mask entries")?;
    }
    let action = match action.trim().to_ascii_lowercase().as_str() {
        "default" => SignalDispositionAction::Default,
        "ignore" => SignalDispositionAction::Ignore,
        "user" => SignalDispositionAction::User,
        other => {
            return Err(crate::SidecarCoreError::typed(
                "EINVAL",
                format!("unsupported process.signal_state action {other}"),
            ));
        }
    };

    Ok((
        signal,
        SignalHandlerRegistration {
            action,
            mask,
            flags,
        },
    ))
}

pub fn apply_process_signal_state_update(
    signal_states: &mut BTreeMap<String, BTreeMap<u32, SignalHandlerRegistration>>,
    process_id: &str,
    signal: u32,
    registration: SignalHandlerRegistration,
) {
    if registration.action == SignalDispositionAction::Default
        && registration.mask.is_empty()
        && registration.flags == 0
    {
        let remove_process_entry = signal_states
            .get_mut(process_id)
            .map(|handlers| {
                handlers.remove(&signal);
                handlers.is_empty()
            })
            .unwrap_or(false);
        if remove_process_entry {
            signal_states.remove(process_id);
        }
        return;
    }

    signal_states
        .entry(process_id.to_owned())
        .or_default()
        .insert(signal, registration);
}

fn validate_process_signal_number(signal: u32, label: &str) -> Result<(), crate::SidecarCoreError> {
    if is_valid_posix_signal_number(signal) {
        Ok(())
    } else {
        Err(crate::SidecarCoreError::typed(
            "EINVAL",
            format!("{label} must be a valid POSIX signal"),
        ))
    }
}

fn signal_state_u32_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<u32, crate::SidecarCoreError> {
    let value = args
        .get(index)
        .ok_or_else(|| crate::SidecarCoreError::typed("EINVAL", format!("{label} missing")))?;
    if let Some(value) = value.as_u64() {
        return u32::try_from(value).map_err(|_| {
            crate::SidecarCoreError::typed("EINVAL", format!("{label} must fit in u32"))
        });
    }
    if let Some(value) = value.as_i64() {
        return u32::try_from(value).map_err(|_| {
            crate::SidecarCoreError::typed("EINVAL", format!("{label} must fit in u32"))
        });
    }
    if let Some(value) = value.as_f64() {
        if value.is_finite() && value.fract() == 0.0 && value >= 0.0 && value <= f64::from(u32::MAX)
        {
            return Ok(value as u32);
        }
        return Err(crate::SidecarCoreError::typed(
            "EINVAL",
            format!("{label} must fit in u32"),
        ));
    }
    if let Some(value) = value.as_str() {
        return value.parse::<u32>().map_err(|error| {
            crate::SidecarCoreError::typed("EINVAL", format!("{label}: {error}"))
        });
    }
    Err(crate::SidecarCoreError::typed(
        "EINVAL",
        format!("{label} must be a u32"),
    ))
}

fn signal_state_str_arg<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, crate::SidecarCoreError> {
    args.get(index).and_then(Value::as_str).ok_or_else(|| {
        crate::SidecarCoreError::typed("EINVAL", format!("{label} must be a string"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_signal_mapping_matches_posix_defaults() {
        assert_eq!(execution_signal_to_kernel(ExecutionSignal::Interrupt), 2);
        assert_eq!(execution_signal_to_kernel(ExecutionSignal::Kill), 9);
        assert_eq!(execution_signal_to_kernel(ExecutionSignal::Terminate), 15);
        assert_eq!(
            execution_signal_from_number(2),
            Some(ExecutionSignal::Interrupt)
        );
        assert_eq!(execution_signal_from_number(9), Some(ExecutionSignal::Kill));
        assert_eq!(
            execution_signal_from_number(15),
            Some(ExecutionSignal::Terminate)
        );
        assert_eq!(execution_signal_from_number(10), None);
    }

    #[test]
    fn default_signal_exit_code_is_128_plus_signal() {
        assert_eq!(default_signal_exit_code(2), Some(130));
        assert_eq!(default_signal_exit_code(9), Some(137));
        assert_eq!(default_signal_exit_code(15), Some(143));
        assert_eq!(default_signal_exit_code(0), None);
    }

    #[test]
    fn validates_posix_signal_number_range() {
        assert!(is_valid_posix_signal_number(0));
        assert!(is_valid_posix_signal_number(64));
        assert!(!is_valid_posix_signal_number(65));
    }

    #[test]
    fn parses_signal_names_and_numbers() {
        assert_eq!(parse_posix_signal("SIGTERM"), Some(15));
        assert_eq!(parse_posix_signal("term"), Some(15));
        assert_eq!(canonical_signal_name(16), Some("SIGSTKFLT"));
        assert_eq!(parse_posix_signal("9"), Some(9));
        assert_eq!(parse_posix_signal("0"), Some(0));
        assert_eq!(parse_posix_signal("64"), Some(64));
        assert_eq!(parse_posix_signal("SIGBOGUS"), None);
        assert_eq!(parse_posix_signal("65"), None);
    }

    #[test]
    fn parses_and_applies_process_signal_state_updates() {
        let args = vec![
            Value::from(15),
            Value::from("user"),
            Value::from("[2]"),
            Value::from(0),
        ];
        let (signal, registration) =
            parse_process_signal_state_request(&args).expect("signal state");
        assert_eq!(signal, 15);
        assert_eq!(registration.action, SignalDispositionAction::User);
        assert_eq!(registration.mask, vec![2]);

        let mut states = BTreeMap::new();
        apply_process_signal_state_update(&mut states, "proc-1", signal, registration);
        assert!(states
            .get("proc-1")
            .is_some_and(|handlers| handlers.contains_key(&15)));

        apply_process_signal_state_update(
            &mut states,
            "proc-1",
            15,
            SignalHandlerRegistration {
                action: SignalDispositionAction::Default,
                mask: Vec::new(),
                flags: 0,
            },
        );
        assert!(!states.contains_key("proc-1"));
    }

    #[test]
    fn rejects_unknown_process_signal_state_values() {
        let invalid_signal = parse_process_signal_state_request(&[
            Value::from(65),
            Value::from("user"),
            Value::from("[]"),
            Value::from(0),
        ])
        .expect_err("unknown signal must fail");
        assert_eq!(invalid_signal.code(), Some("EINVAL"));
        assert_eq!(
            invalid_signal.message(),
            "process.signal_state signal must be a valid POSIX signal"
        );

        let invalid_mask = parse_process_signal_state_request(&[
            Value::from(15),
            Value::from("user"),
            Value::from("[65]"),
            Value::from(0),
        ])
        .expect_err("unknown mask signal must fail");
        assert_eq!(invalid_mask.code(), Some("EINVAL"));
        assert_eq!(
            invalid_mask.message(),
            "process.signal_state mask entries must be a valid POSIX signal"
        );
    }

    #[test]
    fn accepts_only_bounded_integral_float_signal_state_flags() {
        let parse_flags = |flags: f64| {
            parse_process_signal_state_request(&[
                Value::from(15),
                Value::from("user"),
                Value::from("[]"),
                Value::Number(serde_json::Number::from_f64(flags).expect("finite test value")),
            ])
        };

        assert_eq!(
            parse_flags(f64::from(u32::MAX))
                .expect("u32 maximum")
                .1
                .flags,
            u32::MAX
        );
        for invalid in [-1.0, 1.5, f64::from(u32::MAX) + 1.0] {
            let error = parse_flags(invalid).expect_err("invalid float flags must fail");
            assert_eq!(error.code(), Some("EINVAL"));
            assert_eq!(
                error.message(),
                "process.signal_state flags must fit in u32"
            );
        }
    }
}
