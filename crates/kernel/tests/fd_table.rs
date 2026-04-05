use agent_os_kernel::fd_table::{
    FdResult, FdTableManager, FileDescription, FileLockManager, FileLockTarget, FlockOperation,
    FILETYPE_CHARACTER_DEVICE, FILETYPE_REGULAR_FILE, LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN,
    MAX_FDS_PER_PROCESS, O_RDONLY, O_WRONLY,
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
