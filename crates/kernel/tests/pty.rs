use agentos_kernel::pty::{
    LineDisciplineConfig, PartialTermios, PartialTermiosControlChars, PtyManager, MAX_CANON,
    MAX_PTY_BUFFER_BYTES, SIGINT,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn wait_for(predicate: impl Fn() -> bool, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(predicate(), "condition should become true before timeout");
}

#[test]
fn raw_mode_leases_unwind_nested_owners_after_out_of_order_exit() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    let description_id = pty.slave.description.id();

    let first = manager
        .set_raw_mode(description_id, Some(101), true)
        .expect("first foreground owner enters raw mode")
        .expect("first foreground owner receives a lease");
    let second = manager
        .set_raw_mode(description_id, Some(102), true)
        .expect("nested foreground owner enters raw mode")
        .expect("nested foreground owner receives a lease");

    assert!(manager
        .release_raw_mode(description_id, 101, first)
        .expect("older owner exits first"));
    let still_raw = manager
        .get_termios(description_id)
        .expect("read nested raw state");
    assert!(!still_raw.icanon);
    assert!(!still_raw.echo);

    assert!(manager
        .release_raw_mode(description_id, 102, second)
        .expect("newer owner exits last"));
    let restored = manager
        .get_termios(description_id)
        .expect("read restored state");
    assert!(restored.icanon);
    assert!(restored.echo);
    assert!(restored.icrnl);
    assert!(restored.opost);
}

#[test]
fn stale_raw_mode_lease_does_not_overwrite_newer_termios_change() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    let description_id = pty.slave.description.id();
    let generation = manager
        .set_raw_mode(description_id, Some(201), true)
        .expect("foreground owner enters raw mode")
        .expect("foreground owner receives a lease");

    manager
        .set_discipline(
            description_id,
            LineDisciplineConfig {
                echo: Some(true),
                ..Default::default()
            },
        )
        .expect("newer process changes terminal state");
    assert!(manager
        .release_raw_mode(description_id, 201, generation)
        .expect("release stale owner"));

    let current = manager
        .get_termios(description_id)
        .expect("read current termios");
    assert!(current.echo, "newer echo change must survive stale cleanup");
    assert!(
        !current.icanon,
        "stale cleanup must not restore the older canonical snapshot"
    );
}

#[test]
fn background_raw_mode_change_does_not_create_recovery_lease() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    let description_id = pty.slave.description.id();

    let generation = manager
        .set_raw_mode(description_id, None, true)
        .expect("background raw-mode request");
    assert_eq!(generation, None);
    assert!(
        !manager
            .get_termios(description_id)
            .expect("read background mutation")
            .icanon
    );
}

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
                ..Default::default()
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
fn raw_mode_pending_short_read_buffers_remaining_bytes() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    manager
        .set_discipline(
            pty.master.description.id(),
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
                ..Default::default()
            },
        )
        .expect("set raw mode");

    let reader = {
        let manager = manager.clone();
        let slave_id = pty.slave.description.id();
        std::thread::spawn(move || {
            manager
                .read_with_timeout(slave_id, 1, Some(Duration::from_secs(1)))
                .expect("pending short read")
                .expect("first byte should be delivered")
        })
    };

    manager
        .write(pty.master.description.id(), b"hello")
        .expect("write raw input");

    let first = reader.join().expect("reader thread should finish");
    assert_eq!(first, b"h");

    let remaining = manager
        .read(pty.slave.description.id(), 64)
        .expect("read remaining bytes")
        .expect("remaining bytes should stay buffered");
    assert_eq!(remaining, b"ello");
}

#[test]
fn split_delivery_with_second_queued_reader_leaves_no_stale_waiters() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    manager
        .set_discipline(
            pty.master.description.id(),
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
                ..Default::default()
            },
        )
        .expect("set raw mode");

    let slave_id = pty.slave.description.id();

    // Reader A asks for one byte and must be first in the waiter queue.
    let reader_a = {
        let manager = manager.clone();
        std::thread::spawn(move || {
            manager
                .read_with_timeout(slave_id, 1, Some(Duration::from_secs(5)))
                .expect("first read should succeed")
                .expect("first read should deliver data")
        })
    };
    wait_for(
        || manager.pending_read_waiter_count() == 1,
        Duration::from_secs(1),
    );

    // Reader B queues behind A and will pick up the buffered tail.
    let reader_b = {
        let manager = manager.clone();
        std::thread::spawn(move || {
            manager
                .read_with_timeout(slave_id, 64, Some(Duration::from_secs(5)))
                .expect("second read should succeed")
                .expect("second read should deliver data")
        })
    };
    wait_for(
        || manager.pending_read_waiter_count() == 2,
        Duration::from_secs(1),
    );

    // The split delivery hands "h" to reader A and buffers "ello", which
    // reader B drains directly from the input buffer.
    manager
        .write(pty.master.description.id(), b"hello")
        .expect("write raw input");

    assert_eq!(reader_a.join().expect("reader A should finish"), b"h");
    assert_eq!(reader_b.join().expect("reader B should finish"), b"ello");

    // Reader B returned via the direct buffer-drain path, so its waiter
    // entry and queue id must be gone.
    assert_eq!(manager.pending_read_waiter_count(), 0);
    assert_eq!(manager.queued_read_waiter_count(), 0);

    // A stale waiter would swallow this write and the read would time out.
    manager
        .write(pty.master.description.id(), b"world")
        .expect("write after split delivery");
    let follow_up = manager
        .read_with_timeout(slave_id, 64, Some(Duration::from_secs(1)))
        .expect("follow-up read should succeed")
        .expect("follow-up read should deliver data");
    assert_eq!(follow_up, b"world");
}

#[test]
fn split_output_delivery_with_second_queued_reader_leaves_no_stale_waiters() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    manager
        .set_discipline(
            pty.master.description.id(),
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
                ..Default::default()
            },
        )
        .expect("set raw mode");

    let master_id = pty.master.description.id();

    // Reader A asks for one byte and must be first in the waiter queue.
    let reader_a = {
        let manager = manager.clone();
        std::thread::spawn(move || {
            manager
                .read_with_timeout(master_id, 1, Some(Duration::from_secs(5)))
                .expect("first read should succeed")
                .expect("first read should deliver data")
        })
    };
    wait_for(
        || manager.pending_read_waiter_count() == 1,
        Duration::from_secs(1),
    );

    // Reader B queues behind A and will pick up the buffered tail.
    let reader_b = {
        let manager = manager.clone();
        std::thread::spawn(move || {
            manager
                .read_with_timeout(master_id, 64, Some(Duration::from_secs(5)))
                .expect("second read should succeed")
                .expect("second read should deliver data")
        })
    };
    wait_for(
        || manager.pending_read_waiter_count() == 2,
        Duration::from_secs(1),
    );

    // The split delivery hands "h" to reader A and buffers "ello", which
    // reader B drains directly from the output buffer.
    manager
        .write(pty.slave.description.id(), b"hello")
        .expect("write slave output");

    assert_eq!(reader_a.join().expect("reader A should finish"), b"h");
    assert_eq!(reader_b.join().expect("reader B should finish"), b"ello");

    // Reader B returned via the direct buffer-drain path, so its waiter
    // entry and queue id must be gone.
    assert_eq!(manager.pending_read_waiter_count(), 0);
    assert_eq!(manager.queued_read_waiter_count(), 0);

    // A stale waiter would swallow this write and the read would time out.
    manager
        .write(pty.slave.description.id(), b"world")
        .expect("write after split delivery");
    let follow_up = manager
        .read_with_timeout(master_id, 64, Some(Duration::from_secs(1)))
        .expect("follow-up read should succeed")
        .expect("follow-up read should deliver data");
    assert_eq!(follow_up, b"world");
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
fn canonical_mode_eof_on_empty_line_returns_hangup_once() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .write(pty.master.description.id(), [0x04])
        .expect("write eof char");

    let eof = manager
        .read(pty.slave.description.id(), 64)
        .expect("read eof marker");
    assert_eq!(eof, None);

    manager
        .write(pty.master.description.id(), b"after\n")
        .expect("write after eof marker");
    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read line after eof")
        .expect("line should be available");
    assert_eq!(line, b"after\n");
}

#[test]
fn canonical_mode_eof_after_pending_text_delivers_text_without_eof_byte() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .write(pty.master.description.id(), b"partial\x04")
        .expect("write partial line followed by eof");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read partial line")
        .expect("partial line should be delivered");
    assert_eq!(line, b"partial");
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
fn window_size_reports_default_and_resize_updates() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    let initial = manager
        .window_size(pty.slave.description.id())
        .expect("read initial pty size");
    assert_eq!((initial.cols, initial.rows), (80, 24));

    manager
        .resize(pty.master.description.id(), 100, 20)
        .expect("resize pty");
    let resized = manager
        .window_size(pty.slave.description.id())
        .expect("read resized pty size");
    assert_eq!((resized.cols, resized.rows), (100, 20));
}

#[test]
fn control_character_without_foreground_process_group_clears_canonical_line() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .write(pty.master.description.id(), b"partial command")
        .expect("write partial line");
    manager
        .write(pty.master.description.id(), [0x03])
        .expect("write intr char");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical interrupt fallback")
        .expect("fallback line should be available");
    assert_eq!(line, b"\n");

    let echo = manager
        .read(pty.master.description.id(), 64)
        .expect("read interrupt echo")
        .expect("echo should be available");
    assert_eq!(
        String::from_utf8(echo).expect("valid utf8"),
        "partial command^C\r\n"
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
                ..Default::default()
            },
        )
        .expect("set raw mode");

    let error = manager
        .write(
            pty.master.description.id(),
            vec![b'x'; MAX_PTY_BUFFER_BYTES + 1],
        )
        .expect_err("oversized write should fail");
    assert_eq!(error.code(), "E2BIG");

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
fn canonical_echo_backpressure_does_not_mutate_pending_line() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .write(pty.slave.description.id(), vec![b'x'; MAX_PTY_BUFFER_BYTES])
        .expect("fill master output buffer");

    let error = manager
        .write(pty.master.description.id(), b"a")
        .expect_err("echo backpressure should reject the input byte");
    assert_eq!(error.code(), "EAGAIN");

    let drained = manager
        .read(pty.master.description.id(), MAX_PTY_BUFFER_BYTES)
        .expect("read full echo buffer")
        .expect("echo buffer should have data");
    assert_eq!(drained.len(), MAX_PTY_BUFFER_BYTES);

    manager
        .write(pty.master.description.id(), b"\n")
        .expect("newline should succeed after draining echo buffer");
    let line = manager
        .read(pty.slave.description.id(), 16)
        .expect("read canonical line")
        .expect("line should be delivered");

    assert_eq!(line, b"\n");
}

#[test]
fn many_pending_reads_are_cleaned_up_when_peer_closes() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    let reader_count = 64;
    let mut readers = Vec::new();

    for _ in 0..reader_count {
        let manager = manager.clone();
        let slave_id = pty.slave.description.id();
        readers.push(std::thread::spawn(move || {
            manager
                .read_with_timeout(slave_id, 1, Some(Duration::from_secs(5)))
                .expect("read should finish on peer close")
        }));
    }

    wait_for(
        || manager.pending_read_waiter_count() == reader_count,
        Duration::from_secs(1),
    );

    manager.close(pty.master.description.id());

    for reader in readers {
        assert_eq!(reader.join().expect("reader thread should finish"), None);
    }
    assert_eq!(manager.pending_read_waiter_count(), 0);
    assert_eq!(manager.queued_read_waiter_count(), 0);
}

#[test]
fn many_timed_out_reads_are_removed_from_waiter_queues() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();
    let reader_count = 64;
    let mut readers = Vec::new();

    for _ in 0..reader_count {
        let manager = manager.clone();
        let slave_id = pty.slave.description.id();
        readers.push(std::thread::spawn(move || {
            manager
                .read_with_timeout(slave_id, 1, Some(Duration::from_millis(25)))
                .expect_err("read should time out")
                .code()
        }));
    }

    for reader in readers {
        assert_eq!(
            reader.join().expect("reader thread should finish"),
            "EAGAIN"
        );
    }
    assert_eq!(manager.pending_read_waiter_count(), 0);
    assert_eq!(manager.queued_read_waiter_count(), 0);
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
                ..Default::default()
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

#[test]
fn canonical_mode_kill_erases_the_whole_line() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    // Type "abc", then VKILL (Ctrl-U, 0x15) to wipe the line, then "xy\n".
    manager
        .write(pty.master.description.id(), b"abc\x15xy\n")
        .expect("write canonical input with kill");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical line")
        .expect("line should be available");
    assert_eq!(String::from_utf8(line).expect("valid utf8"), "xy\n");

    let echo = manager
        .read(pty.master.description.id(), 64)
        .expect("read echo")
        .expect("echo should be available");
    // "abc" echoed, then three BS-SP-BS to erase them, then "xy", then CRLF.
    assert_eq!(
        String::from_utf8(echo).expect("valid utf8"),
        "abc\x08 \x08\x08 \x08\x08 \x08xy\r\n"
    );
}

#[test]
fn canonical_mode_kill_on_empty_line_is_noop() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    manager
        .write(pty.master.description.id(), b"\x15hi\n")
        .expect("write kill on empty line");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical line")
        .expect("line should be available");
    assert_eq!(String::from_utf8(line).expect("valid utf8"), "hi\n");

    let echo = manager
        .read(pty.master.description.id(), 64)
        .expect("read echo")
        .expect("echo should be available");
    // No erase output for the empty-line kill; just the "hi" echo + CRLF.
    assert_eq!(String::from_utf8(echo).expect("valid utf8"), "hi\r\n");
}

#[test]
fn canonical_mode_werase_erases_preceding_word_and_trailing_space() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    // "foo bar " then VWERASE (Ctrl-W, 0x17) erases the trailing space and
    // "bar", leaving "foo ", then "baz\n".
    manager
        .write(pty.master.description.id(), b"foo bar \x17baz\n")
        .expect("write canonical input with werase");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical line")
        .expect("line should be available");
    assert_eq!(String::from_utf8(line).expect("valid utf8"), "foo baz\n");

    let echo = manager
        .read(pty.master.description.id(), 64)
        .expect("read echo")
        .expect("echo should be available");
    // "foo bar " echoed (8 chars), then 4 BS-SP-BS erases (space + "bar"),
    // then "baz", then CRLF.
    let mut expected = String::from("foo bar ");
    expected.push_str(&"\x08 \x08".repeat(4));
    expected.push_str("baz\r\n");
    assert_eq!(String::from_utf8(echo).expect("valid utf8"), expected);
}

#[test]
fn canonical_mode_werase_on_leading_whitespace_only_erases_it() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    // Only whitespace before VWERASE: erase all of it, nothing else.
    manager
        .write(pty.master.description.id(), b"   \x17done\n")
        .expect("write whitespace then werase");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical line")
        .expect("line should be available");
    assert_eq!(String::from_utf8(line).expect("valid utf8"), "done\n");
}

#[test]
fn canonical_mode_echoctl_echoes_control_char_in_caret_form() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    // A control char that is neither a signal, VEOF, VERASE, VKILL, VWERASE,
    // nor newline (0x01 = Ctrl-A) is buffered and echoed as caret form "^A".
    manager
        .write(pty.master.description.id(), b"a\x01b\n")
        .expect("write control char in canonical mode");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical line")
        .expect("line should be available");
    // The raw control byte is delivered to the slave verbatim.
    assert_eq!(line, b"a\x01b\n");

    let echo = manager
        .read(pty.master.description.id(), 64)
        .expect("read echo")
        .expect("echo should be available");
    assert_eq!(String::from_utf8(echo).expect("valid utf8"), "a^Ab\r\n");
}

#[test]
fn canonical_mode_erase_after_echoctl_removes_both_caret_columns() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    // Type Ctrl-A (echoed "^A", two columns), then VERASE (0x7f): the erase
    // must remove both caret columns, then "z\n".
    manager
        .write(pty.master.description.id(), b"\x01\x7fz\n")
        .expect("write control char then erase");

    let line = manager
        .read(pty.slave.description.id(), 64)
        .expect("read canonical line")
        .expect("line should be available");
    assert_eq!(line, b"z\n");

    let echo = manager
        .read(pty.master.description.id(), 64)
        .expect("read echo")
        .expect("echo should be available");
    // "^A" echoed, then two BS-SP-BS to erase both columns, then "z", then CRLF.
    let mut expected = String::from("^A");
    expected.push_str(&"\x08 \x08".repeat(2));
    expected.push_str("z\r\n");
    assert_eq!(String::from_utf8(echo).expect("valid utf8"), expected);
}

#[test]
fn set_termios_updates_kill_and_werase_control_chars() {
    let manager = PtyManager::new();
    let pty = manager.create_pty();

    let termios = manager
        .get_termios(pty.master.description.id())
        .expect("read default termios");
    assert_eq!(termios.cc.vkill, 0x15);
    assert_eq!(termios.cc.vwerase, 0x17);

    manager
        .set_termios(
            pty.master.description.id(),
            PartialTermios {
                cc: Some(PartialTermiosControlChars {
                    vkill: Some(0x18),
                    vwerase: Some(0x1a),
                    ..PartialTermiosControlChars::default()
                }),
                ..PartialTermios::default()
            },
        )
        .expect("merge termios update");

    let termios = manager
        .get_termios(pty.master.description.id())
        .expect("read merged termios");
    assert_eq!(termios.cc.vkill, 0x18);
    assert_eq!(termios.cc.vwerase, 0x1a);
    // Unspecified control chars keep their defaults.
    assert_eq!(termios.cc.verase, 0x7f);
}
