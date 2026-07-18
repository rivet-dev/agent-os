use agentos_kernel::fd_table::{
    FdResult, FdTableManager, FileDescription, FileLockManager, FileLockTarget, FlockOperation,
    RecordLock, RecordLockType, FD_CLOEXEC, FILETYPE_CHARACTER_DEVICE, FILETYPE_REGULAR_FILE,
    F_DUPFD, F_GETFD, F_GETFL, F_SETFD, F_SETFL, LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN,
    MAX_FDS_PER_PROCESS, O_APPEND, O_NONBLOCK, O_RDONLY, O_WRONLY,
};
use std::fmt::Debug;
use std::sync::Arc;

fn assert_error_code<T: Debug>(result: FdResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

#[test]
fn posix_record_locks_conflict_split_and_release_by_process() {
    let locks = FileLockManager::new();
    let target = FileLockTarget::new(7, 42);
    locks
        .set_record_lock(
            target,
            RecordLock::new(RecordLockType::Write, 10, 20, 10).expect("write lock range"),
        )
        .expect("owner write lock");

    let conflict = locks
        .query_record_lock(
            target,
            RecordLock::new(RecordLockType::Read, 15, 5, 20).expect("read lock range"),
        )
        .expect("conflicting lock");
    assert_eq!(conflict.pid, 10);
    assert_eq!((conflict.start, conflict.end), (10, Some(30)));
    assert_error_code(
        locks.set_record_lock(
            target,
            RecordLock::new(RecordLockType::Read, 15, 5, 20).expect("read lock range"),
        ),
        "EWOULDBLOCK",
    );

    locks
        .set_record_lock(
            target,
            RecordLock::new(RecordLockType::Unlock, 15, 5, 10).expect("unlock range"),
        )
        .expect("split owner range");
    locks
        .set_record_lock(
            target,
            RecordLock::new(RecordLockType::Read, 15, 5, 20).expect("read lock range"),
        )
        .expect("acquire split gap");
    assert_error_code(
        locks.set_record_lock(
            target,
            RecordLock::new(RecordLockType::Write, 25, 1, 20).expect("write lock range"),
        ),
        "EWOULDBLOCK",
    );

    locks.release_process_target(10, target);
    locks
        .set_record_lock(
            target,
            RecordLock::new(RecordLockType::Write, 25, 1, 20).expect("write lock range"),
        )
        .expect("owner close releases remaining ranges");
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
fn open_with_replaces_target_fd_and_releases_previous_entry() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let target_fd = table
        .open("/tmp/old.txt", O_RDONLY)
        .expect("open target FD");
    let previous = Arc::clone(&table.get(target_fd).expect("target entry").description);
    let replacement = Arc::new(FileDescription::new(999, "/tmp/new.txt", O_RDONLY));

    assert_eq!(previous.ref_count(), 1);

    let opened = table
        .open_with(
            Arc::clone(&replacement),
            FILETYPE_REGULAR_FILE,
            Some(target_fd),
        )
        .expect("replace target FD");

    assert_eq!(opened, target_fd);
    assert_eq!(previous.ref_count(), 0);
    assert_eq!(replacement.ref_count(), 2);
    assert!(Arc::ptr_eq(
        &table.get(target_fd).expect("replacement entry").description,
        &replacement
    ));
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
fn nonblocking_status_override_creates_independent_reopen_state() {
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
fn duplicated_descriptors_share_nonblocking_open_status() {
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
    let duplicate = table.dup(fd).expect("duplicate regular file");

    table
        .fcntl(duplicate, F_SETFL, 0)
        .expect("clear shared nonblocking flag");

    assert_eq!(table.fcntl(fd, F_GETFL, 0).unwrap(), O_WRONLY);
    assert_eq!(table.fcntl(duplicate, F_GETFL, 0).unwrap(), O_WRONLY);
}

#[test]
fn shared_description_open_preserves_nonblocking_status() {
    let mut manager = FdTableManager::new();
    manager.create(1);

    let description =
        std::sync::Arc::new(agentos_kernel::fd_table::FileDescription::with_ref_count(
            41,
            "pipe:41:read",
            O_RDONLY | O_NONBLOCK,
            0,
        ));
    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open_with(description, agentos_kernel::fd_table::FILETYPE_PIPE, None)
        .expect("open shared pipe description");

    assert_eq!(
        table.get(fd).expect("opened entry").status_flags.get(),
        O_NONBLOCK
    );
    assert_eq!(
        table.fcntl(fd, F_GETFL, 0).expect("read open status"),
        O_RDONLY | O_NONBLOCK
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
fn fcntl_dupfd_supports_high_minimum_and_skips_alias_collisions() {
    let mut manager = FdTableManager::with_max_fds(1024);
    manager.create(1);

    let table = manager.get_mut(1).expect("FD table should exist");
    let fd = table
        .open("/tmp/test.txt", O_RDONLY)
        .expect("open source FD");
    table.dup2(fd, 512).expect("occupy first high alias");
    table.dup2(fd, 513).expect("occupy second high alias");

    let duplicated = table
        .fcntl(fd, F_DUPFD, 512)
        .expect("duplicate above configured high minimum");
    assert_eq!(duplicated, 514);
    assert_eq!(
        table.stat(duplicated).expect("duplicate stat").rights,
        table.stat(fd).expect("source stat").rights
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
    let target = FileLockTarget::new(1, 42);

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
    let target = FileLockTarget::new(1, 7);

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

#[test]
fn lock_manager_distinguishes_equal_inodes_on_different_devices() {
    let locks = FileLockManager::new();
    let first = FileLockTarget::new(1, 42);
    let second = FileLockTarget::new(2, 42);

    locks
        .apply(1, first, FlockOperation::Exclusive { nonblocking: true })
        .expect("lock inode on first device");
    locks
        .apply(2, second, FlockOperation::Exclusive { nonblocking: true })
        .expect("same inode number on another device must not conflict");

    locks
        .set_record_lock(
            first,
            RecordLock::new(RecordLockType::Write, 0, 0, 10).expect("first record lock"),
        )
        .expect("record lock inode on first device");
    locks
        .set_record_lock(
            second,
            RecordLock::new(RecordLockType::Write, 0, 0, 20).expect("second record lock"),
        )
        .expect("record lock on equal inode from another device must not conflict");
}

#[test]
fn record_lock_wait_graph_detects_cycles_and_clears_cancelled_waits() {
    let locks = FileLockManager::new();
    let first = FileLockTarget::new(1, 100);
    let second = FileLockTarget::new(1, 200);
    let write = |target, pid| {
        RecordLock::new(RecordLockType::Write, 0, 8, pid)
            .map(|request| (target, request))
            .expect("valid record lock")
    };

    let (target, request) = write(first, 10);
    locks
        .set_record_lock(target, request)
        .expect("first process locks first file");
    let (target, request) = write(second, 20);
    locks
        .set_record_lock(target, request)
        .expect("second process locks second file");

    let (target, request) = write(second, 10);
    assert_error_code(
        locks.set_blocking_record_lock(target, request),
        "EWOULDBLOCK",
    );
    let (target, request) = write(first, 20);
    assert_error_code(locks.set_blocking_record_lock(target, request), "EDEADLK");

    assert!(locks.cancel_record_lock_wait(10));
    let (target, request) = write(first, 20);
    assert_error_code(
        locks.set_blocking_record_lock(target, request),
        "EWOULDBLOCK",
    );
}

#[test]
fn record_lock_wait_graph_refreshes_after_unlock_and_success() {
    let locks = FileLockManager::new();
    let first = FileLockTarget::new(1, 300);
    let second = FileLockTarget::new(1, 400);
    let request =
        |lock_type, pid| RecordLock::new(lock_type, 0, 8, pid).expect("valid record lock");

    locks
        .set_record_lock(first, request(RecordLockType::Write, 10))
        .expect("first process locks first file");
    locks
        .set_record_lock(second, request(RecordLockType::Write, 20))
        .expect("second process locks second file");
    assert_error_code(
        locks.set_blocking_record_lock(second, request(RecordLockType::Write, 10)),
        "EWOULDBLOCK",
    );

    locks
        .set_record_lock(second, request(RecordLockType::Unlock, 20))
        .expect("second process unlocks second file");
    locks
        .set_blocking_record_lock(second, request(RecordLockType::Write, 10))
        .expect("first process acquires released lock");

    assert_error_code(
        locks.set_blocking_record_lock(first, request(RecordLockType::Write, 20)),
        "EWOULDBLOCK",
    );
    assert!(locks.release_process_target(20, first));
    assert!(!locks.cancel_record_lock_wait(20));
}

#[test]
fn record_lock_wait_graph_enforces_its_bounded_capacity() {
    let locks = FileLockManager::with_record_lock_limit(1);
    let target = FileLockTarget::new(1, 500);
    let request =
        |pid| RecordLock::new(RecordLockType::Write, 0, 8, pid).expect("valid record lock");

    locks
        .set_record_lock(target, request(10))
        .expect("owner acquires lock");
    assert_error_code(
        locks.set_blocking_record_lock(target, request(20)),
        "EWOULDBLOCK",
    );
    assert_error_code(
        locks.set_blocking_record_lock(target, request(30)),
        "ENOLCK",
    );
}
