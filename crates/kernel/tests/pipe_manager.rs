use agentos_kernel::fd_table::{
    FdResult, FdTableManager, FILETYPE_PIPE, O_NONBLOCK, O_RDONLY, O_RDWR, O_WRONLY,
};
use agentos_kernel::pipe_manager::{
    PipeManager, PipeResult, MAX_PIPE_BUFFER_BYTES, PIPE_BUF_BYTES,
};
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn assert_pipe_error<T: Debug>(result: PipeResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn assert_fd_error<T: Debug>(result: FdResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn wait_for_waiting_reader(manager: &PipeManager, description_id: u64) {
    wait_for_waiting_readers(manager, description_id, 1);
}

fn wait_for_waiting_readers(manager: &PipeManager, description_id: u64, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let count = manager
            .waiting_reader_count(description_id)
            .expect("pipe should still exist");
        if count >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected} waiting readers on pipe description {description_id}, got {count}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn create_pipe_returns_distinct_read_and_write_descriptions() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();

    assert_ne!(pipe.read.description.id(), pipe.write.description.id());
    assert!(manager.is_pipe(pipe.read.description.id()));
    assert!(manager.is_pipe(pipe.write.description.id()));
}

#[test]
fn named_pipe_blocking_open_rendezvous_and_transfers_bytes() {
    let manager = PipeManager::new();
    let reader_manager = manager.clone();
    let reader = thread::spawn(move || {
        reader_manager
            .open_named_pipe((7, 42), "/tmp/fifo", O_RDONLY, Some(Duration::from_secs(1)))
            .expect("reader should rendezvous with writer")
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    while !manager.has_named_pipe((7, 42)) {
        assert!(
            Instant::now() < deadline,
            "reader did not register named pipe"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let writer = manager
        .open_named_pipe((7, 42), "/tmp/fifo", O_WRONLY, Some(Duration::from_secs(1)))
        .expect("writer should observe waiting reader");
    let reader = reader.join().expect("reader thread should finish");

    manager
        .write(writer.description.id(), b"named")
        .expect("write named pipe");
    assert_eq!(
        manager
            .read(reader.description.id(), 16)
            .expect("read named pipe")
            .expect("named pipe should contain bytes"),
        b"named"
    );
}

#[test]
fn named_pipe_nonblocking_open_matches_linux_peer_rules() {
    let manager = PipeManager::new();
    assert_pipe_error(
        manager.open_named_pipe(
            (7, 43),
            "/tmp/fifo",
            O_WRONLY | O_NONBLOCK,
            Some(Duration::ZERO),
        ),
        "ENXIO",
    );

    let reader = manager
        .open_named_pipe(
            (7, 43),
            "/tmp/fifo",
            O_RDONLY | O_NONBLOCK,
            Some(Duration::ZERO),
        )
        .expect("nonblocking reader opens without a writer");
    let writer = manager
        .open_named_pipe(
            (7, 43),
            "/tmp/fifo",
            O_WRONLY | O_NONBLOCK,
            Some(Duration::ZERO),
        )
        .expect("nonblocking writer opens when reader exists");
    let duplex = manager
        .open_named_pipe((7, 43), "/tmp/fifo", O_RDWR, Some(Duration::ZERO))
        .expect("read-write FIFO open never waits for a peer");
    assert!(manager.is_pipe(reader.description.id()));
    assert!(manager.is_pipe(writer.description.id()));
    assert!(manager.is_pipe(duplex.description.id()));
}

#[test]
fn named_pipe_peer_readiness_tracks_opposite_endpoints() {
    let manager = PipeManager::new();
    let reader = manager
        .open_named_pipe(
            (7, 44),
            "/tmp/fifo-ready",
            O_RDONLY | O_NONBLOCK,
            Some(Duration::ZERO),
        )
        .expect("nonblocking reader opens without a writer");
    assert_eq!(
        manager
            .named_pipe_peer_ready(reader.description.id())
            .expect("reader readiness"),
        Some(false)
    );

    let writer = manager
        .open_named_pipe(
            (7, 44),
            "/tmp/fifo-ready",
            O_WRONLY | O_NONBLOCK,
            Some(Duration::ZERO),
        )
        .expect("writer opens after reader");
    assert_eq!(
        manager
            .named_pipe_peer_ready(reader.description.id())
            .expect("reader readiness after writer"),
        Some(true)
    );
    assert_eq!(
        manager
            .named_pipe_peer_ready(writer.description.id())
            .expect("writer readiness"),
        Some(true)
    );
}

#[test]
fn buffered_writes_are_read_back_and_partial_reads_preserve_remainder() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();

    manager
        .write(pipe.write.description.id(), b"hello world")
        .expect("write pipe contents");

    let first = manager
        .read(pipe.read.description.id(), 5)
        .expect("read first slice")
        .expect("first slice should contain data");
    let second = manager
        .read(pipe.read.description.id(), 1024)
        .expect("read remainder")
        .expect("remainder should contain data");

    assert_eq!(String::from_utf8(first).expect("utf8"), "hello");
    assert_eq!(String::from_utf8(second).expect("utf8"), " world");
}

#[test]
fn read_blocks_until_a_writer_delivers_data() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let write_id = pipe.write.description.id();
    let reader = manager.clone();

    let handle = thread::spawn(move || {
        reader
            .read(read_id, 1024)
            .expect("blocking read should succeed")
            .expect("blocking read should produce data")
    });

    thread::sleep(Duration::from_millis(10));
    manager
        .write(write_id, b"delayed")
        .expect("write delayed payload");

    assert_eq!(
        String::from_utf8(handle.join().expect("reader thread should finish")).expect("utf8"),
        "delayed"
    );
}

#[test]
fn closing_the_write_end_delivers_eof_to_waiting_readers() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let write_id = pipe.write.description.id();
    let reader = manager.clone();

    let handle = thread::spawn(move || reader.read(read_id, 1024).expect("blocking read"));
    wait_for_waiting_reader(&manager, read_id);
    manager.close(write_id);

    assert_eq!(handle.join().expect("reader thread should finish"), None);
}

#[test]
fn closing_the_read_end_does_not_wake_waiting_readers() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let write_id = pipe.write.description.id();
    let reader = manager.clone();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_for_thread = Arc::clone(&completed);

    let handle = thread::spawn(move || {
        let result = reader.read(read_id, 1024).expect("blocking read");
        completed_for_thread.store(true, Ordering::SeqCst);
        result
    });

    wait_for_waiting_reader(&manager, read_id);
    manager.close(read_id);
    thread::sleep(Duration::from_millis(25));
    assert!(!completed.load(Ordering::SeqCst));

    manager.close(write_id);
    assert_eq!(handle.join().expect("reader thread should finish"), None);
}

#[test]
fn buffer_limit_is_enforced_until_the_reader_drains_the_pipe() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();

    manager
        .write(pipe.write.description.id(), vec![0; MAX_PIPE_BUFFER_BYTES])
        .expect("fill pipe buffer");
    assert_pipe_error(manager.write(pipe.write.description.id(), [1]), "EAGAIN");

    let drained = manager
        .read(pipe.read.description.id(), MAX_PIPE_BUFFER_BYTES)
        .expect("drain buffer")
        .expect("buffer should contain data");
    assert_eq!(drained.len(), MAX_PIPE_BUFFER_BYTES);

    manager
        .write(pipe.write.description.id(), vec![2; 1024])
        .expect("write after draining");
}

#[test]
fn blocking_small_writes_wait_for_full_pipe_buf_capacity() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let write_id = pipe.write.description.id();
    let writer = manager.clone();

    manager
        .write(
            write_id,
            vec![0; MAX_PIPE_BUFFER_BYTES - (PIPE_BUF_BYTES - 1)],
        )
        .expect("fill pipe to one byte below PIPE_BUF headroom");

    let handle = thread::spawn(move || {
        writer
            .write_blocking(write_id, vec![1; PIPE_BUF_BYTES])
            .expect("small blocking write should eventually succeed")
    });

    thread::sleep(Duration::from_millis(25));
    assert!(
        !handle.is_finished(),
        "PIPE_BUF-sized write should wait until the full chunk fits"
    );

    let first = manager
        .read(read_id, 1)
        .expect("drain one byte")
        .expect("byte should be present");
    assert_eq!(first, vec![0]);

    assert_eq!(
        handle.join().expect("writer thread should finish"),
        PIPE_BUF_BYTES
    );

    let drained = manager
        .read(read_id, MAX_PIPE_BUFFER_BYTES)
        .expect("drain remainder")
        .expect("remainder should be present");
    assert_eq!(drained.len(), MAX_PIPE_BUFFER_BYTES);
    assert!(drained[..drained.len() - PIPE_BUF_BYTES]
        .iter()
        .all(|byte| *byte == 0));
    assert!(drained[drained.len() - PIPE_BUF_BYTES..]
        .iter()
        .all(|byte| *byte == 1));
}

#[test]
fn waiting_reader_receives_large_writes_without_hitting_the_buffer_limit() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let write_id = pipe.write.description.id();
    let reader = manager.clone();

    let handle = thread::spawn(move || {
        reader
            .read(read_id, MAX_PIPE_BUFFER_BYTES + 1024)
            .expect("blocking read should succeed")
            .expect("blocking read should receive data")
            .len()
    });

    wait_for_waiting_reader(&manager, read_id);
    manager
        .write(write_id, vec![7; MAX_PIPE_BUFFER_BYTES + 1024])
        .expect("large direct write should bypass buffering");

    assert_eq!(
        handle.join().expect("reader thread should finish"),
        MAX_PIPE_BUFFER_BYTES + 1024
    );
}

#[test]
fn direct_handoff_honors_waiting_reader_length_and_buffers_the_remainder() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let write_id = pipe.write.description.id();
    let reader = manager.clone();

    let handle = thread::spawn(move || {
        reader
            .read(read_id, 10)
            .expect("blocking read should succeed")
            .expect("blocking read should receive data")
    });

    wait_for_waiting_reader(&manager, read_id);
    manager
        .write(write_id, vec![7; 1024])
        .expect("direct handoff write should succeed");

    let first = handle.join().expect("reader thread should finish");
    let second = manager
        .read(read_id, 2048)
        .expect("remainder read should succeed")
        .expect("remainder should stay buffered");

    assert_eq!(first, vec![7; 10]);
    assert_eq!(second.len(), 1014);
    assert!(second.iter().all(|byte| *byte == 7));
}

#[test]
fn many_waiting_readers_are_cleaned_up_when_the_write_end_closes() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let write_id = pipe.write.description.id();
    let mut handles = Vec::new();

    for _ in 0..32 {
        let reader = manager.clone();
        handles.push(thread::spawn(move || {
            reader.read(read_id, 1024).expect("blocking read")
        }));
    }

    wait_for_waiting_readers(&manager, read_id, handles.len());
    manager.close(write_id);

    for handle in handles {
        assert_eq!(handle.join().expect("reader thread should finish"), None);
    }
    assert_eq!(manager.pending_read_waiter_count(), 0);
    assert_eq!(
        manager
            .waiting_reader_count(read_id)
            .expect("pipe should remain until read end closes"),
        0
    );
}

#[test]
fn many_timed_out_readers_are_removed_from_the_waiting_queue() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();
    let read_id = pipe.read.description.id();
    let mut handles = Vec::new();

    for _ in 0..32 {
        let reader = manager.clone();
        handles.push(thread::spawn(move || {
            reader
                .read_with_timeout(read_id, 1024, Some(Duration::from_secs(2)))
                .expect_err("read should time out")
                .code()
                .to_owned()
        }));
    }

    wait_for_waiting_readers(&manager, read_id, handles.len());
    for handle in handles {
        assert_eq!(
            handle.join().expect("reader thread should finish"),
            "EAGAIN"
        );
    }
    assert_eq!(manager.pending_read_waiter_count(), 0);
    assert_eq!(
        manager
            .waiting_reader_count(read_id)
            .expect("pipe should remain open"),
        0
    );
}

#[test]
fn writing_after_the_read_end_closes_returns_epipe() {
    let manager = PipeManager::new();
    let pipe = manager.create_pipe();

    manager.close(pipe.read.description.id());
    assert_pipe_error(manager.write(pipe.write.description.id(), b"fail"), "EPIPE");
}

#[test]
fn create_pipe_fds_allocates_pipe_entries_in_the_fd_table() {
    let manager = PipeManager::new();
    let mut tables = FdTableManager::new();
    tables.create(1);

    let (read_fd, write_fd) = manager
        .create_pipe_fds(tables.get_mut(1).expect("FD table should exist"))
        .expect("create pipe FDs");
    let table = tables.get(1).expect("FD table should exist");

    assert_eq!(read_fd, 3);
    assert_eq!(write_fd, 4);
    assert_eq!(
        table.get(read_fd).expect("read entry").filetype,
        FILETYPE_PIPE
    );
    assert_eq!(
        table.get(write_fd).expect("write entry").filetype,
        FILETYPE_PIPE
    );
}

#[test]
fn create_pipe_fds_propagates_fd_allocation_failures() {
    let manager = PipeManager::new();
    let mut tables = FdTableManager::with_max_fds(256);
    let table = tables.create(1);

    for index in 0..253 {
        table
            .open(&format!("/tmp/file-{index}"), 0)
            .expect("fill remaining FD slots");
    }

    assert_fd_error(manager.create_pipe_fds(table), "EMFILE");
}
