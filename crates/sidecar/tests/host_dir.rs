mod host_dir {
    include!("../src/plugins/host_dir.rs");

    mod tests {
        use super::{HostDirFilesystem, HostDirMountPlugin};
        use agent_os_kernel::mount_plugin::{FileSystemPluginFactory, OpenFileSystemPluginRequest};
        use agent_os_kernel::mount_table::MountedFileSystem;
        use agent_os_kernel::vfs::VirtualFileSystem;
        use serde_json::json;
        use std::fs;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn temp_dir(prefix: &str) -> PathBuf {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough for temp paths")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
            fs::create_dir_all(&path).expect("create temp dir");
            path
        }

        #[test]
        fn filesystem_rejects_symlink_escapes_and_round_trips_writes() {
            let host_dir = temp_dir("agent-os-host-dir-plugin");
            let outside_dir = temp_dir("agent-os-host-dir-plugin-outside");
            fs::write(host_dir.join("hello.txt"), "hello from host").expect("seed host file");
            std::os::unix::fs::symlink(&outside_dir, host_dir.join("escape"))
                .expect("seed escape symlink");

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            assert_eq!(
                filesystem
                    .read_text_file("/hello.txt")
                    .expect("read host file"),
                "hello from host"
            );

            filesystem
                .write_file("/nested/out.txt", b"written from vm".to_vec())
                .expect("write through host dir fs");
            assert_eq!(
                fs::read_to_string(host_dir.join("nested/out.txt"))
                    .expect("read written host file"),
                "written from vm"
            );

            let error = filesystem
                .read_file("/escape/hostname")
                .expect_err("escape symlink should fail closed");
            assert_eq!(error.code(), "EACCES");
            assert!(
                !outside_dir.join("hostname").exists(),
                "read should not materialize files outside the host mount"
            );

            let error = filesystem
                .write_file("/escape/owned.txt", b"owned".to_vec())
                .expect_err("escape symlink write should fail closed");
            assert_eq!(error.code(), "EACCES");
            assert!(
                !outside_dir.join("owned.txt").exists(),
                "write should not escape the mounted host directory"
            );

            fs::remove_dir_all(host_dir).expect("remove temp dir");
            fs::remove_dir_all(outside_dir).expect("remove outside temp dir");
        }

        #[test]
        fn filesystem_pwrite_updates_in_place_and_zero_fills_gaps() {
            let host_dir = temp_dir("agent-os-host-dir-plugin-pwrite");
            fs::write(host_dir.join("data.txt"), b"abcdef").expect("seed host file");

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            filesystem
                .pwrite("/data.txt", b"XYZ".to_vec(), 2)
                .expect("overwrite bytes in place");
            filesystem
                .pwrite("/data.txt", b"!".to_vec(), 8)
                .expect("extend file with zero-filled hole");

            assert_eq!(
                fs::read(host_dir.join("data.txt")).expect("read written host file"),
                b"abXYZf\0\0!".to_vec()
            );

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }

        #[test]
        fn filesystem_pwrite_rejects_symlink_escape_targets() {
            let host_dir = temp_dir("agent-os-host-dir-plugin-pwrite-escape");
            let outside_dir = temp_dir("agent-os-host-dir-plugin-pwrite-escape-outside");
            fs::write(outside_dir.join("outside.txt"), b"outside").expect("seed outside file");
            std::os::unix::fs::symlink(&outside_dir, host_dir.join("escape"))
                .expect("seed escape symlink");

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            let error = filesystem
                .pwrite("/escape/outside.txt", b"owned".to_vec(), 0)
                .expect_err("pwrite should reject symlink escapes");
            assert_eq!(error.code(), "EACCES");
            assert_eq!(
                fs::read(outside_dir.join("outside.txt")).expect("outside file should stay intact"),
                b"outside".to_vec()
            );

            fs::remove_dir_all(host_dir).expect("remove temp dir");
            fs::remove_dir_all(outside_dir).expect("remove outside temp dir");
        }

        #[test]
        fn plugin_config_can_enforce_read_only_mounts() {
            let host_dir = temp_dir("agent-os-host-dir-plugin-readonly");
            fs::write(host_dir.join("hello.txt"), "hello from host").expect("seed host file");

            let plugin = HostDirMountPlugin;
            let mut mounted = plugin
                .open(OpenFileSystemPluginRequest {
                    vm_id: "vm-1",
                    guest_path: "/workspace",
                    read_only: false,
                    config: &json!({
                        "hostPath": host_dir,
                        "readOnly": true,
                    }),
                    context: &(),
                })
                .expect("open host_dir plugin");

            assert_eq!(
                mounted.read_file("/hello.txt").expect("read host file"),
                b"hello from host".to_vec()
            );
            let error = mounted
                .write_file("/blocked.txt", b"blocked".to_vec())
                .expect_err("readonly plugin config should reject writes");
            assert_eq!(error.code(), "EROFS");

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }
    }
}
