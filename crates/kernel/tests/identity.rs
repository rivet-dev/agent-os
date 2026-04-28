use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, VirtualProcessOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::resource_accounting::ResourceLimits;
use agent_os_kernel::user::UserConfig;
use agent_os_kernel::vfs::MemoryFileSystem;
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
    assert!(cpuinfo.contains("model name\t: agentOS Virtual CPU"));

    let meminfo = read_utf8(&mut kernel, "/proc/meminfo");
    assert!(meminfo.contains("MemTotal:  262144 kB"));
    assert!(meminfo.contains("MemFree:   262144 kB"));
    assert!(meminfo.contains("MemAvailable:262144 kB"));

    let loadavg = read_utf8(&mut kernel, "/proc/loadavg");
    assert!(loadavg.starts_with("0.00 0.00 0.00 1/1 "));
    assert!(loadavg.ends_with('\n'));

    thread::sleep(Duration::from_millis(20));
    let uptime = read_utf8(&mut kernel, "/proc/uptime");
    let uptime_parts = uptime.trim().split_whitespace().collect::<Vec<_>>();
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
