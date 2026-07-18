use agentos_kernel::resource_accounting::ResourceLimits;
use agentos_kernel::system::SystemIdentity;
use agentos_kernel::user::UserManager;

use crate::{virtual_os_cpu_count, virtual_os_freemem_bytes, virtual_os_totalmem_bytes};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedGuestRuntimeIdentity {
    pub virtual_pid: Option<u64>,
    pub virtual_ppid: Option<u64>,
    pub virtual_uid: u64,
    pub virtual_gid: u64,
    pub process_platform: String,
    pub process_arch: String,
    pub os_cpu_count: u64,
    pub os_totalmem: u64,
    pub os_freemem: u64,
    pub os_homedir: String,
    pub os_hostname: String,
    pub os_shell: String,
    pub os_user: String,
    pub os_tmpdir: String,
    pub os_type: String,
    pub os_release: String,
    pub os_version: String,
    pub os_machine: String,
}

pub fn shared_guest_runtime_identity(
    user: &UserManager,
    resource_limits: &ResourceLimits,
    virtual_pid: Option<u64>,
    virtual_ppid: Option<u64>,
) -> SharedGuestRuntimeIdentity {
    shared_guest_runtime_identity_with_system(
        user,
        resource_limits,
        &SystemIdentity::default(),
        virtual_pid,
        virtual_ppid,
    )
}

pub fn shared_guest_runtime_identity_with_system(
    user: &UserManager,
    resource_limits: &ResourceLimits,
    system: &SystemIdentity,
    virtual_pid: Option<u64>,
    virtual_ppid: Option<u64>,
) -> SharedGuestRuntimeIdentity {
    SharedGuestRuntimeIdentity {
        virtual_pid,
        virtual_ppid,
        virtual_uid: u64::from(user.uid),
        virtual_gid: u64::from(user.gid),
        process_platform: String::from("linux"),
        process_arch: String::from("x64"),
        os_cpu_count: virtual_os_cpu_count(resource_limits) as u64,
        os_totalmem: virtual_os_totalmem_bytes(resource_limits),
        os_freemem: virtual_os_freemem_bytes(resource_limits),
        os_homedir: user.homedir.clone(),
        os_hostname: system.hostname.clone(),
        os_shell: user.shell.clone(),
        os_user: user.username.clone(),
        os_tmpdir: String::from("/tmp"),
        os_type: system.os_type.clone(),
        os_release: system.os_release.clone(),
        os_version: system.os_version.clone(),
        os_machine: system.machine.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_kernel::resource_accounting::ResourceLimits;
    use agentos_kernel::user::{UserConfig, UserManager};

    #[test]
    fn builds_guest_identity_from_kernel_user_and_limits() {
        let user = UserManager::from_config(UserConfig {
            uid: Some(501),
            gid: Some(20),
            username: Some(String::from("runner")),
            homedir: Some(String::from("/Users/runner")),
            shell: Some(String::from("/bin/zsh")),
            ..UserConfig::default()
        });
        let limits = ResourceLimits {
            virtual_cpu_count: Some(6),
            max_wasm_memory_bytes: Some(512 * 1024 * 1024),
            ..ResourceLimits::default()
        };

        let identity = shared_guest_runtime_identity(&user, &limits, Some(42), Some(1));

        assert_eq!(identity.virtual_pid, Some(42));
        assert_eq!(identity.virtual_ppid, Some(1));
        assert_eq!(identity.virtual_uid, 501);
        assert_eq!(identity.virtual_gid, 20);
        assert_eq!(identity.process_platform, "linux");
        assert_eq!(identity.process_arch, "x64");
        assert_eq!(identity.os_cpu_count, 6);
        assert_eq!(identity.os_totalmem, 512 * 1024 * 1024);
        assert_eq!(identity.os_homedir, "/Users/runner");
        assert_eq!(identity.os_hostname, "agentos");
        assert_eq!(identity.os_shell, "/bin/zsh");
        assert_eq!(identity.os_user, "runner");
        assert_eq!(identity.os_tmpdir, "/tmp");
        assert_eq!(identity.os_type, "Linux");
        assert_eq!(identity.os_release, "6.8.0-agentos");
        assert_eq!(identity.os_version, "#1 SMP PREEMPT_DYNAMIC agentos");
        assert_eq!(identity.os_machine, "x86_64");
    }

    #[test]
    fn injected_guest_identity_reuses_kernel_system_identity() {
        let user = UserManager::default();
        let limits = ResourceLimits::default();
        let system = SystemIdentity {
            hostname: String::from("vm-host"),
            os_type: String::from("TestOS"),
            os_release: String::from("1.2.3"),
            os_version: String::from("build-7"),
            machine: String::from("vm64"),
            domain_name: String::from("vm-domain"),
        };

        let identity =
            shared_guest_runtime_identity_with_system(&user, &limits, &system, None, None);

        assert_eq!(identity.os_hostname, "vm-host");
        assert_eq!(identity.os_type, "TestOS");
        assert_eq!(identity.os_release, "1.2.3");
        assert_eq!(identity.os_version, "build-7");
        assert_eq!(identity.os_machine, "vm64");
    }
}
