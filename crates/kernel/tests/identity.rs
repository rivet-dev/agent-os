use agentos_kernel::kernel::{KernelVm, KernelVmConfig, VirtualProcessOptions};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::resource_accounting::ResourceLimits;
use agentos_kernel::user::{GroupRecord, UserAccount, UserConfig};
use agentos_kernel::vfs::MemoryFileSystem;
use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

fn configured_kernel() -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new("vm-identity");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_wasm_memory_bytes: Some(256 * 1024 * 1024),
        ..ResourceLimits::default()
    };
    config.user = UserConfig {
        uid: Some(501),
        gid: Some(502),
        euid: Some(700),
        egid: Some(701),
        username: Some(String::from("deploy")),
        homedir: Some(String::from("/srv/deploy")),
        shell: Some(String::from("/bin/bash")),
        gecos: Some(String::from("Deploy User")),
        group_name: Some(String::from("deployers")),
        supplementary_gids: vec![44, 502, 900],
        accounts: Vec::new(),
        groups: Vec::new(),
    };
    KernelVm::new(MemoryFileSystem::new(), config)
}

fn multi_user_root_kernel() -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new("vm-multi-user");
    config.permissions = Permissions::allow_all();
    config.user = UserConfig {
        uid: Some(0),
        gid: Some(0),
        username: Some(String::from("root")),
        homedir: Some(String::from("/root")),
        shell: Some(String::from("/bin/sh")),
        group_name: Some(String::from("root")),
        supplementary_gids: vec![0],
        accounts: vec![UserAccount {
            uid: 1000,
            gid: 1000,
            username: String::from("fsgqa"),
            homedir: String::from("/home/fsgqa"),
            shell: String::from("/bin/sh"),
            gecos: String::new(),
            supplementary_gids: vec![1000, 2000],
        }],
        groups: vec![GroupRecord {
            gid: 2000,
            name: String::from("testers"),
            members: vec![String::from("fsgqa")],
        }],
        ..UserConfig::default()
    };
    KernelVm::new(MemoryFileSystem::new(), config)
}

fn read_utf8(kernel: &mut KernelVm<MemoryFileSystem>, path: &str) -> String {
    String::from_utf8(kernel.read_file(path).expect("read proc file")).expect("utf8 proc file")
}

fn read_utf8_for_process(
    kernel: &mut KernelVm<MemoryFileSystem>,
    requester_driver: &str,
    pid: u32,
    path: &str,
) -> String {
    String::from_utf8(
        kernel
            .read_file_for_process(requester_driver, pid, path)
            .expect("read proc file for process"),
    )
    .expect("utf8 proc file")
}

fn parse_status_fields(body: &str) -> BTreeMap<&str, &str> {
    body.lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key, value.trim()))
        .collect()
}

#[test]
fn identity_syscalls_and_process_metadata_use_kernel_managed_values() {
    let mut kernel = configured_kernel();

    let process = kernel
        .create_virtual_process(
            "identity-driver",
            "identity-driver",
            "identity-check",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create identity process");
    let pid = process.pid();

    assert_eq!(
        kernel
            .process_identity("identity-driver", pid)
            .expect("read process identity")
            .supplementary_gids,
        vec![502, 44, 900]
    );
    assert_eq!(kernel.getuid("identity-driver", pid).expect("getuid"), 501);
    assert_eq!(kernel.getgid("identity-driver", pid).expect("getgid"), 502);
    assert_eq!(
        kernel.geteuid("identity-driver", pid).expect("geteuid"),
        700
    );
    assert_eq!(
        kernel.getegid("identity-driver", pid).expect("getegid"),
        701
    );
    assert_eq!(
        kernel.getgroups("identity-driver", pid).expect("getgroups"),
        vec![502, 44, 900]
    );

    let process_info = kernel
        .list_processes()
        .get(&pid)
        .expect("process info")
        .clone();
    assert_eq!(process_info.identity.uid, 501);
    assert_eq!(process_info.identity.gid, 502);
    assert_eq!(process_info.identity.euid, 700);
    assert_eq!(process_info.identity.egid, 701);
    assert_eq!(process_info.identity.supplementary_gids, vec![502, 44, 900]);

    assert_eq!(
        kernel.getpwuid(501).expect("primary uid lookup"),
        "deploy:x:501:502:Deploy User:/srv/deploy:/bin/bash"
    );
    let unknown_uid = kernel.getpwuid(77).expect_err("unknown uid should fail");
    assert_eq!(unknown_uid.code(), "ENOENT");
    assert_eq!(
        kernel.getgrgid(502).expect("primary gid lookup"),
        "deployers:x:502:deploy"
    );
    assert_eq!(
        kernel.getgrgid(44).expect("supplementary gid lookup"),
        "group44:x:44:deploy"
    );
    let unknown_gid = kernel.getgrgid(77).expect_err("unknown gid should fail");
    assert_eq!(unknown_gid.code(), "ENOENT");
}

#[test]
fn identity_queries_require_process_ownership() {
    let mut kernel = configured_kernel();
    let process = kernel
        .create_virtual_process(
            "identity-driver",
            "identity-driver",
            "identity-check",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create identity process");

    let error = kernel
        .getuid("other-driver", process.pid())
        .expect_err("foreign driver should be rejected");
    assert_eq!(error.code(), "EPERM");
}

#[test]
fn root_can_drop_and_restore_effective_ids_through_saved_ids() {
    let mut kernel = multi_user_root_kernel();
    let process = kernel
        .create_virtual_process(
            "identity-driver",
            "identity-driver",
            "identity-check",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create root process");
    let pid = process.pid();

    kernel
        .setresuid("identity-driver", pid, None, Some(1000), None)
        .expect("drop effective uid");
    assert_eq!(
        kernel.getresuid("identity-driver", pid).expect("getresuid"),
        (0, 1000, 0)
    );
    kernel
        .seteuid("identity-driver", pid, 0)
        .expect("restore saved root euid");
    assert_eq!(kernel.geteuid("identity-driver", pid).unwrap(), 0);

    kernel
        .setuid("identity-driver", pid, 1000)
        .expect("permanently drop uid");
    let error = kernel
        .seteuid("identity-driver", pid, 0)
        .expect_err("unprivileged process must not regain root");
    assert_eq!(error.code(), "EPERM");
}

#[test]
fn switch_user_sets_all_credentials_groups_and_fork_inherits_them() {
    let mut kernel = multi_user_root_kernel();
    let parent = kernel
        .create_virtual_process(
            "identity-driver",
            "identity-driver",
            "parent",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create root parent");
    kernel
        .switch_user("identity-driver", parent.pid(), 1000)
        .expect("switch to fsgqa");
    let identity = kernel
        .process_identity("identity-driver", parent.pid())
        .expect("parent identity");
    assert_eq!(
        (identity.uid, identity.euid, identity.suid),
        (1000, 1000, 1000)
    );
    assert_eq!(
        (identity.gid, identity.egid, identity.sgid),
        (1000, 1000, 1000)
    );
    assert_eq!(identity.supplementary_gids, vec![1000, 2000]);

    let child = kernel
        .create_virtual_process(
            "identity-driver",
            "identity-driver",
            "child",
            Vec::new(),
            VirtualProcessOptions {
                parent_pid: Some(parent.pid()),
                ..VirtualProcessOptions::default()
            },
        )
        .expect("fork child");
    assert_eq!(
        kernel
            .process_identity("identity-driver", child.pid())
            .expect("child identity"),
        identity
    );
    assert_eq!(
        kernel.getpwnam("fsgqa").expect("lookup fsgqa"),
        "fsgqa:x:1000:1000::/home/fsgqa:/bin/sh"
    );
    assert_eq!(
        kernel.getgrnam("testers").expect("lookup testers"),
        "testers:x:2000:fsgqa"
    );
}

#[test]
fn only_effective_root_can_replace_supplementary_groups() {
    let mut kernel = multi_user_root_kernel();
    let process = kernel
        .create_virtual_process(
            "identity-driver",
            "identity-driver",
            "identity-check",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create root process");
    let pid = process.pid();
    kernel
        .setgroups("identity-driver", pid, vec![0, 44, 44])
        .expect("root setgroups");
    assert_eq!(
        kernel.getgroups("identity-driver", pid).unwrap(),
        vec![0, 44]
    );
    kernel
        .seteuid("identity-driver", pid, 1000)
        .expect("drop effective uid");
    let error = kernel
        .setgroups("identity-driver", pid, vec![1000])
        .expect_err("non-root setgroups must fail");
    assert_eq!(error.code(), "EPERM");
}

#[test]
fn procfs_exposes_linux_like_identity_and_system_files() {
    let mut kernel = configured_kernel();
    let process = kernel
        .create_virtual_process(
            "identity-driver",
            "identity-driver",
            "identity-check",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create identity process");
    let pid = process.pid();

    let proc_entries = kernel.read_dir("/proc").expect("read /proc");
    assert!(proc_entries.contains(&String::from("cpuinfo")));
    assert!(proc_entries.contains(&String::from("loadavg")));
    assert!(proc_entries.contains(&String::from("meminfo")));
    assert!(proc_entries.contains(&String::from("mounts")));
    assert!(proc_entries.contains(&String::from("self")));
    assert!(proc_entries.contains(&String::from("uptime")));
    assert!(proc_entries.contains(&String::from("version")));
    assert!(proc_entries.contains(&pid.to_string()));

    let pid_entries = kernel
        .read_dir(&format!("/proc/{pid}"))
        .expect("read /proc/<pid>");
    assert!(pid_entries.contains(&String::from("status")));

    let status = read_utf8(&mut kernel, &format!("/proc/{pid}/status"));
    let self_status =
        read_utf8_for_process(&mut kernel, "identity-driver", pid, "/proc/self/status");
    assert_eq!(status, self_status);

    let status_fields = parse_status_fields(&status);
    assert_eq!(status_fields["Name"], "identity-check");
    assert_eq!(status_fields["State"], "R (running)");
    assert_eq!(status_fields["Pid"], pid.to_string());
    assert_eq!(status_fields["PPid"], "0");
    assert_eq!(status_fields["Uid"], "501\t700\t700\t700");
    assert_eq!(status_fields["Gid"], "502\t701\t701\t701");
    assert_eq!(status_fields["VmSize"], "0 kB");
    assert_eq!(status_fields["VmRSS"], "0 kB");
    assert_eq!(status_fields["Threads"], "1");

    let cpuinfo = read_utf8(&mut kernel, "/proc/cpuinfo");
    assert!(cpuinfo.contains("processor\t: 0"));
    assert!(cpuinfo.contains("model name\t: agentos Virtual CPU"));

    let meminfo = read_utf8(&mut kernel, "/proc/meminfo");
    assert!(meminfo.contains("MemTotal:  262144 kB"));
    assert!(meminfo.contains("MemFree:   262144 kB"));
    assert!(meminfo.contains("MemAvailable:262144 kB"));

    let loadavg = read_utf8(&mut kernel, "/proc/loadavg");
    assert!(loadavg.starts_with("0.00 0.00 0.00 1/1 "));
    assert!(loadavg.ends_with('\n'));

    thread::sleep(Duration::from_millis(20));
    let uptime = read_utf8(&mut kernel, "/proc/uptime");
    let uptime_parts = uptime.split_whitespace().collect::<Vec<_>>();
    assert_eq!(uptime_parts.len(), 2);
    let uptime_seconds = uptime_parts[0].parse::<f64>().expect("uptime seconds");
    let idle_seconds = uptime_parts[1].parse::<f64>().expect("idle seconds");
    assert!(uptime_seconds > 0.0);
    assert!(idle_seconds >= uptime_seconds);

    let version = read_utf8(&mut kernel, "/proc/version");
    assert!(version.starts_with("Linux version 6.8.0-agentos"));

    let status_stat = kernel
        .stat(&format!("/proc/{pid}/status"))
        .expect("stat proc status");
    assert_eq!(status_stat.size, status.len() as u64);
}
