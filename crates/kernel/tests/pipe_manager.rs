use agent_os_kernel::fd_table::{FdResult, FdTableManager, FILETYPE_PIPE};
use agent_os_kernel::pipe_manager::{
    PipeManager, PipeResult, MAX_PIPE_BUFFER_BYTES, PIPE_BUF_BYTES,
};
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn assert_pipe_error<T: Debug>(result: PipeResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn assert_fd_error<T: Debug>(result: FdResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
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
    thread::sleep(Duration::from_millis(10));
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

    thread::sleep(Duration::from_millis(10));
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

    thread::sleep(Duration::from_millis(10));
    manager
        .write(write_id, vec![7; MAX_PIPE_BUFFER_BYTES + 1024])
        .expect("large direct write should bypass buffering");

    assert_eq!(
        handle.join().expect("reader thread should finish"),
        MAX_PIPE_BUFFER_BYTES + 1024
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
    let mut tables = FdTableManager::new();
    let table = tables.create(1);

    for index in 0..253 {
        table
            .open(&format!("/tmp/file-{index}"), 0)
            .expect("fill remaining FD slots");
    }

    assert_fd_error(manager.create_pipe_fds(table), "EMFILE");
}
