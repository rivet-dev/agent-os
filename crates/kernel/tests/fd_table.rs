use agent_os_kernel::fd_table::{
    FdResult, FdTableManager, FileDescription, FileLockManager, FileLockTarget, FlockOperation,
    FD_CLOEXEC, FILETYPE_CHARACTER_DEVICE, FILETYPE_REGULAR_FILE, F_DUPFD, F_GETFD, F_GETFL,
    F_SETFD, F_SETFL, LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN, MAX_FDS_PER_PROCESS, O_APPEND,
    O_NONBLOCK, O_RDONLY, O_WRONLY,
};
use std::fmt::Debug;
use std::sync::Arc;

fn assert_error_code<T: Debug>(result: FdResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

#[test]
fn preallocates_stdio_fds_0_1_2() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get(1).expect("FD table should exist");
    let stdin = table.get(0).expect("stdin entry");
    let stdout = table.get(1).expect("stdout entry");
    let stderr = table.get(2).expect("stderr entry");

    assert_eq!(stdin.filetype, FILETYPE_CHARACTER_DEVICE);
    assert_eq!(stdout.filetype, FILETYPE_CHARACTER_DEVICE);
    assert_eq!(stderr.filetype, FILETYPE_CHARACTER_DEVICE);

    assert_eq!(stdin.description.flags(), O_RDONLY);
    assert_eq!(stdout.description.flags(), O_WRONLY);
    assert_eq!(stderr.description.flags(), O_WRONLY);
}

#[test]
fn opens_new_fds_starting_at_three() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let fd = manager
        .get_mut(1)
        .expect("FD table should exist")
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open regular file");

    assert_eq!(fd, 3);
}

#[test]
fn dup_shares_the_same_file_description() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");
    let dup_fd = table.dup(fd).expect("duplicate FD");

    let original = Arc::clone(&table.get(fd).expect("source entry").description);
    let duplicated = Arc::clone(&table.get(dup_fd).expect("dup entry").description);

    assert_ne!(dup_fd, fd);
    assert!(Arc::ptr_eq(&original, &duplicated));
}

#[test]
fn dup2_replaces_the_target_fd() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");
    table.dup2(fd, 10).expect("dup2 into target FD");

    let source = Arc::clone(&table.get(fd).expect("source entry").description);
    let target = Arc::clone(&table.get(10).expect("target entry").description);

    assert!(Arc::ptr_eq(&source, &target));
}

#[test]
fn dup2_rejects_target_fds_past_the_process_limit() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");
    let result = table.dup2(fd, MAX_FDS_PER_PROCESS as u32);

    assert_error_code(result, "EBADF");
}

#[test]
fn open_with_rejects_target_fds_past_the_process_limit() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let description = Arc::new(FileDescription::new(999, "/tmp/test.txt", O_RDONLY));
    let result = table.open_with(
        description,
        FILETYPE_REGULAR_FILE,
        Some(MAX_FDS_PER_PROCESS as u32),
    );

    assert_error_code(result, "EBADF");
}

#[test]
fn configurable_process_fd_limit_returns_emfile() {
    let mut manager = FdTableManager::with_max_fds(5);
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    table
        .open("/tmp/test-1.txt", O_RDONLY)
        .expect("first non-stdio FD should open");
    table
        .open("/tmp/test-2.txt", O_RDONLY)
        .expect("second non-stdio FD should open");

    let result = table.open("/tmp/test-3.txt", O_RDONLY);
    assert_error_code(result, "EMFILE");
}

#[test]
fn close_decrements_refcount() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");
    let dup_fd = table.dup(fd).expect("duplicate FD");
    let description = Arc::clone(&table.get(fd).expect("source entry").description);

    assert_eq!(description.ref_count(), 2);
    assert!(table.close(dup_fd));
    assert_eq!(description.ref_count(), 1);
}

#[test]
fn fork_creates_an_independent_table_with_shared_descriptions() {
    let mut manager = FdTableManager::new();
    manager.create(1);
    let fd = manager
        .get_mut(1)
        .expect("parent table should exist")
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");

    manager.fork(1, 2);

    let parent_description = Arc::clone(
        &manager
            .get(1)
            .expect("parent table should exist")
            .get(fd)
            .expect("parent FD entry")
            .description,
    );
    let child_description = {
        let child = manager.get_mut(2).expect("child table should exist");
        let description = Arc::clone(&child.get(fd).expect("child FD entry").description);
        assert!(child.close(fd));
        description
    };

    assert!(Arc::ptr_eq(&parent_description, &child_description));
    assert!(manager
        .get(1)
        .expect("parent table should still exist")
        .get(fd)
        .is_some());
}

#[test]
fn stat_returns_fd_metadata() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let fd = manager
        .get_mut(1)
        .expect("FD table should exist")
        .open_with_filetype("/tmp/test.txt", O_WRONLY, FILETYPE_REGULAR_FILE)
        .expect("open regular file");
    let stat = manager
        .get(1)
        .expect("FD table should exist")
        .stat(fd)
        .expect("stat FD");

    assert_eq!(stat.filetype, FILETYPE_REGULAR_FILE);
    assert_eq!(stat.flags, O_WRONLY);
}

#[test]
fn nonblocking_status_flags_are_tracked_per_fd_entry() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open_with_filetype(
            "/tmp/test.txt",
            O_WRONLY | O_NONBLOCK,
            FILETYPE_REGULAR_FILE,
        )
        .expect("open regular file");
    let dup_fd = table
        .dup_with_status_flags(fd, Some(0))
        .expect("duplicate regular file without nonblocking");

    let original = table.stat(fd).expect("stat original FD");
    let duplicated = table.stat(dup_fd).expect("stat duplicate FD");

    assert_eq!(original.flags, O_WRONLY | O_NONBLOCK);
    assert_eq!(duplicated.flags, O_WRONLY);
    assert_eq!(
        table.get(fd).expect("original entry").description.flags(),
        O_WRONLY
    );
    assert_eq!(
        table
            .get(dup_fd)
            .expect("duplicate entry")
            .description
            .flags(),
        O_WRONLY
    );
}

#[test]
fn fcntl_getfl_and_setfl_report_visible_status_flags() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open_with_filetype("/tmp/test.txt", O_WRONLY | O_APPEND, FILETYPE_REGULAR_FILE)
        .expect("open append file");

    assert_eq!(
        table.fcntl(fd, F_GETFL, 0).expect("initial F_GETFL"),
        O_WRONLY | O_APPEND
    );

    table
        .fcntl(fd, F_SETFL, O_APPEND | O_NONBLOCK)
        .expect("set append and nonblocking");

    assert_eq!(
        table.fcntl(fd, F_GETFL, 0).expect("updated F_GETFL"),
        O_WRONLY | O_APPEND | O_NONBLOCK
    );
    assert_eq!(
        table.stat(fd).expect("stat after F_SETFL").flags,
        O_WRONLY | O_APPEND | O_NONBLOCK
    );
}

#[test]
fn fcntl_fd_flags_are_per_descriptor() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");
    let dup_fd = table.dup(fd).expect("duplicate FD");

    table
        .fcntl(fd, F_SETFD, FD_CLOEXEC)
        .expect("set cloexec on source");

    assert_eq!(
        table.fcntl(fd, F_GETFD, 0).expect("read source FD flags"),
        FD_CLOEXEC
    );
    assert_eq!(
        table
            .fcntl(dup_fd, F_GETFD, 0)
            .expect("read duplicate FD flags"),
        0
    );
}

#[test]
fn fcntl_dupfd_uses_lowest_available_fd_at_or_above_minimum() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");
    let filler_a = table.open("/tmp/a.txt", O_RDONLY).expect("open filler a");
    let filler_b = table.open("/tmp/b.txt", O_RDONLY).expect("open filler b");
    let filler_c = table.open("/tmp/c.txt", O_RDONLY).expect("open filler c");
    assert_eq!((fd, filler_a, filler_b, filler_c), (3, 4, 5, 6));

    assert!(table.close(5), "fd 5 should be available for F_DUPFD reuse");

    let duplicated = table
        .fcntl(fd, F_DUPFD, 5)
        .expect("duplicate into lowest fd >= 5");

    assert_eq!(duplicated, 5);
    assert_eq!(
        table
            .fcntl(duplicated, F_GETFD, 0)
            .expect("new duplicate should clear FD flags"),
        0
    );
}

#[test]
fn fcntl_dupfd_rejects_minimum_fd_past_the_process_limit() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");

    assert_error_code(
        table.fcntl(fd, F_DUPFD, MAX_FDS_PER_PROCESS as u32),
        "EINVAL",
    );
}

#[test]
fn stat_reports_ebadf_for_invalid_fd() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let result = manager.get(1).expect("FD table should exist").stat(999);

    assert_error_code(result, "EBADF");
}

#[test]
fn open_reuses_a_freed_fd_after_next_fd_moves_past_the_limit() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let mut opened = Vec::new();
    for _ in 3..MAX_FDS_PER_PROCESS {
        opened.push(
            table
                .open("/tmp/test.txt", O_RDONLY)
                .expect("open should fill remaining slots"),
        );
    }

    assert!(table.close(5), "fd 5 should be open before reuse");

    let reused = table
        .open("/tmp/reused.txt", O_RDONLY)
        .expect("open should wrap and reuse a freed fd");
    assert_eq!(reused, 5);
}

#[test]
fn flock_operation_parser_accepts_supported_modes() {
    assert_eq!(
        FlockOperation::from_bits(LOCK_SH).expect("shared operation"),
        FlockOperation::Shared { nonblocking: false }
    );
    assert_eq!(
        FlockOperation::from_bits(LOCK_EX | LOCK_NB).expect("exclusive nonblocking operation"),
        FlockOperation::Exclusive { nonblocking: true }
    );
    assert_eq!(
        FlockOperation::from_bits(LOCK_UN).expect("unlock operation"),
        FlockOperation::Unlock
    );
}

#[test]
fn flock_manager_enforces_shared_and_exclusive_conflicts() {
    let locks = FileLockManager::new();
    let target = FileLockTarget::new(42);

    locks
        .apply(1, target, FlockOperation::Shared { nonblocking: false })
        .expect("first shared lock");
    locks
        .apply(2, target, FlockOperation::Shared { nonblocking: false })
        .expect("second shared lock");

    let blocked = locks.apply(3, target, FlockOperation::Exclusive { nonblocking: true });
    assert_error_code(blocked, "EWOULDBLOCK");

    locks
        .apply(1, target, FlockOperation::Unlock)
        .expect("unlock first shared lock");
    locks
        .apply(2, target, FlockOperation::Unlock)
        .expect("unlock second shared lock");
    locks
        .apply(3, target, FlockOperation::Exclusive { nonblocking: true })
        .expect("exclusive lock becomes available");
}

#[test]
fn flock_manager_treats_reacquire_on_same_description_as_non_conflicting() {
    let locks = FileLockManager::new();
    let target = FileLockTarget::new(7);

    locks
        .apply(99, target, FlockOperation::Exclusive { nonblocking: false })
        .expect("initial exclusive lock");
    locks
        .apply(99, target, FlockOperation::Exclusive { nonblocking: true })
        .expect("same description can reacquire exclusive lock");
    locks
        .apply(99, target, FlockOperation::Shared { nonblocking: true })
        .expect("same description can downgrade to shared lock");

    let shared = locks.apply(100, target, FlockOperation::Shared { nonblocking: true });
    shared.expect("downgrade should allow other shared holders");
}
