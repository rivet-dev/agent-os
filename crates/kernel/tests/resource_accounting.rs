use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::pty::LineDisciplineConfig;
use agent_os_kernel::resource_accounting::{measure_filesystem_usage, ResourceLimits};
use agent_os_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn resource_snapshot_counts_processes_fds_pipes_and_ptys() {
    let mut config = KernelVmConfig::new("vm-resources");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");
    let (read_fd, write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    let (master_fd, slave_fd, _) = kernel.open_pty("shell", process.pid()).expect("open pty");
    kernel
        .pty_set_discipline(
            "shell",
            process.pid(),
            master_fd,
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
            },
        )
        .expect("set raw pty");

    kernel
        .fd_write("shell", process.pid(), write_fd, b"pipe-data")
        .expect("write pipe");
    kernel
        .fd_write("shell", process.pid(), master_fd, b"term")
        .expect("write pty");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.running_processes, 1);
    assert_eq!(snapshot.fd_tables, 1);
    assert_eq!(snapshot.pipes, 1);
    assert_eq!(snapshot.ptys, 1);
    assert_eq!(snapshot.open_fds, 7);
    assert_eq!(snapshot.pipe_buffered_bytes, 9);
    assert_eq!(snapshot.pty_buffered_input_bytes, 4);
    assert_eq!(snapshot.pty_buffered_output_bytes, 0);

    let _ = kernel
        .fd_read("shell", process.pid(), read_fd, 16)
        .expect("drain pipe");
    let _ = kernel
        .fd_read("shell", process.pid(), slave_fd, 16)
        .expect("drain pty");
    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap process");
}

#[test]
fn resource_limits_reject_extra_processes_pipes_and_ptys() {
    let mut config = KernelVmConfig::new("vm-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
        max_open_fds: Some(6),
        max_pipes: Some(1),
        max_ptys: Some(1),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn initial process");

    let error = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect_err("second process should exceed process limit");
    assert_eq!(error.code(), "EAGAIN");

    kernel
        .open_pipe("shell", process.pid())
        .expect("first pipe should succeed");
    let error = kernel
        .open_pipe("shell", process.pid())
        .expect_err("second pipe should exceed pipe limit");
    assert_eq!(error.code(), "EAGAIN");

    let error = kernel
        .open_pty("shell", process.pid())
        .expect_err("global FD limit should prevent PTY allocation");
    assert_eq!(error.code(), "EAGAIN");

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap process");
}

#[test]
fn zombie_processes_count_against_process_limits_until_reaped() {
    let mut config = KernelVmConfig::new("vm-zombie-process-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn initial process");
    process.finish(0);

    let error = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect_err("zombie should still count against process limit");
    assert_eq!(error.code(), "EAGAIN");

    kernel.wait_and_reap(process.pid()).expect("reap zombie");
    kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn should succeed after zombie is reaped");
}

#[test]
fn filesystem_limits_reject_inode_growth_and_file_expansion() {
    let mut config = KernelVmConfig::new("vm-filesystem-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(5),
        max_inode_count: Some(4),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .write_file("/tmp/a.txt", b"hello".to_vec())
        .expect("seed file within byte limit");
    kernel
        .create_dir("/tmp/dir")
        .expect("create directory within inode limit");

    let write_error = kernel
        .write_file("/tmp/b.txt", b"!".to_vec())
        .expect_err("additional file should exceed inode limit");
    assert_eq!(write_error.code(), "ENOSPC");

    let truncate_error = kernel
        .truncate("/tmp/a.txt", 6)
        .expect_err("truncate should exceed filesystem byte limit");
    assert_eq!(truncate_error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/tmp/a.txt")
            .expect("file should stay unchanged"),
        b"hello".to_vec()
    );
}

#[test]
fn filesystem_limits_reject_fd_pwrite_before_resizing_file() {
    let mut config = KernelVmConfig::new("vm-fd-pwrite-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(16),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/data.txt", b"abc".to_vec())
        .expect("seed file");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");
    let fd = kernel
        .fd_open("shell", process.pid(), "/tmp/data.txt", 0, None)
        .expect("open file");

    let error = kernel
        .fd_pwrite("shell", process.pid(), fd, b"z", 16)
        .expect_err("pwrite should exceed filesystem byte limit");
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/tmp/data.txt")
            .expect("file should stay unchanged"),
        b"abc".to_vec()
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn concurrent_process_spawns_stop_at_max_processes_without_poisoning_kernel() {
    let mut config = KernelVmConfig::new("vm-process-flood");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(4),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let kernel = Arc::new(Mutex::new(kernel));
    let worker_count = 6;
    let attempt_count = 24;
    let barrier = Arc::new(Barrier::new(worker_count));
    let next_attempt = Arc::new(AtomicUsize::new(0));

    let results = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..worker_count {
            let kernel = Arc::clone(&kernel);
            let barrier = Arc::clone(&barrier);
            let next_attempt = Arc::clone(&next_attempt);
            workers.push(scope.spawn(move || {
                let mut successes = Vec::new();
                let mut errors = Vec::new();
                barrier.wait();
                loop {
                    let attempt = next_attempt.fetch_add(1, Ordering::SeqCst);
                    if attempt >= attempt_count {
                        break;
                    }

                    let result = kernel.lock().expect("lock kernel").spawn_process(
                        "sh",
                        Vec::new(),
                        SpawnOptions {
                            requester_driver: Some(String::from("shell")),
                            ..SpawnOptions::default()
                        },
                    );
                    match result {
                        Ok(process) => successes.push(process),
                        Err(error) => errors.push(error.code().to_owned()),
                    }
                }
                (successes, errors)
            }));
        }

        let mut successes = Vec::new();
        let mut errors = Vec::new();
        for worker in workers {
            let (mut worker_successes, mut worker_errors) = worker.join().expect("join worker");
            successes.append(&mut worker_successes);
            errors.append(&mut worker_errors);
        }
        (successes, errors)
    });

    let (processes, errors) = results;
    assert_eq!(
        processes.len(),
        4,
        "spawn flood should stop at process limit"
    );
    assert_eq!(errors.len(), attempt_count - processes.len());
    assert!(errors.iter().all(|code| code == "EAGAIN"));
    assert_eq!(
        kernel
            .lock()
            .expect("lock kernel")
            .resource_snapshot()
            .running_processes,
        4
    );

    {
        let mut kernel = kernel.lock().expect("lock kernel");
        for process in &processes {
            process.finish(0);
        }
        for process in &processes {
            kernel
                .wait_and_reap(process.pid())
                .expect("reap spawned process");
        }
        kernel
            .spawn_process(
                "sh",
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from("shell")),
                    ..SpawnOptions::default()
                },
            )
            .expect("kernel should still allow spawn after cleanup");
    }
}

#[test]
fn concurrent_fd_opens_stop_at_max_open_fds_and_recover_after_close() {
    let mut config = KernelVmConfig::new("vm-fd-flood");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_open_fds: Some(7),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/data.txt", b"seed".to_vec())
        .expect("seed file");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");
    let pid = process.pid();
    let expected_capacity = kernel
        .resource_limits()
        .max_open_fds
        .expect("fd limit")
        .saturating_sub(kernel.resource_snapshot().open_fds);

    let kernel = Arc::new(Mutex::new(kernel));
    let worker_count = 6;
    let attempt_count = 24;
    let barrier = Arc::new(Barrier::new(worker_count));
    let next_attempt = Arc::new(AtomicUsize::new(0));

    let results = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..worker_count {
            let kernel = Arc::clone(&kernel);
            let barrier = Arc::clone(&barrier);
            let next_attempt = Arc::clone(&next_attempt);
            workers.push(scope.spawn(move || {
                let mut opened = Vec::new();
                let mut errors = Vec::new();
                barrier.wait();
                loop {
                    let attempt = next_attempt.fetch_add(1, Ordering::SeqCst);
                    if attempt >= attempt_count {
                        break;
                    }

                    let result = kernel.lock().expect("lock kernel").fd_open(
                        "shell",
                        pid,
                        "/data.txt",
                        0,
                        None,
                    );
                    match result {
                        Ok(fd) => opened.push(fd),
                        Err(error) => errors.push(error.code().to_owned()),
                    }
                }
                (opened, errors)
            }));
        }

        let mut opened = Vec::new();
        let mut errors = Vec::new();
        for worker in workers {
            let (mut worker_opened, mut worker_errors) = worker.join().expect("join worker");
            opened.append(&mut worker_opened);
            errors.append(&mut worker_errors);
        }
        (opened, errors)
    });

    let (opened_fds, errors) = results;
    assert_eq!(opened_fds.len(), expected_capacity);
    assert_eq!(errors.len(), attempt_count - opened_fds.len());
    assert!(errors.iter().all(|code| code == "EMFILE"));

    let mut kernel = kernel.lock().expect("lock kernel");
    assert_eq!(
        kernel.resource_snapshot().open_fds,
        kernel.resource_limits().max_open_fds.expect("fd limit")
    );
    for fd in &opened_fds {
        kernel
            .fd_close("shell", pid, *fd)
            .expect("close flooded fd");
    }
    kernel
        .fd_open("shell", pid, "/data.txt", 0, None)
        .expect("fd slots should reopen after close");
    process.finish(0);
    kernel.wait_and_reap(pid).expect("reap shell");
}

#[test]
fn concurrent_inode_creation_stops_at_max_inode_count() {
    let base_usage = measure_filesystem_usage(&mut MemoryFileSystem::new()).expect("measure base");
    let allowed_new_files = 4;

    let mut config = KernelVmConfig::new("vm-inode-flood");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(base_usage.inode_count + allowed_new_files),
        ..ResourceLimits::default()
    };

    let kernel = Arc::new(Mutex::new(KernelVm::new(MemoryFileSystem::new(), config)));
    let worker_count = 6;
    let attempt_count = 24;
    let barrier = Arc::new(Barrier::new(worker_count));
    let next_attempt = Arc::new(AtomicUsize::new(0));

    let errors = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..worker_count {
            let kernel = Arc::clone(&kernel);
            let barrier = Arc::clone(&barrier);
            let next_attempt = Arc::clone(&next_attempt);
            workers.push(scope.spawn(move || {
                let mut successes = 0usize;
                let mut errors = Vec::new();
                barrier.wait();
                loop {
                    let attempt = next_attempt.fetch_add(1, Ordering::SeqCst);
                    if attempt >= attempt_count {
                        break;
                    }

                    let path = format!("/inode-flood-{attempt}.txt");
                    match kernel
                        .lock()
                        .expect("lock kernel")
                        .write_file(&path, b"x".to_vec())
                    {
                        Ok(()) => successes += 1,
                        Err(error) => errors.push(error.code().to_owned()),
                    }
                }
                (successes, errors)
            }));
        }

        let mut successes = 0usize;
        let mut errors = Vec::new();
        for worker in workers {
            let (worker_successes, mut worker_errors) = worker.join().expect("join worker");
            successes += worker_successes;
            errors.append(&mut worker_errors);
        }
        (successes, errors)
    });

    let (successes, errors) = errors;
    assert_eq!(successes, allowed_new_files);
    assert_eq!(errors.len(), attempt_count - successes);
    assert!(errors.iter().all(|code| code == "ENOSPC"));

    let usage =
        measure_filesystem_usage(&mut *kernel.lock().expect("lock kernel").filesystem_mut())
            .expect("measure flooded usage");
    assert_eq!(
        usage.inode_count,
        base_usage.inode_count + allowed_new_files
    );
}

#[test]
fn concurrent_file_writes_stop_at_max_filesystem_bytes() {
    let base_usage = measure_filesystem_usage(&mut MemoryFileSystem::new()).expect("measure base");
    let bytes_per_file = 4u64;
    let allowed_new_files = 3u64;

    let mut config = KernelVmConfig::new("vm-byte-flood");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(base_usage.total_bytes + (bytes_per_file * allowed_new_files)),
        ..ResourceLimits::default()
    };

    let kernel = Arc::new(Mutex::new(KernelVm::new(MemoryFileSystem::new(), config)));
    let worker_count = 6;
    let attempt_count = 18;
    let barrier = Arc::new(Barrier::new(worker_count));
    let next_attempt = Arc::new(AtomicUsize::new(0));

    let results = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..worker_count {
            let kernel = Arc::clone(&kernel);
            let barrier = Arc::clone(&barrier);
            let next_attempt = Arc::clone(&next_attempt);
            workers.push(scope.spawn(move || {
                let mut successes = 0usize;
                let mut errors = Vec::new();
                barrier.wait();
                loop {
                    let attempt = next_attempt.fetch_add(1, Ordering::SeqCst);
                    if attempt >= attempt_count {
                        break;
                    }

                    let path = format!("/byte-flood-{attempt}.txt");
                    match kernel
                        .lock()
                        .expect("lock kernel")
                        .write_file(&path, b"data".to_vec())
                    {
                        Ok(()) => successes += 1,
                        Err(error) => errors.push(error.code().to_owned()),
                    }
                }
                (successes, errors)
            }));
        }

        let mut successes = 0usize;
        let mut errors = Vec::new();
        for worker in workers {
            let (worker_successes, mut worker_errors) = worker.join().expect("join worker");
            successes += worker_successes;
            errors.append(&mut worker_errors);
        }
        (successes, errors)
    });

    let (successes, errors) = results;
    assert_eq!(successes as u64, allowed_new_files);
    assert_eq!(errors.len(), attempt_count - successes);
    assert!(errors.iter().all(|code| code == "ENOSPC"));

    let usage =
        measure_filesystem_usage(&mut *kernel.lock().expect("lock kernel").filesystem_mut())
            .expect("measure flooded usage");
    assert_eq!(
        usage.total_bytes,
        base_usage.total_bytes + (bytes_per_file * allowed_new_files)
    );
}

#[test]
fn blocking_pipe_and_pty_reads_time_out_instead_of_hanging_forever() {
    let mut config = KernelVmConfig::new("vm-read-timeouts");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_blocking_read_ms: Some(25),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");

    let (read_fd, _write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    let (master_fd, slave_fd, _) = kernel.open_pty("shell", process.pid()).expect("open pty");
    kernel
        .pty_set_discipline(
            "shell",
            process.pid(),
            master_fd,
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
            },
        )
        .expect("set raw pty");

    let started = Instant::now();
    let pipe_error = kernel
        .fd_read("shell", process.pid(), read_fd, 16)
        .expect_err("empty pipe read should time out");
    assert_eq!(pipe_error.code(), "EAGAIN");
    assert!(
        started.elapsed() >= Duration::from_millis(20),
        "pipe read timed out too early: {:?}",
        started.elapsed()
    );

    let started = Instant::now();
    let pty_error = kernel
        .fd_read("shell", process.pid(), slave_fd, 16)
        .expect_err("empty PTY read should time out");
    assert_eq!(pty_error.code(), "EAGAIN");
    assert!(
        started.elapsed() >= Duration::from_millis(20),
        "PTY read timed out too early: {:?}",
        started.elapsed()
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn resource_limits_reject_oversized_spawn_payloads() {
    let mut config = KernelVmConfig::new("vm-spawn-payload-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_process_argv_bytes: Some(13),
        max_process_env_bytes: Some(15),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let argv_error = kernel
        .spawn_process(
            "sh",
            vec![String::from("1234567890")],
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect_err("oversized argv should be rejected");
    assert_eq!(argv_error.code(), "EINVAL");

    let env_error = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                env: BTreeMap::from([(String::from("LONG"), String::from("1234567890"))]),
                ..SpawnOptions::default()
            },
        )
        .expect_err("oversized environment should be rejected");
    assert_eq!(env_error.code(), "EINVAL");
}

#[test]
fn resource_limits_reject_oversized_pread_and_write_operations() {
    let mut config = KernelVmConfig::new("vm-io-op-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_pread_bytes: Some(4),
        max_fd_write_bytes: Some(3),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/tmp/data.txt", b"hello".to_vec())
        .expect("seed file");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");
    let fd = kernel
        .fd_open("shell", process.pid(), "/tmp/data.txt", 0, None)
        .expect("open file");

    let pread_error = kernel
        .fd_pread("shell", process.pid(), fd, 5, 0)
        .expect_err("oversized pread should be rejected");
    assert_eq!(pread_error.code(), "EINVAL");

    let write_error = kernel
        .fd_write("shell", process.pid(), fd, b"four")
        .expect_err("oversized fd_write should be rejected");
    assert_eq!(write_error.code(), "EINVAL");

    let pwrite_error = kernel
        .fd_pwrite("shell", process.pid(), fd, b"four", 0)
        .expect_err("oversized fd_pwrite should be rejected");
    assert_eq!(pwrite_error.code(), "EINVAL");

    assert_eq!(
        kernel
            .read_file("/tmp/data.txt")
            .expect("file should remain unchanged"),
        b"hello".to_vec()
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn resource_limits_reject_oversized_readdir_batches() {
    let mut config = KernelVmConfig::new("vm-readdir-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_readdir_entries: Some(2),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel.create_dir("/tmp").expect("create tmp");
    kernel
        .write_file("/tmp/a.txt", b"a".to_vec())
        .expect("write first entry");
    kernel
        .write_file("/tmp/b.txt", b"b".to_vec())
        .expect("write second entry");
    kernel
        .write_file("/tmp/c.txt", b"c".to_vec())
        .expect("write third entry");

    let error = kernel
        .read_dir("/tmp")
        .expect_err("oversized readdir batch should be rejected");
    assert_eq!(error.code(), "ENOMEM");
}
