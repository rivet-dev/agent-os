use agentos_kernel::process_table::{ProcessInfo, ProcessStatus};
use agentos_sidecar_protocol::protocol::{ProcessSnapshotEntry, ProcessSnapshotStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedProcessSnapshotStatus {
    Running,
    Stopped,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedProcessSnapshotEntry {
    pub process_id: String,
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub driver: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub status: SharedProcessSnapshotStatus,
    pub exit_code: Option<i32>,
}

pub fn process_status_from_kernel(status: ProcessStatus) -> SharedProcessSnapshotStatus {
    match status {
        ProcessStatus::Running => SharedProcessSnapshotStatus::Running,
        ProcessStatus::Stopped => SharedProcessSnapshotStatus::Stopped,
        ProcessStatus::Exited => SharedProcessSnapshotStatus::Exited,
    }
}

pub fn process_snapshot_entry_from_kernel(
    process_id: &str,
    info: &ProcessInfo,
    cwd: impl Into<String>,
    exit_code: Option<i32>,
) -> SharedProcessSnapshotEntry {
    SharedProcessSnapshotEntry {
        process_id: process_id.to_owned(),
        pid: info.pid,
        ppid: info.ppid,
        pgid: info.pgid,
        sid: info.sid,
        driver: info.driver.clone(),
        command: info.command.clone(),
        args: Vec::new(),
        cwd: cwd.into(),
        status: if exit_code.is_some() {
            SharedProcessSnapshotStatus::Exited
        } else {
            process_status_from_kernel(info.status)
        },
        exit_code: exit_code.or(info.exit_code),
    }
}

pub fn protocol_process_snapshot_entry(entry: SharedProcessSnapshotEntry) -> ProcessSnapshotEntry {
    ProcessSnapshotEntry {
        process_id: entry.process_id,
        pid: entry.pid,
        ppid: entry.ppid,
        pgid: entry.pgid,
        sid: entry.sid,
        driver: entry.driver,
        command: entry.command,
        args: entry.args,
        cwd: entry.cwd,
        status: match entry.status {
            SharedProcessSnapshotStatus::Running => ProcessSnapshotStatus::Running,
            SharedProcessSnapshotStatus::Stopped => ProcessSnapshotStatus::Stopped,
            SharedProcessSnapshotStatus::Exited => ProcessSnapshotStatus::Exited,
        },
        exit_code: entry.exit_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_kernel::user::ProcessIdentity;

    fn process_info(status: ProcessStatus, exit_code: Option<i32>) -> ProcessInfo {
        ProcessInfo {
            pid: 42,
            ppid: 1,
            pgid: 42,
            sid: 42,
            driver: "javascript".to_owned(),
            command: "node".to_owned(),
            status,
            exit_code,
            pending_termination: None,
            termination: None,
            runtime_fault: None,
            identity: ProcessIdentity::default(),
        }
    }

    #[test]
    fn maps_kernel_process_statuses() {
        assert_eq!(
            process_status_from_kernel(ProcessStatus::Running),
            SharedProcessSnapshotStatus::Running
        );
        assert_eq!(
            process_status_from_kernel(ProcessStatus::Stopped),
            SharedProcessSnapshotStatus::Stopped
        );
        assert_eq!(
            process_status_from_kernel(ProcessStatus::Exited),
            SharedProcessSnapshotStatus::Exited
        );
    }

    #[test]
    fn builds_process_snapshot_from_kernel_info() {
        let entry = process_snapshot_entry_from_kernel(
            "exec-1",
            &process_info(ProcessStatus::Running, None),
            "/workspace",
            None,
        );

        assert_eq!(entry.process_id, "exec-1");
        assert_eq!(entry.pid, 42);
        assert_eq!(entry.ppid, 1);
        assert_eq!(entry.pgid, 42);
        assert_eq!(entry.sid, 42);
        assert_eq!(entry.driver, "javascript");
        assert_eq!(entry.command, "node");
        assert_eq!(entry.args, Vec::<String>::new());
        assert_eq!(entry.cwd, "/workspace");
        assert_eq!(entry.status, SharedProcessSnapshotStatus::Running);
        assert_eq!(entry.exit_code, None);
    }

    #[test]
    fn explicit_exit_code_marks_snapshot_exited() {
        let entry = process_snapshot_entry_from_kernel(
            "exec-1",
            &process_info(ProcessStatus::Running, Some(9)),
            "/workspace",
            Some(7),
        );

        assert_eq!(entry.status, SharedProcessSnapshotStatus::Exited);
        assert_eq!(entry.exit_code, Some(7));
    }

    #[test]
    fn maps_shared_process_snapshot_to_protocol_entry() {
        let entry = protocol_process_snapshot_entry(SharedProcessSnapshotEntry {
            process_id: String::from("proc-1"),
            pid: 42,
            ppid: 1,
            pgid: 42,
            sid: 42,
            driver: String::from("javascript"),
            command: String::from("node"),
            args: vec![String::from("app.js")],
            cwd: String::from("/workspace"),
            status: SharedProcessSnapshotStatus::Stopped,
            exit_code: None,
        });

        assert_eq!(entry.process_id, "proc-1");
        assert_eq!(entry.pid, 42);
        assert_eq!(entry.args, vec![String::from("app.js")]);
        assert_eq!(entry.cwd, "/workspace");
        assert_eq!(entry.status, ProcessSnapshotStatus::Stopped);
        assert_eq!(entry.exit_code, None);
    }
}
