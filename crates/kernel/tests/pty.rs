use agent_os_kernel::pty::{
    LineDisciplineConfig, PartialTermios, PartialTermiosControlChars, PtyManager, MAX_CANON,
    MAX_PTY_BUFFER_BYTES, SIGINT,
};
use std::sync::{Arc, Mutex};

#[test]
fn raw_mode_delivers_bytes_and_applies_icrnl_translation() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    manager
        .set_discipline(
            pty.master.description.id(),
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
            },
        )
        .expect("set raw mode");

    manager
        .write(pty.master.description.id(), b"hello\rworld")
        .expect("write master");
    let data = manager
        .read(pty.slave.description.id(), 64)
        .expect("read slave")
        .expect("slave should receive data");

    assert_eq!(String::from_utf8(data).expect("valid utf8"), "hello\nworld");
}

#[test]
fn canonical_mode_buffers_until_newline_and_honors_backspace() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .write(pty.master.description.id(), b"echo helo\x7flo\n")
        .expect("write canonical input");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical line")
        .expect("line should be available");
    assert_eq!(String::from_utf8(line).expect("valid utf8"), "echo hello\n");

    let echo = manager
        .read(pty.master.description.id(), 64)
        .expect("read echo")
        .expect("echo should be available");
    assert_eq!(
        String::from_utf8(echo).expect("valid utf8"),
        "echo helo\x08 \x08lo\r\n"
    );
}

#[test]
fn control_characters_signal_the_foreground_process_group() {
    let signals = Arc::new(Mutex::new(Vec::new()));
    let signal_log = Arc::clone(&signals);
    let manager = PtyManager::with_signal_handler(Arc::new(move |pgid, signal| {
        signal_log
            .lock()
            .expect("signal log lock poisoned")
            .push((pgid, signal));
    }));
    let pty = manager.create_pty();

    manager
        .set_foreground_pgid(pty.master.description.id(), 42)
        .expect("set foreground pgid");
    manager
        .write(pty.master.description.id(), [0x03])
        .expect("write intr char");

    assert_eq!(
        *signals.lock().expect("signal log lock poisoned"),
        vec![(42, SIGINT)]
    );
}

#[test]
fn peer_close_returns_hangup_instead_of_blocking() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager.close(pty.master.description.id());
    let result = manager
        .read(pty.slave.description.id(), 16)
        .expect("read after hangup");

    assert_eq!(result, None);
}

#[test]
fn oversized_raw_write_fails_atomically() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    manager
        .set_discipline(
            pty.master.description.id(),
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
            },
        )
        .expect("set raw mode");

    let error = manager
        .write(
            pty.master.description.id(),
            vec![b'x'; MAX_PTY_BUFFER_BYTES + 1],
        )
        .expect_err("oversized write should fail");
    assert_eq!(error.code(), "EAGAIN");

    manager
        .write(pty.master.description.id(), vec![b'a'; MAX_CANON.min(8)])
        .expect("subsequent small write should still succeed");
    let data = manager
        .read(pty.slave.description.id(), 16)
        .expect("read after failed write")
        .expect("data should be buffered");
    assert_eq!(data, vec![b'a'; MAX_CANON.min(8)]);
}

#[test]
fn set_discipline_only_updates_requested_fields() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .set_discipline(
            pty.master.description.id(),
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
            },
        )
        .expect("set initial raw mode");
    manager
        .set_discipline(
            pty.master.description.id(),
            LineDisciplineConfig {
                echo: Some(true),
                ..LineDisciplineConfig::default()
            },
        )
        .expect("enable echo only");

    let termios = manager
        .get_termios(pty.master.description.id())
        .expect("read merged termios");
    assert!(!termios.icanon);
    assert!(termios.echo);
    assert!(!termios.isig);
}

#[test]
fn set_termios_only_updates_requested_fields() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .set_termios(
            pty.master.description.id(),
            PartialTermios {
                echo: Some(false),
                cc: Some(PartialTermiosControlChars {
                    verase: Some(0x08),
                    ..PartialTermiosControlChars::default()
                }),
                ..PartialTermios::default()
            },
        )
        .expect("merge termios update");

    let termios = manager
        .get_termios(pty.master.description.id())
        .expect("read merged termios");
    assert!(termios.icrnl);
    assert!(termios.icanon);
    assert!(!termios.echo);
    assert_eq!(termios.cc.verase, 0x08);
    assert_eq!(termios.cc.vintr, 0x03);
}
