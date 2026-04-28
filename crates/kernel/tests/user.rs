use agent_os_kernel::user::{UserConfig, UserManager};

#[test]
fn uses_sensible_defaults_when_not_configured() {
    let user = UserManager::new();

    assert_eq!(user.uid, 1000);
    assert_eq!(user.gid, 1000);
    assert_eq!(user.euid, 1000);
    assert_eq!(user.egid, 1000);
    assert_eq!(user.username, "user");
    assert_eq!(user.homedir, "/home/user");
    assert_eq!(user.shell, "/bin/sh");
    assert_eq!(user.gecos, "");
}

#[test]
fn empty_config_uses_the_same_defaults() {
    let user = UserManager::from_config(UserConfig::default());

    assert_eq!(user.uid, 1000);
    assert_eq!(user.gid, 1000);
    assert_eq!(user.username, "user");
}

#[test]
fn effective_ids_default_to_real_ids() {
    let with_uid = UserManager::from_config(UserConfig {
        uid: Some(500),
        ..UserConfig::default()
    });
    let with_gid = UserManager::from_config(UserConfig {
        gid: Some(500),
        ..UserConfig::default()
    });

    assert_eq!(with_uid.euid, 500);
    assert_eq!(with_gid.egid, 500);
}

#[test]
fn accepts_custom_configuration() {
    let user = UserManager::from_config(UserConfig {
        uid: Some(501),
        gid: Some(502),
        euid: Some(0),
        egid: Some(0),
        username: Some(String::from("admin")),
        homedir: Some(String::from("/home/admin")),
        shell: Some(String::from("/bin/bash")),
        gecos: Some(String::from("Admin User")),
        ..UserConfig::default()
    });

    assert_eq!(user.uid, 501);
    assert_eq!(user.gid, 502);
    assert_eq!(user.euid, 0);
    assert_eq!(user.egid, 0);
    assert_eq!(user.username, "admin");
    assert_eq!(user.homedir, "/home/admin");
    assert_eq!(user.shell, "/bin/bash");
    assert_eq!(user.gecos, "Admin User");
}

#[test]
fn supports_root_configuration() {
    let user = UserManager::from_config(UserConfig {
        uid: Some(0),
        gid: Some(0),
        username: Some(String::from("root")),
        homedir: Some(String::from("/root")),
        ..UserConfig::default()
    });

    assert_eq!(user.uid, 0);
    assert_eq!(user.gid, 0);
    assert_eq!(user.euid, 0);
    assert_eq!(user.egid, 0);
    assert_eq!(user.username, "root");
    assert_eq!(user.homedir, "/root");
}

#[test]
fn getpwuid_returns_configured_entry_for_the_active_user() {
    let user = UserManager::new();

    assert_eq!(
        user.getpwuid(1000),
        Some(String::from("user:x:1000:1000::/home/user:/bin/sh"))
    );

    let with_gecos = UserManager::from_config(UserConfig {
        gecos: Some(String::from("Test User")),
        ..UserConfig::default()
    });
    assert_eq!(
        with_gecos.getpwuid(1000),
        Some(String::from(
            "user:x:1000:1000:Test User:/home/user:/bin/sh"
        ))
    );
}

#[test]
fn getpwuid_returns_custom_entry_and_rejects_unknown_uids() {
    let deploy = UserManager::from_config(UserConfig {
        uid: Some(501),
        gid: Some(502),
        username: Some(String::from("deploy")),
        homedir: Some(String::from("/opt/deploy")),
        shell: Some(String::from("/bin/bash")),
        gecos: Some(String::from("Deploy User")),
        ..UserConfig::default()
    });

    assert_eq!(
        deploy.getpwuid(501),
        Some(String::from(
            "deploy:x:501:502:Deploy User:/opt/deploy:/bin/bash"
        ))
    );
    assert_eq!(deploy.getpwuid(9999), None);
}

#[test]
fn getpwuid_handles_root_uid_for_root_and_non_root_configs() {
    let user = UserManager::new();
    assert_eq!(user.getpwuid(0), None);

    let root = UserManager::from_config(UserConfig {
        uid: Some(0),
        gid: Some(0),
        username: Some(String::from("root")),
        homedir: Some(String::from("/root")),
        ..UserConfig::default()
    });
    assert_eq!(
        root.getpwuid(0),
        Some(String::from("root:x:0:0::/root:/bin/sh"))
    );
}

#[test]
fn getgroups_and_getgrgid_use_kernel_managed_group_state() {
    let user = UserManager::from_config(UserConfig {
        gid: Some(123),
        username: Some(String::from("deploy")),
        group_name: Some(String::from("deployers")),
        supplementary_gids: vec![456, 123, 789],
        ..UserConfig::default()
    });

    assert_eq!(user.getgroups(), vec![123, 456, 789]);
    assert_eq!(
        user.getgrgid(123),
        Some(String::from("deployers:x:123:deploy"))
    );
    assert_eq!(
        user.getgrgid(456),
        Some(String::from("group456:x:456:deploy"))
    );
    assert_eq!(user.getgrgid(999), None);
}
