use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, VirtualProcessOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::user::UserConfig;
use agent_os_kernel::vfs::MemoryFileSystem;

fn configured_kernel() -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new("vm-identity");
    config.permissions = Permissions::allow_all();
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
    assert_eq!(
        kernel.getuid("identity-driver", pid).expect("getuid"),
        501
    );
    assert_eq!(
        kernel.getgid("identity-driver", pid).expect("getgid"),
        502
    );
    assert_eq!(
        kernel.geteuid("identity-driver", pid).expect("geteuid"),
        700
    );
    assert_eq!(
        kernel.getegid("identity-driver", pid).expect("getegid"),
        701
    );
    assert_eq!(
        kernel
            .getgroups("identity-driver", pid)
            .expect("getgroups"),
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
        kernel.getpwuid(501),
        "deploy:x:501:502:Deploy User:/srv/deploy:/bin/bash"
    );
    assert_eq!(kernel.getpwuid(77), "user77:x:77:77::/home/user77:/bin/sh");
    assert_eq!(kernel.getgrgid(502), "deployers:x:502:deploy");
    assert_eq!(kernel.getgrgid(77), "group77:x:77:");
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
